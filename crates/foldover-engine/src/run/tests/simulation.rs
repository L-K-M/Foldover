//! Simulation (PLAN.md §5.2): random edits to both folders between runs,
//! runs cancelled partway or killed at a base commit, and after every run
//! two invariants:
//!
//! - nothing is lost: a file version a run replaced or removed is in its
//!   folder's archive;
//! - a finished run leaves both folders equal outside the conflicts it
//!   reported.
//!
//! Folders are real temporary directories, so the local endpoint, its
//! staging and its archive run as in production. A failing case replays
//! from proptest's seed; `FOLDOVER_SIMULATION_CASES` scales the nightly
//! run.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use proptest::prelude::*;

use super::*;

/// Cases per test run; CI keeps it short, the nightly run raises it.
const DEFAULT_CASES: u32 = 8;

fn cases() -> u32 {
    std::env::var("FOLDOVER_SIMULATION_CASES")
        .ok()
        .and_then(|cases| cases.parse().ok())
        .unwrap_or(DEFAULT_CASES)
}

/// The paths edits touch: nested, but never a file where another path
/// needs a folder.
const PATHS: [&str; 8] = [
    "a.txt",
    "b.txt",
    "docs/c.txt",
    "docs/d.txt",
    "docs/deep/e.txt",
    "photos/f.jpg",
    "photos/g.jpg",
    "photos/2026/h.jpg",
];

#[derive(Clone, Copy, Debug)]
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

fn edit() -> impl Strategy<Value = Edit> {
    let path = 0..PATHS.len();
    prop_oneof![
        3 => (folder(), path.clone()).prop_map(|(folder, path)| Edit::Write(folder, path)),
        2 => (folder(), path.clone()).prop_map(|(folder, path)| Edit::Delete(folder, path)),
        1 => (folder(), path.clone(), path)
            .prop_map(|(folder, from, to)| Edit::Rename(folder, from, to)),
    ]
}

fn interruption() -> impl Strategy<Value = Interruption> {
    prop_oneof![
        4 => Just(Interruption::None),
        1 => (0u8..12).prop_map(Interruption::Cancel),
        1 => (0u8..3).prop_map(Interruption::Crash),
    ]
}

fn round() -> impl Strategy<Value = Round> {
    (prop::collection::vec(edit(), 0..6), interruption()).prop_map(|(edits, interruption)| Round {
        edits,
        interruption,
    })
}

/// Every file below `root`, outside Foldover's own folder, by path.
fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_name() == ".foldover" {
                continue;
            }
            if entry.file_type().unwrap().is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(root).unwrap();
                found.insert(
                    relative.to_string_lossy().into_owned(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }
    found
}

/// Every file's content in `root`'s archive, from any run.
fn archived(root: &Path) -> Vec<Vec<u8>> {
    let archive = root.join(".foldover/archive");
    if !archive.exists() {
        return Vec::new();
    }
    files(&archive).into_values().collect()
}

struct Simulation {
    pair: Fixture,
    /// Each write gets new content, so the archive check is exact.
    writes: u32,
    hour: u32,
}

impl Simulation {
    fn root(&self, folder: Folder) -> std::path::PathBuf {
        match folder {
            Folder::Left => self.pair.left.path().to_owned(),
            Folder::Right => self.pair.right.path().to_owned(),
        }
    }

    fn apply(&mut self, edit: &Edit) {
        match *edit {
            Edit::Write(folder, path) => {
                let path = self.root(folder).join(PATHS[path]);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                self.writes += 1;
                fs::write(path, format!("content {}", self.writes)).unwrap();
            }
            Edit::Delete(folder, path) => {
                let _ = fs::remove_file(self.root(folder).join(PATHS[path]));
            }
            Edit::Rename(folder, from, to) => {
                let (from, to) = (
                    self.root(folder).join(PATHS[from]),
                    self.root(folder).join(PATHS[to]),
                );
                if from.exists() && !to.exists() {
                    fs::create_dir_all(to.parent().unwrap()).unwrap();
                    fs::rename(from, to).unwrap();
                }
            }
        }
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
        let mut store = self.pair.store();
        let (mut checks, mut commits) = (0u8, 0u8);
        // Whether the interruption happened; an error is excused only then.
        let fired = std::cell::Cell::new(false);
        let outcome = prepared.execute(
            Approval::new(u64::MAX, u64::MAX),
            context,
            at(self.hour),
            &mut |planned, commit| {
                if matches!(commit, Commit::Base { .. }) {
                    if matches!(interruption, Interruption::Crash(at) if at == commits) {
                        fired.set(true);
                        return Err(crate::Error::StaleRevision);
                    }
                    commits += 1;
                }
                store.apply(planned, commit)
            },
            &mut || {
                checks = checks.saturating_add(1);
                match interruption {
                    Interruption::Cancel(at) if checks > at => {
                        fired.set(true);
                        ScanControl::Cancel
                    }
                    _ => ScanControl::Continue,
                }
            },
            &NoProgress,
        );
        match (outcome, interruption) {
            (Ok(RunOutcome::Ran(report)), _) => Ended::Finished(report),
            (Ok(RunOutcome::NeedsApproval(_)), _) => Ended::NeedsReview,
            // Only an interruption that happened may end a run in an error.
            (Err(_), _) if fired.get() => Ended::Interrupted,
            (Err(error), _) => panic!("an uninterrupted run failed: {error}"),
        }
    }

