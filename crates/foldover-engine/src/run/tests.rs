use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use foldover_types::{LocalRoot, PairDraft, PairId, SyncMode};

use super::*;

struct Fixture {
    left: tempfile::TempDir,
    right: tempfile::TempDir,
    mode: SyncMode,
    revision: u64,
    base: BTreeMap<SyncPath, BaselineEntry>,
    identities: RecordedIdentities,
    resolutions: BTreeMap<SyncPath, foldover_types::Resolution>,
    skips: BTreeMap<SyncPath, foldover_types::Skip>,
    filters: Vec<String>,
    size_limit: Option<u64>,
    age_limit: Option<u32>,
    durability: foldover_types::Durability,
    right_kind: RightKind,
    journal: Option<foldover_state::Journal>,
}

/// How the fixture reaches its right folder.
#[derive(Clone, Copy, PartialEq)]
enum RightKind {
    Local,
    /// Through OpenSSH's `sftp-server` on standard streams.
    Sftp,
}

fn sftp_server() -> Option<std::path::PathBuf> {
    std::env::var_os("FOLDOVER_SFTP_SERVER")
        .map(std::path::PathBuf::from)
        .into_iter()
        .chain(
            [
                "/usr/libexec/sftp-server",
                "/usr/lib/openssh/sftp-server",
                "/usr/lib/ssh/sftp-server",
            ]
            .map(std::path::PathBuf::from),
        )
        .find(|path| path.exists())
}

fn fixture(mode: SyncMode) -> Fixture {
    Fixture {
        left: tempfile::tempdir().unwrap(),
        right: tempfile::tempdir().unwrap(),
        mode,
        revision: 1,
        base: BTreeMap::new(),
        identities: RecordedIdentities::default(),
        resolutions: BTreeMap::new(),
        skips: BTreeMap::new(),
        filters: Vec::new(),
        size_limit: None,
        age_limit: None,
        durability: foldover_types::Durability::Standard,
        right_kind: RightKind::Local,
        journal: None,
    }
}

/// Applies commits like the state store, journal included.
struct Store<'a> {
    revision: u64,
    base: &'a mut BTreeMap<SyncPath, BaselineEntry>,
    identities: &'a mut RecordedIdentities,
    journal: &'a mut Option<foldover_state::Journal>,
}

impl Store<'_> {
    fn apply(&mut self, planned: u64, commit: Commit<'_>) -> Result<(), crate::Error> {
        if planned != self.revision {
            return Err(crate::Error::StaleRevision);
        }
        let updates = match commit {
            Commit::Identities { left, right } => {
                *self.identities = RecordedIdentities::new(Some(left), Some(right));
                return Ok(());
            }
            Commit::Base {
                run,
                updates,
                journal,
            } => {
                match self.journal {
                    _ if journal.is_empty() => {}
                    Some(current) if current.run == run => {
                        current.entries.extend_from_slice(journal)
                    }
                    _ => {
                        *self.journal = Some(foldover_state::Journal {
                            run: run.to_owned(),
                            entries: journal.to_vec(),
                        })
                    }
                }
                updates
            }
            Commit::Undoing { updates } => updates,
            Commit::Undid { run, seqs } => {
                if let Some(journal) = self.journal.as_mut().filter(|journal| journal.run == run) {
                    journal.entries.retain(|entry| !seqs.contains(&entry.seq));
                }
                return Ok(());
            }
            Commit::Undone { run, updates } => {
                if self
                    .journal
                    .as_ref()
                    .is_some_and(|journal| journal.run == run)
                {
                    *self.journal = None;
                }
                updates
            }
        };
        for update in updates {
            match update {
                BaseUpdate::Record(path, entry) => {
                    self.base.insert(path.clone(), *entry);
                }
                BaseUpdate::Forget(path) => {
                    self.base.remove(path);
                }
            }
        }
        Ok(())
    }
}

fn root(path: &Path) -> LocalRoot {
    LocalRoot::from_bytes(path.as_os_str().as_bytes().to_vec()).unwrap()
}

const STARTED: std::time::SystemTime = std::time::UNIX_EPOCH;

fn proceed() -> ScanControl {
    ScanControl::Continue
}

fn path(bytes: &[u8]) -> SyncPath {
    SyncPath::try_from(bytes).unwrap()
}

impl Fixture {
    fn prepare(&self) -> PairRun {
        self.try_prepare().unwrap()
    }

    fn inputs(&self) -> RunInputs {
        let filters = foldover_types::Filters::new(self.filters.clone())
            .and_then(|filters| filters.with_size_limit(self.size_limit))
            .and_then(|filters| filters.with_age_limit(self.age_limit))
            .unwrap();
        let draft = PairDraft::new(
            "pair".into(),
            root(self.left.path()),
            root(self.right.path()),
            self.mode,
        )
        .unwrap()
        .with_options(
            foldover_types::PairOptions::new(
                foldover_types::MetadataPolicy::default(),
                self.durability,
            )
            .with_filters(filters),
        );
        let pair =
            PairConfig::from_stored(PairId::from_bytes([1; 16]).unwrap(), self.revision, draft)
                .unwrap();
        RunInputs::new(
            pair,
            self.base.clone(),
            self.identities,
            self.resolutions.clone(),
        )
        .with_skips(self.skips.clone())
        .with_lease_timing(TEST_LEASES)
    }

    fn try_prepare(&self) -> Result<PairRun, RunError> {
        let inputs = self.inputs();
        if self.right_kind == RightKind::Local {
            return PairRun::prepare(inputs, &mut proceed, &NoProgress);
        }
        let server = sftp_server().expect("callers check for sftp-server");
        let right = foldover_endpoint::SftpEndpoint::spawn(
            &server,
            &[std::ffi::OsStr::new("-d"), std::ffi::OsStr::new("/")],
            self.right.path().as_os_str().as_bytes(),
        )
        .unwrap();
        let left = foldover_endpoint::LocalEndpoint::open(self.left.path()).unwrap();
        let endpoints = foldover_endpoint::EndpointPair::new(
            foldover_endpoint::Endpoint::Local(left),
            foldover_endpoint::Endpoint::Sftp(right),
        );
        PairRun::prepare_on(inputs, endpoints, &mut proceed, &NoProgress)
    }

    /// Run with an in-memory base that applies commits like the state store.
    fn run(&mut self, approval: Approval, context: RunContext) -> RunOutcome {
        self.run_at(approval, context, STARTED)
    }

    fn run_at(
        &mut self,
        approval: Approval,
        context: RunContext,
        started: std::time::SystemTime,
    ) -> RunOutcome {
        let prepared = self.prepare();
        let mut store = self.store();
        prepared
            .execute(
                approval,
                context,
                started,
                &mut |planned, commit| store.apply(planned, commit),
                &mut proceed,
                &NoProgress,
            )
            .unwrap()
    }

    fn store(&mut self) -> Store<'_> {
        Store {
            revision: self.revision,
            base: &mut self.base,
            identities: &mut self.identities,
            journal: &mut self.journal,
        }
    }

    /// A first run stands for the reviewed preview a person accepted.
    fn sync(&mut self) -> RunReport {
        let context = if self.identities.side(Side::Left).is_none() {
            RunContext::Reviewed
        } else {
            RunContext::Interactive
        };
        match self.run(Approval::default(), context) {
            RunOutcome::Ran(report) => report,
            RunOutcome::NeedsApproval(preview) => {
                panic!("unexpected approval request: {preview:?}")
            }
        }
    }
}

