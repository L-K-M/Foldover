//! Simulation (PLAN.md §5.2, ADR 0038): random edits to both folders
//! between runs; runs cancelled partway, killed at a base commit, or cut
//! off by a folder that starts failing every write; and after every run
//! these invariants:
//!
//! - nothing is lost: a file version a run replaced or removed is in its
//!   folder's archive, or at another path there when the run moved it, except on a Backup target that keeps no records,
//!   which never gains an archive;
//! - a finished run leaves both folders equal outside the conflicts it
//!   reported, comparing names the way the folders do; a Backup leaves
//!   every source file on its target;
//! - a one-way run never writes its source;
//! - on in-memory folders, a run right after a finished one writes
//!   nothing, so no quirk feeds a copy loop.
//!
//! Folders are real temporary directories, through the production local
//! endpoint, staging and archive; or in-memory folders with file system
//! quirks, fast enough for the nightly gate. A failing case replays from
//! proptest's seed. `FOLDOVER_SIMULATION_CASES` and
//! `FOLDOVER_MEMORY_SIMULATION_CASES` scale the nightly run, and
//! `FOLDOVER_SIMULATION_LEFT_VOLUME` and `…_RIGHT_VOLUME` put the real
//! folders on chosen volumes, as the nightly device matrix does with disk
//! images of each file system.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::Duration;

use foldover_sim::{MemoryEndpoint, NameStorage, Quirks, Replace};
use foldover_types::{CaseMatching, FolderRecords, NameMatching, PairRoot, UnicodeMatching};
use proptest::prelude::*;
use unicode_normalization::UnicodeNormalization;

use super::*;

/// Cases per test run; CI keeps them short, the nightly run raises them.
const DISK_CASES: u32 = 8;
const MEMORY_CASES: u32 = 64;

fn cases(variable: &str, default: u32) -> u32 {
    std::env::var(variable)
        .ok()
        .and_then(|cases| cases.parse().ok())
        .unwrap_or(default)
}

/// Where real folders live instead of temporary directories.
const LEFT_VOLUME: &str = "FOLDOVER_SIMULATION_LEFT_VOLUME";
const RIGHT_VOLUME: &str = "FOLDOVER_SIMULATION_RIGHT_VOLUME";

/// The paths edits touch: nested, but never a file where another path
/// needs a folder, with names only some folders tell apart: letter case,
/// and a composed and a decomposed `é`.
const PATHS: &[&str] = &[
    "a.txt",
    "A.txt",
    "b.txt",
    "docs/c.txt",
    "docs/C.txt",
    "docs/caf\u{e9}.txt",
    "docs/cafe\u{301}.txt",
    "docs/deep/e.txt",
    "photos/f.jpg",
    "photos/2026/h.jpg",
];

/// Leases quick enough that one a failed run could not release is taken
/// over at once.
const MEMORY_LEASES: LeaseTiming = LeaseTiming {
    ttl: Duration::from_millis(5),
    beat: Duration::from_millis(1),
    poll: Duration::from_millis(1),
    wait: Duration::from_secs(1),
};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Folder {
    Left,
    Right,
}

#[derive(Clone, Debug)]
enum Edit {
    /// Create or overwrite with new content.
    Write(Folder, usize),
    Delete(Folder, usize),
    Rename(Folder, usize, usize),
}

/// How a run ends before it finishes on its own.
#[derive(Clone, Copy, Debug)]
enum Interruption {
    None,
    /// The person cancels after this many checkpoints.
    Cancel(u8),
    /// The agent dies at this base commit, its writes done but unrecorded.
    Crash(u8),
    /// From this write step on, every write to the folder fails, as when
    /// a disk goes away mid-run. In-memory folders only.
    Fault(Folder, u8),
}

/// Whether a strategy may cut runs off with failing writes.
#[derive(Clone, Copy)]
enum Faults {
    Off,
    On,
}