    /// Nothing that left a folder during a run is missing from its archive.
    fn check_nothing_lost(&self, before: &[BTreeMap<String, Vec<u8>>; 2]) {
        for (folder, before) in [Folder::Left, Folder::Right].iter().zip(before) {
            let root = self.root(*folder);
            let (after, archive) = (files(&root), archived(&root));
            for (path, content) in before {
                if after.get(path) == Some(content) {
                    continue;
                }
                assert!(
                    archive.contains(content),
                    "{folder:?} {path} ({}) left without an archived copy",
                    String::from_utf8_lossy(content)
                );
            }
        }
    }

    /// Both folders match except where the run reported a conflict.
    fn check_converged(&self, report: &RunReport) {
        let (left, right) = (
            files(&self.root(Folder::Left)),
            files(&self.root(Folder::Right)),
        );
        let conflicted: Vec<String> = report
            .conflicts()
            .iter()
            .map(|(path, _)| String::from_utf8_lossy(path.as_bytes()).into_owned())
            .collect();
        let affected = |path: &str| {
            conflicted
                .iter()
                .any(|conflict| path == conflict || path.starts_with(&format!("{conflict}/")))
        };
        for path in left.keys().chain(right.keys()) {
            if affected(path) {
                continue;
            }
            assert_eq!(
                left.get(path)
                    .map(|content| String::from_utf8_lossy(content).into_owned()),
                right
                    .get(path)
                    .map(|content| String::from_utf8_lossy(content).into_owned()),
                "{path} differs after a finished run"
            );
        }
    }

    fn snapshot(&self) -> [BTreeMap<String, Vec<u8>>; 2] {
        [
            files(&self.root(Folder::Left)),
            files(&self.root(Folder::Right)),
        ]
    }

    fn play(&mut self, round: &Round) {
        for edit in &round.edits {
            self.apply(edit);
        }
        let before = self.snapshot();
        let mut ended = self.run(round.interruption, RunContext::Interactive);
        self.check_nothing_lost(&before);
        // An interrupted run is followed by one that finishes, and a run a
        // gate held back by one a person reviewed.
        // Each follow-up run is checked against the state before it and
        // before the round.
        if !matches!(round.interruption, Interruption::None) {
            let between = self.snapshot();
            ended = self.run(Interruption::None, RunContext::Interactive);
            self.check_nothing_lost(&before);
            self.check_nothing_lost(&between);
        }
        if matches!(ended, Ended::NeedsReview) {
            let between = self.snapshot();
            ended = self.run(Interruption::None, RunContext::Reviewed);
            self.check_nothing_lost(&before);
            self.check_nothing_lost(&between);
        }
        match ended {
            Ended::Finished(report)
                if report.completion() == Completion::Finished && report.failures().is_empty() =>
            {
                self.check_converged(&report);
            }
            Ended::Finished(report) => panic!("a finished run reported {:?}", report.failures()),
            Ended::NeedsReview => panic!("a reviewed run still needed review"),
            Ended::Interrupted => unreachable!("only interrupted runs end interrupted"),
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn random_edits_and_interrupted_runs_lose_nothing_and_converge(
        rounds in prop::collection::vec(round(), 1..8),
    ) {
        let mut simulation = Simulation {
            pair: fixture(SyncMode::Sync),
            writes: 0,
            hour: 0,
        };
        // Some content to start from, synced once.
        for path in 0..3 {
            simulation.apply(&Edit::Write(Folder::Left, path));
        }
        simulation.play(&Round {
            edits: Vec::new(),
            interruption: Interruption::None,
        });
        for round in &rounds {
            simulation.play(round);
        }
    }
}