#[test]
fn first_sync_copies_both_ways_and_a_second_run_has_nothing_to_do() {
    let mut pair = fixture(SyncMode::Sync);
    fs::create_dir(pair.left.path().join("photos")).unwrap();
    fs::write(pair.left.path().join("photos/a.jpg"), b"a").unwrap();
    fs::write(pair.right.path().join("notes.txt"), b"n").unwrap();

    let report = pair.sync();
    assert_eq!(report.copied(), 3);
    assert!(report.failures().is_empty(), "{:?}", report.failures());
    assert_eq!(
        fs::read(pair.right.path().join("photos/a.jpg")).unwrap(),
        b"a"
    );
    assert_eq!(fs::read(pair.left.path().join("notes.txt")).unwrap(), b"n");
    assert_eq!(pair.base.len(), 3);

    assert!(pair.prepare().preview().items().is_empty());
}

#[test]
fn edits_replace_and_archive_and_deletions_propagate_to_the_archive() {
    let mut pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("edit"), b"v1").unwrap();
    fs::write(pair.left.path().join("delete"), b"d").unwrap();
    pair.sync();

    fs::write(pair.left.path().join("edit"), b"v2").unwrap();
    fs::remove_file(pair.right.path().join("delete")).unwrap();
    let preview = pair.prepare().preview();
    assert_eq!(
        preview
            .items()
            .iter()
            .map(|item| (item.path().clone(), item.action()))
            .collect::<Vec<_>>(),
        [
            (path(b"delete"), Action::Archive { on: Side::Left }),
            (
                path(b"edit"),
                Action::Copy {
                    to: Side::Right,
                    change: ChangeKind::ReplaceContent
                }
            ),
        ]
    );
    // Only copies move data; an archive moves nothing between folders.
    assert_eq!(
        preview
            .items()
            .iter()
            .map(PreviewItem::bytes)
            .collect::<Vec<_>>(),
        [0, 2]
    );

    let report = pair.sync();
    assert_eq!((report.copied(), report.archived()), (1, 1));
    assert_eq!(fs::read(pair.right.path().join("edit")).unwrap(), b"v2");
    assert_eq!(
        fs::read(
            pair.right
                .path()
                .join(".foldover/archive/1970-01-01T00-00-00.000Z/edit")
        )
        .unwrap(),
        b"v1"
    );
    assert!(!pair.left.path().join("delete").exists());
    assert_eq!(
        fs::read(
            pair.left
                .path()
                .join(".foldover/archive/1970-01-01T00-00-00.000Z/delete")
        )
        .unwrap(),
        b"d"
    );
    assert!(!pair.base.contains_key(&path(b"delete")));
    assert!(pair.prepare().preview().items().is_empty());
}

#[test]
fn conflicts_are_reported_and_both_versions_stay_untouched() {
    let mut pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("file"), b"base").unwrap();
    pair.sync();
    fs::write(pair.left.path().join("file"), b"left").unwrap();
    fs::write(pair.right.path().join("file"), b"right").unwrap();

    let report = pair.sync();
    assert_eq!(
        report.conflicts(),
        [(path(b"file"), ConflictKind::BothChanged)]
    );
    assert_eq!(fs::read(pair.left.path().join("file")).unwrap(), b"left");
    assert_eq!(fs::read(pair.right.path().join("file")).unwrap(), b"right");
}

#[test]
fn mass_deletions_need_approval_and_unattended_runs_stop() {
    let mut pair = fixture(SyncMode::Sync);
    // One file stays, so the emptied-folder gate does not decide this.
    for index in 0..=60 {
        fs::write(pair.left.path().join(format!("{index}")), b"x").unwrap();
    }
    pair.sync();
    for index in 0..60 {
        fs::remove_file(pair.right.path().join(format!("{index}"))).unwrap();
    }

    let RunOutcome::NeedsApproval(preview) = pair.run(Approval::default(), RunContext::Interactive)
    else {
        panic!("expected an approval request");
    };
    assert_eq!(preview.deletions(Side::Left).deletions(), 60);
    // Every file plus the identity folder survives the refused run.
    assert_eq!(fs::read_dir(pair.left.path()).unwrap().count(), 62);
    assert!(matches!(
        pair.run(Approval::new(60, 0), RunContext::Unattended),
        RunOutcome::NeedsApproval(_)
    ));
    assert!(matches!(
        pair.run(Approval::new(59, 0), RunContext::Interactive),
        RunOutcome::NeedsApproval(_)
    ));

    let RunOutcome::Ran(report) = pair.run(Approval::new(60, 0), RunContext::Interactive) else {
        panic!("approved deletions should run");
    };
    assert_eq!(report.archived(), 60);
}

#[test]
fn mirror_archives_target_extras_and_backup_keeps_them() {
    let mut mirror = fixture(SyncMode::Mirror { source: Side::Left });
    fs::write(mirror.left.path().join("kept"), b"k").unwrap();
    fs::write(mirror.right.path().join("extra"), b"e").unwrap();
    // A first run waits for a reviewed preview (§3.2, §3.6).
    assert!(matches!(
        mirror.run(Approval::default(), RunContext::Interactive),
        RunOutcome::NeedsApproval(_)
    ));
    assert!(mirror.right.path().join("extra").exists());
    let RunOutcome::Ran(report) = mirror.run(Approval::new(0, 1), RunContext::Reviewed) else {
        panic!("a reviewed approval runs the mirror")
    };
    assert_eq!((report.copied(), report.archived()), (1, 1));
    assert!(!mirror.right.path().join("extra").exists());
    assert!(
        mirror
            .right
            .path()
            .join(".foldover/archive/1970-01-01T00-00-00.000Z/extra")
            .exists()
    );

    let mut backup = fixture(SyncMode::Backup { source: Side::Left });
    fs::write(backup.left.path().join("kept"), b"k").unwrap();
    fs::write(backup.right.path().join("extra"), b"e").unwrap();
    let report = backup.sync();
    assert_eq!((report.copied(), report.archived()), (1, 0));
    assert!(backup.right.path().join("extra").exists());
}

#[test]
fn a_changed_pair_revision_stops_the_run() {
    let mut pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("file"), b"x").unwrap();
    let prepared = pair.prepare();
    pair.revision = 2;
    let result = prepared.execute(
        Approval::default(),
        RunContext::Reviewed,
        STARTED,
        &mut |planned, _| {
            if planned == pair.revision {
                Ok(())
            } else {
                Err(crate::Error::StaleRevision)
            }
        },
        &mut proceed,
        &NoProgress,
    );
    assert!(matches!(
        result,
        Err(RunError::Commit(crate::Error::StaleRevision))
    ));
}