/// How a simulated run ended.
enum Ended {
    Finished(RunReport),
    /// A gate wants a person's review, as for an emptied folder.
    NeedsReview,
    Interrupted,
}

#[derive(Clone, Debug)]
struct Round {
    edits: Vec<Edit>,
    interruption: Interruption,
}

fn folder() -> impl Strategy<Value = Folder> {
    prop_oneof![Just(Folder::Left), Just(Folder::Right)]
}

fn edit(paths: usize) -> impl Strategy<Value = Edit> {
    let path = 0..paths;
    prop_oneof![
        3 => (folder(), path.clone()).prop_map(|(folder, path)| Edit::Write(folder, path)),
        2 => (folder(), path.clone()).prop_map(|(folder, path)| Edit::Delete(folder, path)),
        1 => (folder(), path.clone(), path)
            .prop_map(|(folder, from, to)| Edit::Rename(folder, from, to)),
    ]
}

fn interruption(faults: Faults) -> BoxedStrategy<Interruption> {
    let common = prop_oneof![
        8 => Just(Interruption::None),
        2 => (0u8..12).prop_map(Interruption::Cancel),
        2 => (0u8..3).prop_map(Interruption::Crash),
    ];
    match faults {
        Faults::Off => common.boxed(),
        Faults::On => prop_oneof![
            6 => common,
            1 => (folder(), 0u8..16).prop_map(|(folder, after)| Interruption::Fault(folder, after)),
        ]
        .boxed(),
    }
}

fn rounds(paths: usize, faults: Faults) -> impl Strategy<Value = Vec<Round>> {
    let round = (
        prop::collection::vec(edit(paths), 0..6),
        interruption(faults),
    )
        .prop_map(|(edits, interruption)| Round {
            edits,
            interruption,
        });
    prop::collection::vec(round, 1..8)
}

/// How the pair syncs; most histories two-way. Half the Backups keep
/// no records in their target (PLAN.md §3.6).
fn mode() -> impl Strategy<Value = (SyncMode, FolderRecords)> {
    let side = prop_oneof![Just(Side::Left), Just(Side::Right)];
    let records = prop_oneof![Just(FolderRecords::Both), Just(FolderRecords::SourceOnly)];
    prop_oneof![
        2 => Just((SyncMode::Sync, FolderRecords::Both)),
        1 => side
            .clone()
            .prop_map(|source| (SyncMode::Mirror { source }, FolderRecords::Both)),
        1 => (side, records).prop_map(|(source, records)| (SyncMode::Backup { source }, records)),
    ]
}

/// One folder's file system, drawn from those Foldover meets.
fn quirks() -> impl Strategy<Value = Quirks> {
    let names = prop_oneof![
        Just((NameMatching::EXACT, NameStorage::AsGiven)),
        // APFS's default: case ignored, case and normalization kept.
        Just((
            NameMatching::new(CaseMatching::Insensitive, UnicodeMatching::Normalized),
            NameStorage::AsGiven,
        )),
        // HFS+: case ignored, names stored decomposed.
        Just((
            NameMatching::new(CaseMatching::Insensitive, UnicodeMatching::Normalized),
            NameStorage::Decomposed,
        )),
        // An SFTP server: case kept, names written composed and found in
        // any spelling (PLAN.md §4.10).
        Just((
            NameMatching::new(CaseMatching::Exact, UnicodeMatching::Normalized),
            NameStorage::Composed,
        )),
    ];
    let times = prop_oneof![Just(Duration::ZERO), Just(Duration::from_secs(2))];
    let replace = prop_oneof![Just(Replace::Atomic), Just(Replace::TwoStep)];
    (names, times, replace).prop_map(|((names, storage), time_resolution, replace)| Quirks {
        names,
        storage,
        time_resolution,
        replace,
        capacity: None,
    })
}