#[test]
fn failed_folders_skip_their_contents_and_later_runs_recover_leftovers() {
    let mut pair = fixture(SyncMode::Mirror { source: Side::Left });
    fs::create_dir(pair.left.path().join("folder")).unwrap();
    fs::write(pair.left.path().join("folder/file"), b"x").unwrap();
    // A file where the folder must go makes its creation fail.
    let prepared = pair.prepare();
    fs::write(pair.right.path().join("folder"), b"blocker").unwrap();
    let mut base = BTreeMap::new();
    let RunOutcome::Ran(report) = prepared
        .execute(
            Approval::default(),
            RunContext::Reviewed,
            STARTED,
            &mut |_, commit| {
                if let Commit::Base { updates, .. } = commit {
                    for update in updates {
                        if let BaseUpdate::Record(path, entry) = update {
                            base.insert(path.clone(), *entry);
                        }
                    }
                }
                Ok(())
            },
            &mut proceed,
            &NoProgress,
        )
        .unwrap()
    else {
        panic!("expected a run");
    };
    assert_eq!(report.failures().len(), 1);
    assert_eq!(report.failures()[0].path(), &path(b"folder"));
    assert!(base.is_empty());
    assert_eq!(
        fs::read(pair.right.path().join("folder")).unwrap(),
        b"blocker"
    );

    fs::remove_file(pair.right.path().join("folder")).unwrap();
    fs::write(pair.right.path().join(".foldover-tmp-left"), b"displaced").unwrap();
    let report = pair.sync();
    assert_eq!((report.copied(), report.recovered()), (2, 1));
    assert_eq!(
        fs::read(
            pair.right
                .path()
                .join(".foldover/archive/1970-01-01T00-00-00.000Z/.foldover-tmp-left")
        )
        .unwrap(),
        b"displaced"
    );
}

#[test]
fn a_replaced_folder_at_the_same_path_stops_before_any_change() {
    let mut pair = fixture(SyncMode::Sync);
    for index in 0..3 {
        fs::write(pair.left.path().join(format!("{index}")), b"x").unwrap();
    }
    pair.sync();
    assert!(pair.identities.side(Side::Left).is_some());
    assert!(pair.identities.side(Side::Right).is_some());

    // An unmounted disk can leave an empty folder at the same path.
    let original = pair.right.path().with_extension("original");
    fs::rename(pair.right.path(), &original).unwrap();
    fs::create_dir(pair.right.path()).unwrap();
    assert!(matches!(
        pair.try_prepare(),
        Err(RunError::IdentityChanged { side: Side::Right })
    ));
    assert_eq!(fs::read_dir(pair.left.path()).unwrap().count(), 4);

    fs::remove_dir(pair.right.path()).unwrap();
    fs::rename(&original, pair.right.path()).unwrap();
    assert!(pair.prepare().preview().items().is_empty());
}

#[test]
fn completed_runs_name_their_archive_and_prune_expired_runs() {
    let pair = fixture(SyncMode::Mirror { source: Side::Left });
    fs::write(pair.left.path().join("file"), b"x").unwrap();

    // A run 31 days after an archived one deletes it; a newer one stays.
    let day = std::time::Duration::from_secs(86_400);
    let archive = pair.right.path().join(".foldover/archive");
    for age in [31, 29] {
        let name = foldover_endpoint::archive_name(std::time::UNIX_EPOCH + day * (40 - age));
        fs::create_dir_all(archive.join(&name)).unwrap();
        fs::write(archive.join(name).join("kept"), b"k").unwrap();
    }
    let prepared = pair.prepare();
    let started = std::time::UNIX_EPOCH + day * 40;
    let RunOutcome::Ran(report) = prepared
        .execute(
            Approval::default(),
            RunContext::Reviewed,
            started,
            &mut |_, _| Ok(()),
            &mut proceed,
            &NoProgress,
        )
        .unwrap()
    else {
        panic!("expected a run");
    };
    assert_eq!(report.archive(), foldover_endpoint::archive_name(started));
    assert_eq!(report.pruned(), 1);
    assert!(
        !archive
            .join(foldover_endpoint::archive_name(
                std::time::UNIX_EPOCH + day * 9
            ))
            .exists()
    );
    assert!(
        archive
            .join(foldover_endpoint::archive_name(
                std::time::UNIX_EPOCH + day * 11
            ))
            .exists()
    );
}

#[test]
fn resolved_conflicts_keep_the_chosen_side_until_either_side_changes() {
    let mut pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("file"), b"base").unwrap();
    fs::write(pair.left.path().join("gone"), b"base").unwrap();
    pair.sync();
    fs::write(pair.left.path().join("file"), b"left").unwrap();
    fs::write(pair.right.path().join("file"), b"right").unwrap();
    fs::remove_file(pair.left.path().join("gone")).unwrap();
    fs::write(pair.right.path().join("gone"), b"edited").unwrap();

    let preview = pair.prepare().preview();
    let versions = |name: &[u8]| {
        preview
            .items()
            .iter()
            .find(|item| item.path() == &path(name))
            .unwrap()
            .versions()
    };
    let (left, right) = versions(b"file");
    pair.resolutions.insert(
        path(b"file"),
        foldover_types::Resolution::new(Side::Right, left, right),
    );
    let (left, right) = versions(b"gone");
    pair.resolutions.insert(
        path(b"gone"),
        foldover_types::Resolution::new(Side::Left, left, right),
    );

    let report = pair.sync();
    assert!(report.conflicts().is_empty(), "{:?}", report.conflicts());
    assert_eq!((report.copied(), report.archived()), (1, 1));
    assert_eq!(fs::read(pair.left.path().join("file")).unwrap(), b"right");
    let archive = pair
        .left
        .path()
        .join(".foldover/archive/1970-01-01T00-00-00.000Z");
    assert_eq!(fs::read(archive.join("file")).unwrap(), b"left");
    assert!(!pair.right.path().join("gone").exists());

    // A choice made for other versions no longer applies.
    fs::write(pair.left.path().join("file"), b"left again").unwrap();
    fs::write(pair.right.path().join("file"), b"right again").unwrap();
    let report = pair.sync();
    assert_eq!(
        report.conflicts(),
        [(path(b"file"), ConflictKind::BothChanged)]
    );
}

/// A folder deleted on one side while a file inside it changed on the
/// other is one conflict at the folder; choosing a side settles the folder
/// and everything in it.
#[test]
fn a_folder_choice_settles_the_whole_folder() {
    for keep in [Side::Left, Side::Right] {
        let mut pair = fixture(SyncMode::Sync);
        fs::create_dir(pair.left.path().join("photos")).unwrap();
        fs::write(pair.left.path().join("photos/a.jpg"), b"a").unwrap();
        fs::write(pair.left.path().join("photos/b.jpg"), b"b").unwrap();
        pair.sync();
        fs::write(pair.left.path().join("photos/a.jpg"), b"edited").unwrap();
        fs::remove_dir_all(pair.right.path().join("photos")).unwrap();

        let preview = pair.prepare().preview();
        let folder = preview
            .items()
            .iter()
            .find(|item| item.path() == &path(b"photos"))
            .unwrap();
        assert_eq!(
            folder.action(),
            Action::Conflict(ConflictKind::DeletedVersusEdited)
        );
        let (left, right) = folder.versions();
        pair.resolutions.insert(
            path(b"photos"),
            foldover_types::Resolution::new(keep, left, right),
        );

        // Keeping B's deletion empties folder A, so the choice runs as the
        // reviewed preview a person accepts after choosing (§3.6).
        let RunOutcome::Ran(report) = pair.run(Approval::new(3, 0), RunContext::Reviewed) else {
            panic!("a reviewed choice runs");
        };
        assert!(report.conflicts().is_empty(), "{:?}", report.conflicts());
        assert!(report.failures().is_empty(), "{:?}", report.failures());
        match keep {
            Side::Left => {
                let photos = pair.right.path().join("photos");
                assert_eq!(fs::read(photos.join("a.jpg")).unwrap(), b"edited");
                assert_eq!(fs::read(photos.join("b.jpg")).unwrap(), b"b");
            }
            Side::Right => {
                assert!(!pair.left.path().join("photos").exists());
                let archive = pair
                    .left
                    .path()
                    .join(".foldover/archive/1970-01-01T00-00-00.000Z/photos");
                assert_eq!(fs::read(archive.join("a.jpg")).unwrap(), b"edited");
                assert_eq!(fs::read(archive.join("b.jpg")).unwrap(), b"b");
            }
        }
        assert!(pair.prepare().preview().items().is_empty());
    }
}

#[test]
fn earlier_snapshots_supply_versions_for_settled_unchanged_entries() {
    let pair = fixture(SyncMode::Sync);
    let file = pair.left.path().join("file");
    fs::write(&file, b"data").unwrap();
    let past = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(past)
        .unwrap();
    let first = pair.prepare();

    // A deliberately wrong stored version shows whether it was reused.
    let stored = foldover_types::EntryVersion::new(
        foldover_types::EntryContent::File(foldover_types::Digest::from_bytes([7; 32])),
        foldover_types::Digest::from_bytes([7; 32]),
    );
    let left: BTreeMap<_, _> = first
        .snapshot(Side::Left)
        .into_iter()
        .map(|entry| {
            let observed =
                foldover_endpoint::ObservedEntry::restore(stored, &entry.fingerprint).unwrap();
            (entry.path, observed)
        })
        .collect();
    let draft = PairDraft::new(
        "pair".into(),
        root(pair.left.path()),
        root(pair.right.path()),
        pair.mode,
    )
    .unwrap();
    let config =
        PairConfig::from_stored(PairId::from_bytes([1; 16]).unwrap(), pair.revision, draft)
            .unwrap();
    let inputs = RunInputs::new(config, BTreeMap::new(), pair.identities, BTreeMap::new())
        .with_snapshots(left, BTreeMap::new());
    let second = PairRun::prepare(inputs, &mut proceed, &NoProgress).unwrap();
    let preview = second.preview();
    assert_eq!(preview.items()[0].versions().0, Some(stored));
}

#[test]
fn filters_from_the_pair_and_both_ignore_files_exclude_entries_on_both_sides() {
    let mut pair = fixture(SyncMode::Sync);
    pair.filters = vec!["*.tmp".into()];
    fs::write(pair.left.path().join("a.tmp"), b"tmp").unwrap();
    fs::write(pair.left.path().join("keep"), b"k").unwrap();
    fs::create_dir(pair.right.path().join("cache")).unwrap();
    fs::write(pair.right.path().join("cache/blob"), b"b").unwrap();
    // Only the right root names the cache; both sides honor it.
    fs::write(pair.right.path().join(".foldoverignore"), b"cache/\n").unwrap();
    pair.sync();
    assert!(!pair.right.path().join("a.tmp").exists());
    assert!(pair.right.path().join("keep").exists());
    assert!(!pair.left.path().join("cache").exists());
    assert!(pair.left.path().join(".foldoverignore").exists());

    // Excluding an already synced file forgets it instead of deleting it.
    fs::write(pair.left.path().join(".foldoverignore"), b"cache/\nkeep\n").unwrap();
    pair.sync();
    fs::remove_file(pair.left.path().join("keep")).unwrap();
    let report = pair.sync();
    assert_eq!(report.archived(), 0);
    assert!(pair.right.path().join("keep").exists());
}

#[test]
fn runs_count_the_bytes_each_side_receives_and_check_free_space() {
    let pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("big"), vec![1; 4096]).unwrap();
    fs::write(pair.right.path().join("small"), vec![2; 100]).unwrap();
    fs::create_dir(pair.right.path().join("folder")).unwrap();
    let run = pair.prepare();
    assert_eq!(run.bytes_to_copy(Side::Right), 4096);
    assert_eq!(run.bytes_to_copy(Side::Left), 100);
    // A temporary volume has room for this.
    assert!(
        pair.prepare()
            .endpoints
            .endpoint(Side::Left)
            .available_space()
            .unwrap()
            > 0
    );
}

#[test]
fn copies_need_their_size_plus_the_reserve() {
    let reserve = super::SPACE_RESERVE;
    assert!(super::require_space(Side::Left, 10, 10 + reserve).is_ok());
    assert!(matches!(
        super::require_space(Side::Right, 11, 10 + reserve),
        Err(RunError::InsufficientSpace {
            side: Side::Right,
            needed: 11,
            ..
        })
    ));
    assert!(super::require_space(Side::Left, u64::MAX, u64::MAX).is_err());
}

#[test]
fn files_too_large_in_either_folder_are_skipped_in_both() {
    let mut pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("small"), b"a").unwrap();
    fs::write(pair.left.path().join("big"), b"0123456789").unwrap();
    pair.sync();

    pair.size_limit = Some(5);
    assert!(pair.prepare().preview().items().is_empty());

    // Shrinking it on one side changes nothing while the other is too large.
    fs::write(pair.left.path().join("big"), b"xyz").unwrap();
    fs::write(pair.left.path().join("small"), b"b").unwrap();
    let report = pair.sync();
    assert_eq!((report.copied(), report.archived()), (1, 0));
    assert_eq!(
        fs::read(pair.right.path().join("big")).unwrap(),
        b"0123456789"
    );
    assert_eq!(fs::read(pair.left.path().join("big")).unwrap(), b"xyz");
    assert_eq!(fs::read(pair.right.path().join("small")).unwrap(), b"b");

    // The base row survived the skip, so the edit made meanwhile syncs.
    pair.size_limit = None;
    let preview = pair.prepare().preview();
    assert_eq!(
        preview
            .items()
            .iter()
            .map(|item| (item.path().clone(), item.action()))
            .collect::<Vec<_>>(),
        [(
            path(b"big"),
            Action::Copy {
                to: Side::Right,
                change: ChangeKind::ReplaceContent
            }
        )]
    );
}

#[test]
fn files_old_in_both_folders_are_skipped_until_edited() {
    let mut pair = fixture(SyncMode::Sync);
    let age = |name: &str, folder: &Path| {
        let file = fs::File::options()
            .write(true)
            .open(folder.join(name))
            .unwrap();
        let hundred_days = std::time::Duration::from_secs(100 * 24 * 60 * 60);
        file.set_modified(std::time::SystemTime::now() - hundred_days)
            .unwrap();
    };
    // Synced long ago: the copy carries the old modification time.
    fs::write(pair.left.path().join("kept"), b"k").unwrap();
    age("kept", pair.left.path());
    pair.sync();
    fs::write(pair.left.path().join("stale"), b"s").unwrap();
    age("stale", pair.left.path());
    fs::write(pair.left.path().join("fresh"), b"f").unwrap();

    pair.age_limit = Some(30);
    let report = pair.sync();
    assert_eq!(report.copied(), 1);
    assert!(pair.right.path().join("fresh").exists());
    assert!(!pair.right.path().join("stale").exists());

    // An edit makes it recent on one side: it syncs as an edit.
    fs::write(pair.left.path().join("kept"), b"edited").unwrap();
    let report = pair.sync();
    assert_eq!((report.copied(), report.conflicts().len()), (1, 0));
    assert_eq!(fs::read(pair.right.path().join("kept")).unwrap(), b"edited");
}