/// Every file below `root`, outside Foldover's own folder, by path.
/// AppleDouble files are macOS's storage for attributes on volumes
/// without native ones (exFAT, FAT), not files a person wrote.
fn disk_files(root: &Path) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_name() == ".foldover"
                || entry.file_name().as_encoded_bytes().starts_with(b"._")
            {
                continue;
            }
            if entry.file_type().unwrap().is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(root).unwrap();
                found.insert(
                    relative.as_os_str().as_encoded_bytes().to_vec(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }
    found
}

/// A name the way `names` compares it, like the engine's name keys.
fn folded(path: &[u8], names: NameMatching) -> Vec<u8> {
    let text = String::from_utf8_lossy(path);
    let text: String = match names.unicode() {
        UnicodeMatching::Normalized => text.nfc().collect(),
        UnicodeMatching::Exact => text.into_owned(),
    };
    match names.case() {
        CaseMatching::Insensitive => text.to_lowercase().into_bytes(),
        CaseMatching::Exact => text.into_bytes(),
    }
}

/// A folder's files by folded name: each spelling with its content.
type Folded = BTreeMap<Vec<u8>, Vec<(Vec<u8>, Vec<u8>)>>;

/// Whether written files carry an extended attribute. On volumes without
/// native ones, such as exFAT and FAT, macOS keeps it in a `._` sidecar,
/// which the device matrix wants to meet.
#[derive(Clone, Copy, PartialEq)]
enum Attributes {
    None,
    Tagged,
}

struct Simulation {
    pair: Fixture,
    attributes: Attributes,
    mode: SyncMode,
    /// A Backup's source files after the last finished run, by folded
    /// name: what the target was last brought up to.
    backed_up: BTreeMap<Vec<u8>, Vec<u8>>,
    paths: &'static [&'static str],
    /// How the pair compares names: as its stricter folder does.
    names: NameMatching,
    /// Each write gets new content, so the archive check is exact.
    writes: u32,
    hour: u32,
}

impl Simulation {
    fn on_disk() -> Self {
        let mut pair = fixture(SyncMode::Sync);
        let placed = |variable| {
            std::env::var_os(variable).map(|volume| tempfile::tempdir_in(volume).unwrap())
        };
        if let Some(left) = placed(LEFT_VOLUME) {
            pair.left = left;
        }
        if let Some(right) = placed(RIGHT_VOLUME) {
            pair.right = right;
        }
        // Compare names the way the folders' file systems do.
        let names = |path: &Path| {
            Endpoint::open(&PairRoot::Local(root(path)), &Reach::default())
                .unwrap()
                .name_matching()
        };
        let names = names(pair.left.path()).either(names(pair.right.path()));
        let chosen = [LEFT_VOLUME, RIGHT_VOLUME]
            .iter()
            .any(|variable| std::env::var_os(variable).is_some());
        let attributes = if chosen && cfg!(target_os = "macos") {
            Attributes::Tagged
        } else {
            Attributes::None
        };
        Self {
            pair,
            attributes,
            mode: SyncMode::Sync,
            backed_up: BTreeMap::new(),
            paths: PATHS,
            names,
            writes: 0,
            hour: 0,
        }
    }

    fn in_memory(left: Quirks, right: Quirks) -> Self {
        Self::in_memory_as((SyncMode::Sync, FolderRecords::Both), left, right)
    }

    fn in_memory_as(
        (mode, records): (SyncMode, FolderRecords),
        left: Quirks,
        right: Quirks,
    ) -> Self {
        let mut pair = fixture(mode);
        pair.records = records;
        pair.memory = Some([MemoryEndpoint::new(left), MemoryEndpoint::new(right)]);
        pair.leases = MEMORY_LEASES;
        Self {
            pair,
            attributes: Attributes::None,
            mode,
            backed_up: BTreeMap::new(),
            paths: PATHS,
            names: left.names.either(right.names),
            writes: 0,
            hour: 0,
        }
    }