#[test]
fn age_excluded_files_stay_in_the_snapshot_cache_for_reuse() {
    let mut pair = fixture(SyncMode::Sync);
    let file = pair.left.path().join("old");
    fs::write(&file, b"data").unwrap();
    let hundred_days = std::time::Duration::from_secs(100 * 24 * 60 * 60);
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - hundred_days)
        .unwrap();
    pair.age_limit = Some(30);

    // Old on the only side that has it: skipped in both, so not planned.
    let first = pair.prepare();
    assert!(first.preview().items().is_empty());
    // The observation still feeds the next run's cache.
    let cached = first
        .snapshot(Side::Left)
        .into_iter()
        .find(|entry| entry.path == path(b"old"))
        .expect("an age-excluded file stays cached");

    // A wrong stored version proves the next run reused it without rereading.
    let stored = foldover_types::EntryVersion::new(
        foldover_types::EntryContent::File(foldover_types::Digest::from_bytes([7; 32])),
        foldover_types::Digest::from_bytes([7; 32]),
    );
    let left: BTreeMap<_, _> = [(
        cached.path.clone(),
        foldover_endpoint::ObservedEntry::restore(stored, &cached.fingerprint).unwrap(),
    )]
    .into_iter()
    .collect();
    let inputs = pair.inputs().with_snapshots(left, BTreeMap::new());
    let second = PairRun::prepare(inputs, &mut proceed, &NoProgress).unwrap();
    assert!(second.preview().items().is_empty());
    assert_eq!(
        second
            .snapshot(Side::Left)
            .iter()
            .find(|entry| entry.path == path(b"old"))
            .map(|entry| entry.version),
        Some(stored)
    );
}

const MIB: usize = 1024 * 1024;

/// Content big enough to resume.
fn resumable_content() -> Vec<u8> {
    (0..20 * MIB).map(|index| (index % 253) as u8).collect()
}

/// Run until about 18 MiB of 32 KiB reads went by, then cancel; returns
/// the partial the broken-off copy kept.
fn break_off(pair: &mut Fixture) -> std::path::PathBuf {
    let prepared = pair.prepare();
    let mut store = pair.store();
    let mut reads = 0;
    let outcome = prepared
        .execute(
            Approval::default(),
            RunContext::Reviewed,
            at(1),
            &mut |planned, commit| store.apply(planned, commit),
            &mut || {
                reads += 1;
                if reads > 18 * 32 {
                    ScanControl::Cancel
                } else {
                    ScanControl::Continue
                }
            },
            &NoProgress,
        )
        .unwrap();
    let RunOutcome::Ran(report) = outcome else {
        panic!("unexpected approval request");
    };
    assert_eq!(report.copied(), 0);
    let mut kept = fs::read_dir(pair.left.path().join(".foldover/partial"))
        .unwrap()
        .map(|entry| entry.unwrap().path());
    let partial = kept.next().expect("a kept partial");
    assert!(kept.next().is_none());
    partial
}

/// PLAN.md §4.9: a large copy that breaks off keeps what arrived, and the
/// next run fetches only the rest.
#[test]
fn a_broken_off_copy_from_a_server_resumes_where_it_stopped() {
    if sftp_server().is_none() {
        eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
        return;
    }
    let mut pair = fixture(SyncMode::Sync);
    pair.right_kind = RightKind::Sftp;
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    let data = resumable_content();
    fs::write(right.join("big.bin"), &data).unwrap();

    let partial = break_off(&mut pair);
    let kept = fs::metadata(&partial).unwrap().len();
    assert!((16 * MIB as u64..20 * MIB as u64).contains(&kept), "{kept}");
    assert!(!left.join("big.bin").exists());

    let report = pair.sync();
    assert_eq!(report.copied(), 1, "{:?}", report.failures());
    assert!(fs::read(left.join("big.bin")).unwrap() == data);
    assert!(!partial.exists());
}

/// A kept part that no longer matches fails its copy once, then the copy
/// starts over.
#[test]
fn a_corrupt_partial_is_dropped_and_the_copy_starts_over() {
    if sftp_server().is_none() {
        eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
        return;
    }
    let mut pair = fixture(SyncMode::Sync);
    pair.right_kind = RightKind::Sftp;
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    let data = resumable_content();
    fs::write(right.join("big.bin"), &data).unwrap();
    let partial = break_off(&mut pair);
    let mut garbled = fs::read(&partial).unwrap();
    garbled[MIB] ^= 0xff;
    fs::write(&partial, garbled).unwrap();

    let report = pair.sync();
    assert_eq!(report.copied(), 0);
    assert_eq!(report.failures().len(), 1, "{:?}", report.failures());
    assert!(!partial.exists());
    assert!(!left.join("big.bin").exists());

    let report = pair.sync();
    assert_eq!(report.copied(), 1, "{:?}", report.failures());
    assert!(fs::read(left.join("big.bin")).unwrap() == data);
}

#[test]
fn sftp_folders_sync_like_local_ones() {
    if sftp_server().is_none() {
        eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
        return;
    }
    let mut pair = fixture(SyncMode::Sync);
    pair.right_kind = RightKind::Sftp;
    fs::create_dir(pair.left.path().join("docs")).unwrap();
    fs::write(pair.left.path().join("docs/a.txt"), b"a").unwrap();
    fs::write(pair.left.path().join("top"), vec![3; 300_000]).unwrap();
    std::os::unix::fs::symlink("top", pair.left.path().join("link")).unwrap();
    let report = pair.sync();
    assert_eq!(report.copied(), 4, "{:?}", report.failures());
    assert_eq!(
        fs::read(pair.right.path().join("docs/a.txt")).unwrap(),
        b"a"
    );
    assert_eq!(
        fs::read(pair.right.path().join("top")).unwrap(),
        vec![3; 300_000]
    );
    assert!(pair.right.path().join(".foldover/id").exists());
    assert!(pair.prepare().preview().items().is_empty());

    // An edit on the server comes back; a deletion here is archived there.
    fs::write(
        pair.right.path().join("docs/a.txt"),
        b"edited on the server",
    )
    .unwrap();
    fs::remove_file(pair.left.path().join("top")).unwrap();
    let report = pair.sync();
    assert_eq!(
        (report.copied(), report.archived()),
        (1, 1),
        "{:?}",
        report.failures()
    );
    assert_eq!(
        fs::read(pair.left.path().join("docs/a.txt")).unwrap(),
        b"edited on the server"
    );
    assert!(!pair.right.path().join("top").exists());
    let archived = pair
        .right
        .path()
        .join(".foldover/archive")
        .join(report.archive())
        .join("top");
    assert_eq!(fs::read(archived).unwrap(), vec![3; 300_000]);

    // Edits on both sides are a conflict; neither is overwritten.
    fs::write(pair.left.path().join("docs/a.txt"), b"here").unwrap();
    fs::write(pair.right.path().join("docs/a.txt"), b"there").unwrap();
    let report = pair.sync();
    assert_eq!(report.conflicts().len(), 1);
    assert_eq!(
        fs::read(pair.right.path().join("docs/a.txt")).unwrap(),
        b"there"
    );
}

/// Full durability cannot be promised to an SFTP folder; the run must
/// refuse before anything mutates either side, identity files included.
#[test]
fn full_durability_rejects_a_run_with_an_sftp_side() {
    if sftp_server().is_none() {
        eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
        return;
    }
    let mut pair = fixture(SyncMode::Sync);
    pair.right_kind = RightKind::Sftp;
    pair.durability = foldover_types::Durability::Full;
    fs::write(pair.left.path().join("report"), b"new").unwrap();
    let error = pair
        .prepare()
        .execute(
            Approval::default(),
            RunContext::Interactive,
            STARTED,
            &mut |_, _| panic!("nothing may commit"),
            &mut proceed,
            &NoProgress,
        )
        .unwrap_err();
    assert!(
        matches!(error, RunError::UnsupportedDurability { side: Side::Right }),
        "{error:?}"
    );
    // Nothing was written: no identities, no copy, no archive.
    assert!(!pair.left.path().join(".foldover").exists());
    assert!(!pair.right.path().join(".foldover").exists());
    assert!(!pair.right.path().join("report").exists());
    assert_eq!(fs::read(pair.left.path().join("report")).unwrap(), b"new");
}

#[test]
fn a_first_run_waits_for_a_reviewed_preview_even_when_it_only_copies() {
    let mut pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("new"), b"n").unwrap();

    for context in [RunContext::Interactive, RunContext::Unattended] {
        let RunOutcome::NeedsApproval(preview) = pair.run(Approval::default(), context) else {
            panic!("an unreviewed first run must wait");
        };
        assert_eq!(preview.items().len(), 1);
    }
    // Nothing was written: no identities, no copy.
    assert!(pair.identities.side(Side::Left).is_none());
    assert!(!pair.left.path().join(".foldover").exists());
    assert_eq!(fs::read_dir(pair.right.path()).unwrap().count(), 0);

    let RunOutcome::Ran(report) = pair.run(Approval::default(), RunContext::Reviewed) else {
        panic!("a reviewed first run runs");
    };
    assert_eq!(report.copied(), 1);
    assert!(pair.identities.side(Side::Right).is_some());
}

#[test]
fn a_suddenly_empty_folder_needs_review_before_the_other_side_is_emptied() {
    let mut pair = fixture(SyncMode::Sync);
    for index in 0..3 {
        fs::write(pair.left.path().join(format!("{index}")), b"x").unwrap();
    }
    pair.sync();
    for index in 0..3 {
        fs::remove_file(pair.right.path().join(format!("{index}"))).unwrap();
    }

    // Three deletions are far below the mass-deletion fuse.
    for context in [RunContext::Interactive, RunContext::Unattended] {
        assert!(matches!(
            pair.run(Approval::new(3, 0), context),
            RunOutcome::NeedsApproval(_)
        ));
    }
    assert_eq!(fs::read_dir(pair.left.path()).unwrap().count(), 4);
    let RunOutcome::Ran(report) = pair.run(Approval::new(3, 0), RunContext::Reviewed) else {
        panic!("a reviewed plan runs");
    };
    assert_eq!(report.archived(), 3);

    // An emptied Backup target only receives copies again: no review.
    let mut backup = fixture(SyncMode::Backup { source: Side::Left });
    fs::write(backup.left.path().join("kept"), b"k").unwrap();
    backup.sync();
    fs::remove_file(backup.right.path().join("kept")).unwrap();
    assert_eq!(backup.sync().copied(), 1);
}

#[test]
fn a_root_whose_identity_changes_mid_run_stops_at_the_next_batch() {
    let mut pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("first"), b"f").unwrap();
    pair.sync();
    for index in 0..300 {
        fs::write(pair.left.path().join(format!("{index:03}")), b"x").unwrap();
    }

    let prepared = pair.prepare();
    let identity = pair.right.path().join(".foldover/id");
    let other = EndpointId::from_bytes([7; 16]);
    let mut calls = 0;
    let mut control = || {
        calls += 1;
        if calls == 10 {
            fs::write(&identity, format!("{other}\n")).unwrap();
        }
        ScanControl::Continue
    };
    let base = &mut pair.base;
    let error = prepared
        .execute(
            Approval::default(),
            RunContext::Interactive,
            STARTED,
            &mut |_, commit| {
                if let Commit::Base { updates, .. } = commit {
                    for update in updates {
                        if let BaseUpdate::Record(path, entry) = update {
                            base.insert(path.clone(), *entry);
                        }
                    }
                }
                Ok(())
            },
            &mut control,
            &NoProgress,
        )
        .unwrap_err();
    assert!(
        matches!(error, RunError::IdentityChanged { side: Side::Right }),
        "{error:?}"
    );

    // Writes up to the check landed and were recorded; the rest waited.
    let copied = fs::read_dir(pair.right.path()).unwrap().count() - 1;
    assert_eq!(copied, pair.base.len());
    assert_eq!(copied, 1 + COMMIT_BATCH);
}

const HOUR: std::time::Duration = std::time::Duration::from_secs(3_600);

/// Leases that give up on a busy folder at once and count a still one as
/// dead quickly.
const TEST_LEASES: LeaseTiming = LeaseTiming {
    ttl: std::time::Duration::from_millis(200),
    beat: std::time::Duration::from_millis(50),
    poll: std::time::Duration::from_millis(20),
    wait: std::time::Duration::ZERO,
};

/// PLAN.md §4: while another Mac's run holds a folder's lease, a run
/// writes nothing and says who is syncing.
#[test]
fn a_folder_another_mac_is_syncing_waits_until_its_lease_is_free_or_dead() {
    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    fs::write(left.join("keep.txt"), b"k").unwrap();
    pair.sync();
    fs::write(left.join("new.txt"), b"n").unwrap();
    let lock = right.join(".foldover/lock");
    fs::write(
        &lock,
        "foldover-lease 1\nholder other/run\nlabel Studio\nbeat 1\n",
    )
    .unwrap();

    let prepared = pair.prepare();
    let mut store = pair.store();
    let busy = prepared.execute(
        Approval::default(),
        RunContext::Interactive,
        at(1),
        &mut |planned, commit| store.apply(planned, commit),
        &mut proceed,
        &NoProgress,
    );
    assert!(
        matches!(&busy, Err(RunError::Lease(LeaseError::Busy { label })) if label == "Studio"),
        "{busy:?}"
    );
    assert!(!right.join("new.txt").exists());
    // Released: the run proceeds and leaves no lease behind.
    fs::remove_file(&lock).unwrap();
    pair.ran(Approval::default(), at(2));
    assert_eq!(fs::read(right.join("new.txt")).unwrap(), b"n");
    assert!(!lock.exists());
}

/// Runs `hours` after the first, so each gets its own archive folder.
fn at(hours: u32) -> std::time::SystemTime {
    STARTED + HOUR * hours
}

impl Fixture {
    fn ran(&mut self, approval: Approval, started: std::time::SystemTime) -> RunReport {
        match self.run_at(approval, RunContext::Interactive, started) {
            RunOutcome::Ran(report) => report,
            RunOutcome::NeedsApproval(preview) => panic!("unexpected approval: {preview:?}"),
        }
    }

    fn undo_at(&mut self, started: std::time::SystemTime) -> UndoReport {
        let journal = self.journal.clone().expect("a journaled run");
        let prepared = self.prepare();
        let mut store = self.store();
        prepared
            .undo(
                journal,
                started,
                &mut |planned, commit| store.apply(planned, commit),
                &mut proceed,
            )
            .unwrap()
    }
}