    fn memory(&self, folder: Folder) -> Option<&MemoryEndpoint> {
        let folders = self.pair.memory.as_ref()?;
        Some(match folder {
            Folder::Left => &folders[0],
            Folder::Right => &folders[1],
        })
    }

    fn root(&self, folder: Folder) -> std::path::PathBuf {
        match folder {
            Folder::Left => self.pair.left.path().to_owned(),
            Folder::Right => self.pair.right.path().to_owned(),
        }
    }

    fn apply(&mut self, edit: &Edit) {
        match *edit {
            Edit::Write(folder, path) => {
                self.writes += 1;
                let content = format!("content {}", self.writes);
                let path = self.paths[path];
                if let Some(memory) = self.memory(folder) {
                    memory
                        .put_file(path.as_bytes(), content.as_bytes())
                        .unwrap();
                    return;
                }
                let path = self.root(folder).join(path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, &content).unwrap();
                // macOS's FAT driver cannot rename a file with an accented
                // name while a sidecar holds its attributes, whoever asks
                // (ADR 0019); those names stay untagged.
                if self.attributes == Attributes::Tagged
                    && path
                        .file_name()
                        .is_some_and(|name| name.as_encoded_bytes().is_ascii())
                {
                    let tagged = std::process::Command::new("xattr")
                        .args(["-w", "ch.lkmc.foldover.simulation", &content])
                        .arg(&path)
                        .status()
                        .unwrap();
                    assert!(tagged.success());
                }
            }
            Edit::Delete(folder, path) => {
                let path = self.paths[path];
                if let Some(memory) = self.memory(folder) {
                    memory.delete_file(path.as_bytes());
                    return;
                }
                let _ = fs::remove_file(self.root(folder).join(path));
            }
            Edit::Rename(folder, from, to) => {
                let (from, to) = (self.paths[from], self.paths[to]);
                if let Some(memory) = self.memory(folder) {
                    memory.move_file(from.as_bytes(), to.as_bytes()).unwrap();
                    return;
                }
                let root = self.root(folder);
                let (from_path, to_path) = (root.join(from), root.join(to));
                if from_path.exists() && !to_path.exists() {
                    fs::create_dir_all(to_path.parent().unwrap()).unwrap();
                    // macOS's FAT driver renames only by the spelling a file
                    // was created with, as Foldover does: try both.
                    if let Err(first) = fs::rename(&from_path, &to_path) {
                        assert_eq!(first.kind(), std::io::ErrorKind::NotFound, "{from} -> {to}");
                        let composed = |path: &str| root.join(path.nfc().collect::<String>());
                        let decomposed = |path: &str| root.join(path.nfd().collect::<String>());
                        let renamed = fs::rename(composed(from), composed(to))
                            .or_else(|_| fs::rename(decomposed(from), decomposed(to)));
                        if let Err(error) = renamed {
                            let listed = |path: &Path| -> Vec<Vec<u8>> {
                                fs::read_dir(path.parent().unwrap())
                                    .map(|entries| {
                                        entries
                                            .flatten()
                                            .map(|entry| {
                                                entry.file_name().as_encoded_bytes().to_vec()
                                            })
                                            .collect()
                                    })
                                    .unwrap_or_default()
                            };
                            panic!(
                                "{from:?} -> {to:?}: {first}, then composed and decomposed: {error}; from's folder {:?}, to's folder {:?}",
                                listed(&from_path),
                                listed(&to_path)
                            );
                        }
                    }
                }
            }
        }
    }