#[test]
fn undo_reverts_the_newest_run_and_the_next_preview_shows_the_redo() {
    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    fs::write(left.join("edit.txt"), b"v1").unwrap();
    fs::write(left.join("gone.txt"), b"g").unwrap();
    fs::write(left.join("keep.txt"), b"k").unwrap();
    pair.sync();
    let before = pair.base.clone();

    fs::write(left.join("edit.txt"), b"version 2").unwrap();
    fs::remove_file(left.join("gone.txt")).unwrap();
    fs::write(left.join("new.txt"), b"n").unwrap();
    fs::create_dir(left.join("folder")).unwrap();
    fs::write(left.join("folder/x.txt"), b"x").unwrap();
    let report = pair.ran(Approval::new(0, 1), at(1));
    assert_eq!((report.copied(), report.archived()), (4, 1));

    let undo = pair.undo_at(at(2));
    assert!(undo.not_undone().is_empty(), "{:?}", undo.not_undone());
    assert_eq!((undo.removed(), undo.restored(), undo.beside()), (3, 2, 0));
    assert_eq!(fs::read(right.join("edit.txt")).unwrap(), b"v1");
    assert_eq!(fs::read(right.join("gone.txt")).unwrap(), b"g");
    assert!(!right.join("new.txt").exists());
    assert!(!right.join("folder").exists());
    // What the run created waits in the undo's own archive.
    let archive = right.join(".foldover/archive").join(undo.archive());
    assert_eq!(fs::read(archive.join("new.txt")).unwrap(), b"n");
    assert_eq!(fs::read(archive.join("folder/x.txt")).unwrap(), b"x");

    // The base is back to before the run, and the journal is spent.
    assert_eq!(pair.base, before);
    assert!(pair.journal.is_none());
    // Syncing again would redo exactly what was undone.
    assert_eq!(pair.prepare().preview().items().len(), 5);
}

#[test]
fn undo_leaves_entries_changed_since_the_run_and_says_why() {
    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    fs::write(left.join("keep.txt"), b"k").unwrap();
    pair.sync();
    fs::write(left.join("new.txt"), b"n").unwrap();
    pair.ran(Approval::default(), at(1));
    fs::write(right.join("new.txt"), b"edited in B").unwrap();

    let undo = pair.undo_at(at(2));
    assert_eq!(undo.removed(), 0);
    assert_eq!(undo.not_undone().len(), 1);
    assert_eq!(undo.not_undone()[0].path(), &path(b"new.txt"));
    assert!(undo.not_undone()[0].message().starts_with("Changed since"));
    assert_eq!(fs::read(right.join("new.txt")).unwrap(), b"edited in B");
    assert!(pair.journal.is_none());
}

#[test]
fn undo_puts_a_removed_folder_back_with_its_files() {
    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    fs::write(left.join("keep.txt"), b"k").unwrap();
    fs::create_dir(left.join("dir")).unwrap();
    fs::write(left.join("dir/a"), b"a").unwrap();
    fs::write(left.join("dir/b"), b"b").unwrap();
    pair.sync();
    let before = pair.base.clone();
    fs::remove_dir_all(left.join("dir")).unwrap();
    assert_eq!(pair.ran(Approval::new(0, 3), at(1)).archived(), 3);
    assert!(!right.join("dir").exists());

    let undo = pair.undo_at(at(2));
    assert!(undo.not_undone().is_empty(), "{:?}", undo.not_undone());
    assert_eq!(undo.restored(), 3);
    assert_eq!(fs::read(right.join("dir/a")).unwrap(), b"a");
    assert_eq!(fs::read(right.join("dir/b")).unwrap(), b"b");
    assert_eq!(pair.base, before);
}

#[test]
fn undo_keeps_the_newer_version_when_the_earlier_one_left_the_archive() {
    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    fs::write(left.join("edit.txt"), b"v1").unwrap();
    pair.sync();
    fs::write(left.join("edit.txt"), b"version 2").unwrap();
    let report = pair.ran(Approval::default(), at(1));
    let archived = right
        .join(".foldover/archive")
        .join(report.archive())
        .join("edit.txt");
    fs::remove_file(archived).unwrap();

    let undo = pair.undo_at(at(2));
    assert_eq!(undo.restored(), 0);
    assert_eq!(
        undo.not_undone()[0].message(),
        "The earlier version is no longer in the archive"
    );
    assert_eq!(fs::read(right.join("edit.txt")).unwrap(), b"version 2");
}

/// Review: base restores committed after the reverts could be lost to a
/// crash, leaving a base that makes the next sync propagate the undo.
/// They now commit first; a crash right after that leaves both sides as
/// the run left them under their pre-run base, which plans no writes.
#[test]
fn a_crash_after_an_undo_commits_its_base_plans_no_writes_and_undo_can_retry() {
    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    fs::write(left.join("edit.txt"), b"v1").unwrap();
    fs::write(left.join("gone.txt"), b"g").unwrap();
    fs::write(left.join("keep.txt"), b"k").unwrap();
    pair.sync();
    fs::write(left.join("edit.txt"), b"version 2").unwrap();
    fs::remove_file(left.join("gone.txt")).unwrap();
    fs::write(left.join("new.txt"), b"n").unwrap();
    pair.ran(Approval::new(0, 1), at(1));

    let journal = pair.journal.clone().unwrap();
    let prepared = pair.prepare();
    let mut store = pair.store();
    let error = prepared
        .undo(
            journal,
            at(2),
            &mut |planned, commit| {
                let undoing = matches!(commit, Commit::Undoing { .. });
                store.apply(planned, commit)?;
                if undoing {
                    // The agent dies right after the base commit.
                    return Err(crate::Error::StaleRevision);
                }
                Ok(())
            },
            &mut proceed,
        )
        .unwrap_err();
    assert!(matches!(error, RunError::Commit(_)), "{error:?}");
    assert_eq!(fs::read(right.join("edit.txt")).unwrap(), b"version 2");
    assert!(right.join("new.txt").exists() && !right.join("gone.txt").exists());
    assert!(pair.prepare().preview().items().is_empty());

    let undo = pair.undo_at(at(3));
    assert!(undo.not_undone().is_empty(), "{:?}", undo.not_undone());
    assert_eq!((undo.removed(), undo.restored()), (1, 2));
    assert_eq!(fs::read(right.join("edit.txt")).unwrap(), b"v1");
}

/// Review: a retried undo planned every entry again, so what a cancelled
/// attempt had already put back was listed as no longer archived.
#[test]
fn undoing_again_after_a_cancel_picks_up_only_what_is_left() {
    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    let count = COMMIT_BATCH + 1;
    fs::write(left.join("keep.txt"), b"k").unwrap();
    for index in 0..count {
        fs::write(left.join(format!("{index}.txt")), b"x").unwrap();
    }
    pair.sync();
    let before = pair.base.clone();
    for index in 0..count {
        fs::remove_file(left.join(format!("{index}.txt"))).unwrap();
    }
    pair.ran(Approval::new(0, count as u64), at(1));

    // Cancelled once the first batch is back.
    let journal = pair.journal.clone().unwrap();
    let prepared = pair.prepare();
    let mut store = pair.store();
    let mut checks = 0;
    let undo = prepared
        .undo(
            journal,
            at(2),
            &mut |planned, commit| store.apply(planned, commit),
            &mut || {
                checks += 1;
                if checks > 1 {
                    ScanControl::Cancel
                } else {
                    ScanControl::Continue
                }
            },
        )
        .unwrap();
    assert_eq!(undo.completion(), Completion::Cancelled);
    assert_eq!(undo.restored(), COMMIT_BATCH as u64);

    let undo = pair.undo_at(at(3));
    assert!(undo.not_undone().is_empty(), "{:?}", undo.not_undone());
    assert_eq!((undo.restored(), undo.beside()), (1, 0));
    // Every file, keep.txt and .foldover.
    assert_eq!(fs::read_dir(&right).unwrap().count(), count + 2);
    assert_eq!(pair.base, before);
    assert!(pair.journal.is_none());
}

impl Fixture {
    /// The versions a fresh preview shows for `name`.
    fn previewed(&self, name: &[u8]) -> (Option<EntryVersion>, Option<EntryVersion>) {
        self.prepare()
            .preview()
            .items()
            .iter()
            .find(|item| item.path() == &path(name))
            .unwrap()
            .versions()
    }

    fn keep(&mut self, side: Side, name: &[u8]) {
        let (left, right) = self.previewed(name);
        self.resolutions.insert(
            path(name),
            foldover_types::Resolution::new(side, left, right),
        );
    }
}

/// PLAN.md §3.8 per-item override: keeping the side a change would have
/// overwritten copies it the other way instead.
#[test]
fn keeping_the_target_side_copies_a_change_the_other_way() {
    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    fs::write(left.join("deleted"), b"d").unwrap();
    fs::write(left.join("edited"), b"v1").unwrap();
    fs::write(left.join("other"), b"o").unwrap();
    pair.sync();
    fs::remove_file(left.join("deleted")).unwrap();
    fs::write(left.join("edited"), b"version 2").unwrap();

    pair.keep(Side::Right, b"deleted");
    pair.keep(Side::Right, b"edited");
    let report = pair.sync();
    assert_eq!((report.copied(), report.archived()), (2, 0));
    // The deletion is undone in A instead of propagated to B.
    assert_eq!(fs::read(left.join("deleted")).unwrap(), b"d");
    // B's version wins in A; A's edit is in A's archive.
    assert_eq!(fs::read(left.join("edited")).unwrap(), b"v1");
    let archive = left.join(".foldover/archive/1970-01-01T00-00-00.000Z");
    assert_eq!(fs::read(archive.join("edited")).unwrap(), b"version 2");
    assert_eq!(fs::read(right.join("edited")).unwrap(), b"v1");
    assert!(pair.prepare().preview().items().is_empty());
}

#[test]
fn keeping_a_folder_a_deletion_would_archive_restores_it_with_its_files() {
    let mut pair = fixture(SyncMode::Sync);
    let left = pair.left.path().to_owned();
    fs::write(left.join("keep"), b"k").unwrap();
    fs::create_dir(left.join("dir")).unwrap();
    fs::write(left.join("dir/a"), b"a").unwrap();
    pair.sync();
    fs::remove_dir_all(left.join("dir")).unwrap();

    pair.keep(Side::Right, b"dir");
    let report = pair.sync();
    assert_eq!((report.copied(), report.archived()), (2, 0));
    assert_eq!(fs::read(left.join("dir/a")).unwrap(), b"a");
    assert!(pair.prepare().preview().items().is_empty());
}

#[test]
fn a_skipped_change_and_everything_inside_it_waits_for_another_run() {
    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    fs::write(left.join("keep"), b"k").unwrap();
    pair.sync();
    fs::write(left.join("later"), b"l").unwrap();
    fs::write(left.join("now"), b"n").unwrap();
    fs::create_dir(left.join("folder")).unwrap();
    fs::write(left.join("folder/inner"), b"i").unwrap();

    for name in [b"later".as_slice(), b"folder"] {
        let (left, right) = pair.previewed(name);
        pair.skips
            .insert(path(name), foldover_types::Skip::new(left, right));
    }
    let preview = pair.prepare().preview();
    assert!(
        preview
            .items()
            .iter()
            .filter(|item| item.path() != &path(b"now"))
            .all(|item| item.action() == Action::KeepTarget),
        "{preview:?}"
    );
    assert_eq!(pair.sync().copied(), 1);
    assert!(right.join("now").exists());
    assert!(!right.join("later").exists() && !right.join("folder").exists());

    // A skip covers what the person saw; an edit since makes it lapse.
    fs::write(left.join("later"), b"edited").unwrap();
    assert_eq!(pair.sync().copied(), 1);
    assert!(right.join("later").exists());

    // Without the skip the rest copies on the next run.
    pair.skips.clear();
    assert_eq!(pair.sync().copied(), 2);
    assert!(right.join("folder/inner").exists());
}

/// Counts what a run reports, as the agent's status does.
#[derive(Default)]
struct Counts {
    listed: std::cell::Cell<u64>,
    checked: std::cell::Cell<u64>,
    planned: std::cell::Cell<(u64, u64)>,
    applied: std::cell::Cell<u64>,
    copied: std::cell::Cell<u64>,
}

impl RunProgress for Counts {
    fn waiting(&self, _: Option<&str>) {}

    fn observed(&self, _: Side, step: ObserveStep) {
        let counter = match step {
            ObserveStep::Listed => &self.listed,
            ObserveStep::Checked => &self.checked,
        };
        counter.set(counter.get() + 1);
    }

    fn planned(&self, changes: u64, bytes: u64) {
        self.planned.set((changes, bytes));
    }

    fn applied(&self, bytes: u64) {
        self.applied.set(self.applied.get() + 1);
        self.copied.set(self.copied.get() + bytes);
    }
}

#[test]
fn runs_report_what_they_observe_plan_and_apply() {
    let pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("a"), b"aaa").unwrap();
    fs::create_dir(pair.left.path().join("dir")).unwrap();
    fs::write(pair.left.path().join("dir/b"), b"bbbbb").unwrap();
    fs::write(pair.right.path().join("c"), b"c").unwrap();

    let counts = Counts::default();
    let prepared = PairRun::prepare(pair.inputs(), &mut proceed, &counts).unwrap();
    assert_eq!((counts.listed.get(), counts.checked.get()), (4, 4));

    let outcome = prepared
        .execute(
            Approval::default(),
            RunContext::Reviewed,
            STARTED,
            &mut |_, _| Ok(()),
            &mut proceed,
            &counts,
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Ran(_)));
    // Four changes: a, dir and dir/b to B; c to A. Folders copy no bytes.
    assert_eq!(counts.planned.get(), (4, 9));
    assert_eq!((counts.applied.get(), counts.copied.get()), (4, 9));
}

/// Review: with only one identity recorded (the other root was changed),
/// the recorded side must still be checked, not adopted, before writes.
#[test]
fn a_recorded_side_must_still_match_when_the_other_side_is_new() {
    let mut pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("file"), b"x").unwrap();
    pair.sync();
    let left = pair.identities.side(Side::Left).unwrap();
    pair.identities = RecordedIdentities::new(Some(left), None);
    pair.right = tempfile::tempdir().unwrap();

    let prepared = pair.prepare();
    // The left disk is swapped while the scan runs.
    let other = EndpointId::from_bytes([9; 16]);
    fs::write(pair.left.path().join(".foldover/id"), format!("{other}\n")).unwrap();
    let error = prepared
        .execute(
            Approval::default(),
            RunContext::Reviewed,
            STARTED,
            &mut |_, commit| {
                assert!(!matches!(commit, Commit::Identities { .. }));
                Ok(())
            },
            &mut proceed,
            &NoProgress,
        )
        .unwrap_err();
    assert!(
        matches!(error, RunError::IdentityChanged { side: Side::Left }),
        "{error:?}"
    );
    assert_eq!(fs::read_dir(pair.right.path()).unwrap().count(), 0);
}