    /// Everything in a folder on disk, Foldover's own files included, one
    /// line each, to show what a failed check saw.
    fn tree(&self, folder: Folder) -> String {
        if self.memory(folder).is_some() {
            return "(in memory)".into();
        }
        let root = self.root(folder);
        let mut lines = Vec::new();
        let mut pending = vec![root.clone()];
        while let Some(directory) = pending.pop() {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let shown = path
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    lines.push(format!("  {shown}/"));
                    pending.push(path);
                } else {
                    let content = fs::read(&path).unwrap_or_default();
                    lines.push(format!("  {shown}: {}", String::from_utf8_lossy(&content)));
                }
            }
        }
        lines.sort();
        lines.join("\n")
    }

    fn files(&self, folder: Folder) -> BTreeMap<Vec<u8>, Vec<u8>> {
        match self.memory(folder) {
            Some(memory) => memory.user_files(),
            None => disk_files(&self.root(folder)),
        }
    }

    /// Every file's content in the folder's archive, from any run, and in
    /// temporaries an interrupted write left: those may hold a displaced
    /// original, which the next run archives.
    fn kept(&self, folder: Folder) -> Vec<Vec<u8>> {
        if let Some(memory) = self.memory(folder) {
            let mut kept = memory.archived_files();
            kept.extend(memory.leftover_files());
            return kept;
        }
        let archive = self.root(folder).join(".foldover/archive");
        let leftovers = disk_files(&self.root(folder))
            .into_iter()
            .filter(|(path, _)| {
                path.rsplit(|byte| *byte == b'/')
                    .next()
                    .is_some_and(|name| name.starts_with(b".foldover-tmp-"))
            })
            .map(|(_, content)| content);
        let archived = archive
            .exists()
            .then(|| disk_files(&archive).into_values())
            .into_iter()
            .flatten();
        archived.chain(leftovers).collect()
    }

    /// One run, interrupted as asked. `Reviewed` stands for a person who
    /// saw the preview and approved it.
    fn run(&mut self, interruption: Interruption, context: RunContext) -> Ended {
        self.hour += 1;
        let context = if self.pair.identities.side(Side::Left).is_none() {
            RunContext::Reviewed
        } else {
            context
        };
        // Both folders are always there; a scan that fails is a bug.
        let prepared = self.pair.try_prepare().unwrap();
        if let Interruption::Fault(folder, after) = interruption {
            self.memory(folder)
                .expect("faults are injected into in-memory folders")
                .inject_fault(after.into());
        }
        let mut store = self.pair.store();
        let (mut checks, mut commits) = (0u8, 0u8);
        let outcome = prepared.execute(
            Approval::new(u64::MAX, u64::MAX),
            context,
            at(self.hour),
            &mut |planned, commit| {
                if matches!(commit, Commit::Base { .. }) {
                    if matches!(interruption, Interruption::Crash(at) if at == commits) {
                        return Err(crate::Error::StaleRevision);
                    }
                    commits += 1;
                }
                store.apply(planned, commit)
            },
            &mut || {
                checks = checks.saturating_add(1);
                match interruption {
                    Interruption::Cancel(at) if checks > at => ScanControl::Cancel,
                    _ => ScanControl::Continue,
                }
            },
            &NoProgress,
        );
        for folder in [Folder::Left, Folder::Right] {
            if let Some(memory) = self.memory(folder) {
                memory.clear_fault();
            }
        }
        match (outcome, interruption) {
            (Ok(RunOutcome::Ran(report)), _) => Ended::Finished(report),
            (Ok(RunOutcome::NeedsApproval(_)), _) => Ended::NeedsReview,
            // Only an interruption may end a run in an error.
            (Err(error), Interruption::None) => panic!("an uninterrupted run failed: {error}"),
            (Err(_), _) => Ended::Interrupted,
        }
    }

    /// Nothing that left a folder during a run is missing from what it
    /// keeps. A Backup target without records keeps nothing: what a run
    /// replaces there goes by design, and no archive appears.
    fn check_nothing_lost(&self, before: &[BTreeMap<Vec<u8>, Vec<u8>>; 2]) {
        for (folder, before) in [Folder::Left, Folder::Right].iter().zip(before) {
            if self.unrecorded() == Some(*folder) {
                if let Some(memory) = self.memory(*folder) {
                    assert!(
                        memory.archived_files().is_empty(),
                        "{folder:?} keeps no records but gained an archive"
                    );
                }
                continue;
            }
            let (after, archive) = (self.files(*folder), self.kept(*folder));
            // Every write has its own content, so the same content at
            // another path is this version, moved there (PLAN.md §4.7).
            let kept_here: BTreeSet<&Vec<u8>> = after.values().collect();
            for (path, content) in before {
                if after.get(path) == Some(content) || kept_here.contains(content) {
                    continue;
                }
                assert!(
                    archive.contains(content),
                    "{folder:?} {} ({}) left without an archived copy; the folder holds:\n{}",
                    String::from_utf8_lossy(path),
                    String::from_utf8_lossy(content),
                    self.tree(*folder)
                );
            }
        }
    }

    /// The Backup target that keeps no records, if the pair has one.
    fn unrecorded(&self) -> Option<Folder> {
        if self.pair.records != FolderRecords::SourceOnly
            || !matches!(self.mode, SyncMode::Backup { .. })
        {
            return None;
        }
        self.source().map(|source| match source {
            Folder::Left => Folder::Right,
            Folder::Right => Folder::Left,
        })
    }

    /// The folder a one-way mode copies from.
    fn source(&self) -> Option<Folder> {
        match self.mode {
            SyncMode::Sync => None,
            SyncMode::Mirror { source } | SyncMode::Backup { source } => Some(match source {
                Side::Left => Folder::Left,
                Side::Right => Folder::Right,
            }),
        }
    }

    /// A one-way run never writes its source.
    fn check_source_untouched(&self, before: &[BTreeMap<Vec<u8>, Vec<u8>>; 2]) {
        let Some(source) = self.source() else {
            return;
        };
        let before = match source {
            Folder::Left => &before[0],
            Folder::Right => &before[1],
        };
        assert_eq!(
            &self.files(source),
            before,
            "a one-way run wrote its source"
        );
    }

    /// Both folders match except where the run reported a conflict, with
    /// names compared as the folders compare them. A Backup's target holds
    /// every source file, with the source's content where the source
    /// changed since the last finished run, and loses nothing it held
    /// before this round's runs (`before`): it keeps its own edits and
    /// extras.
    fn check_converged(&mut self, report: &RunReport, before: &[BTreeMap<Vec<u8>, Vec<u8>>; 2]) {
        let conflicted: Vec<Vec<u8>> = report
            .conflicts()
            .iter()
            .map(|(path, _)| folded(path.as_bytes(), self.names))
            .collect();
        let affected = |key: &[u8]| {
            conflicted.iter().any(|conflict| {
                key == conflict.as_slice()
                    || (key.starts_with(conflict) && key.get(conflict.len()) == Some(&b'/'))
            })
        };
        let by_key = |folder| {
            let mut keyed: Folded = BTreeMap::new();
            for (path, content) in self.files(folder) {
                keyed
                    .entry(folded(&path, self.names))
                    .or_default()
                    .push((path, content));
            }
            keyed
        };
        let (left, right) = (by_key(Folder::Left), by_key(Folder::Right));
        if let SyncMode::Backup { source } = self.mode {
            let (source, target, target_before) = match source {
                Side::Left => (&left, &right, &before[1]),
                Side::Right => (&right, &left, &before[0]),
            };
            // A byte-exact target may hold several spellings of a name the
            // source folds together; any of them may be the copy.
            let copies = |key: &[u8]| -> Vec<Vec<u8>> {
                target
                    .get(key)
                    .map(|copies| copies.iter().map(|(_, content)| content.clone()).collect())
                    .unwrap_or_default()
            };
            for key in source.keys().filter(|key| !affected(key)) {
                let copied = copies(key);
                assert!(
                    !copied.is_empty(),
                    "{} is missing from the backup",
                    String::from_utf8_lossy(key)
                );
                let current = source[key].first().map(|(_, content)| content.clone());
                if let Some(current) = current
                    && self.backed_up.get(key) != Some(&current)
                {
                    assert!(
                        copied.contains(&current),
                        "{} changed in the source but not in the backup",
                        String::from_utf8_lossy(key)
                    );
                }
            }
            let held: BTreeSet<Vec<u8>> = target_before
                .keys()
                .map(|path| folded(path, self.names))
                .collect();
            for key in &held {
                assert!(
                    affected(key) || target.contains_key(key),
                    "the backup lost {}",
                    String::from_utf8_lossy(key)
                );
            }
            // Review: nor does it make up names the source never had.
            for key in target.keys().filter(|key| !affected(key)) {
                assert!(
                    source.contains_key(key) || held.contains(key),
                    "the backup gained {}",
                    String::from_utf8_lossy(key)
                );
            }
            // What the run brought the target up to. A name in conflict, or
            // spelled several ways in the source, was not brought up, and
            // one the source lost keeps its last content, as the base does:
            // it comes back unchanged if the source gets it back.
            for (key, copies) in source {
                if let [(_, content)] = copies.as_slice()
                    && !affected(key)
                {
                    self.backed_up.insert(key.clone(), content.clone());
                }
            }
            return;
        }
        for key in left.keys().chain(right.keys()) {
            if affected(key) {
                continue;
            }
            let side = |files: &Folded| {
                let found = files.get(key).map(Vec::as_slice).unwrap_or_default();
                assert!(
                    found.len() < 2,
                    "names that fold together without a reported conflict: {:?}",
                    found
                        .iter()
                        .map(|(path, _)| String::from_utf8_lossy(path).into_owned())
                        .collect::<Vec<_>>()
                );
                found
                    .first()
                    .map(|(_, content)| String::from_utf8_lossy(content).into_owned())
            };
            assert_eq!(
                side(&left),
                side(&right),
                "{} differs after a finished run",
                String::from_utf8_lossy(key)
            );
        }
    }

    /// A run right after a finished one finds nothing to write.
    fn check_settled(&mut self) {
        let Ended::Finished(report) = self.run(Interruption::None, RunContext::Interactive) else {
            panic!("a run after a finished one did not finish");
        };
        assert!(report.failures().is_empty(), "{:?}", report.failures());
        assert_eq!(
            (
                report.copied(),
                report.updated(),
                report.archived(),
                report.recovered()
            ),
            (0, 0, 0, 0),
            "a run after a finished one wrote"
        );
    }

    fn snapshot(&self) -> [BTreeMap<Vec<u8>, Vec<u8>>; 2] {
        [self.files(Folder::Left), self.files(Folder::Right)]
    }

    fn play(&mut self, round: &Round) {
        for edit in &round.edits {
            self.apply(edit);
        }
        let before = self.snapshot();
        let mut ended = self.run(round.interruption, RunContext::Interactive);
        self.check_nothing_lost(&before);
        self.check_source_untouched(&before);
        // An interrupted run is followed by one that finishes, and a run a
        // gate held back by one a person reviewed.
        if !matches!(round.interruption, Interruption::None) {
            ended = self.run(Interruption::None, RunContext::Interactive);
            self.check_nothing_lost(&before);
            self.check_source_untouched(&before);
        }
        if matches!(ended, Ended::NeedsReview) {
            ended = self.run(Interruption::None, RunContext::Reviewed);
            self.check_nothing_lost(&before);
            self.check_source_untouched(&before);
        }
        match ended {
            Ended::Finished(report)
                if report.completion() == Completion::Finished && report.failures().is_empty() =>
            {
                self.check_converged(&report, &before);
                // What interrupted writes left is archived by now.
                for folder in [Folder::Left, Folder::Right] {
                    if let Some(memory) = self.memory(folder) {
                        assert!(
                            memory.leftover_files().is_empty(),
                            "{folder:?} kept leftovers after a finished run"
                        );
                    }
                }
            }
            Ended::Finished(report) => panic!("a finished run reported {:?}", report.failures()),
            Ended::NeedsReview => panic!("a reviewed run still needed review"),
            Ended::Interrupted => unreachable!("only interrupted runs end interrupted"),
        }
        if self.pair.memory.is_some() {
            self.check_settled();
            self.check_nothing_lost(&before);
            self.check_source_untouched(&before);
        }
    }

    /// Some content to start from, in the source if there is one, synced
    /// once.
    fn start(&mut self) {
        let folder = self.source().unwrap_or(Folder::Left);
        for path in 0..3 {
            self.apply(&Edit::Write(folder, path));
        }
        self.play(&Round {
            edits: Vec::new(),
            interruption: Interruption::None,
        });
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases("FOLDOVER_SIMULATION_CASES", DISK_CASES)))]

    #[test]
    fn random_edits_and_interrupted_runs_lose_nothing_and_converge(
        rounds in rounds(PATHS.len(), Faults::Off),
    ) {
        let mut simulation = Simulation::on_disk();
        simulation.start();
        for round in &rounds {
            simulation.play(round);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(
        cases("FOLDOVER_MEMORY_SIMULATION_CASES", MEMORY_CASES),
    ))]

    #[test]
    fn quirky_folders_lose_nothing_converge_and_settle(
        mode in mode(),
        left in quirks(),
        right in quirks(),
        rounds in rounds(PATHS.len(), Faults::On),
    ) {
        let mut simulation = Simulation::in_memory_as(mode, left, right);
        simulation.start();
        for round in &rounds {
            simulation.play(round);
        }
    }
}

/// A folder without room for a run and the free-space reserve stops it
/// before anything is written; once space is freed, the pair catches up.
#[test]
fn a_full_folder_stops_the_run_until_space_is_freed() {
    let mut simulation = Simulation::in_memory(
        Quirks::default(),
        Quirks {
            capacity: Some(40),
            ..Quirks::default()
        },
    );
    for path in 0..5 {
        simulation.apply(&Edit::Write(Folder::Left, path));
    }
    let before = simulation.snapshot();
    let prepared = simulation.pair.try_prepare().unwrap();
    let mut store = simulation.pair.store();
    let outcome = prepared.execute(
        Approval::new(u64::MAX, u64::MAX),
        RunContext::Reviewed,
        at(1),
        &mut |planned, commit| store.apply(planned, commit),
        &mut proceed,
        &NoProgress,
    );
    assert!(
        matches!(
            outcome,
            Err(RunError::InsufficientSpace {
                side: Side::Right,
                ..
            })
        ),
        "{outcome:?}"
    );
    assert_eq!(simulation.snapshot(), before);

    simulation.memory(Folder::Right).unwrap().set_capacity(None);
    simulation.hour = 1;
    simulation.play(&Round {
        edits: Vec::new(),
        interruption: Interruption::None,
    });
}

/// Simulation: a folder that stores names decomposed, as HFS+ does,
/// respelled a copied `café.txt`. The next run took the new spelling for
/// a rename: it archived the other folder's file and copied it back under
/// the decomposed name.
#[test]
fn a_name_a_folder_stores_decomposed_syncs_once() {
    let hfs = Quirks {
        names: NameMatching::new(CaseMatching::Insensitive, UnicodeMatching::Normalized),
        storage: NameStorage::Decomposed,
        ..Quirks::default()
    };
    let mut simulation = Simulation::in_memory(hfs, Quirks::default());
    simulation.start();
    // `docs/café.txt`, composed, on the folder that keeps names as given.
    simulation.play(&Round {
        edits: vec![Edit::Write(Folder::Right, 5)],
        interruption: Interruption::None,
    });
    let right = simulation.files(Folder::Right);
    assert!(right.contains_key("docs/caf\u{e9}.txt".as_bytes()));
    assert!(!right.contains_key("docs/cafe\u{301}.txt".as_bytes()));
}
