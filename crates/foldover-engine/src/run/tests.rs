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
    filters: Vec<String>,
    size_limit: Option<u64>,
    age_limit: Option<u32>,
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
        filters: Vec::new(),
        size_limit: None,
        age_limit: None,
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

    fn try_prepare(&self) -> Result<PairRun, RunError> {
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
        .with_options(foldover_types::PairOptions::default().with_filters(filters));
        let pair =
            PairConfig::from_stored(PairId::from_bytes([1; 16]).unwrap(), self.revision, draft)
                .unwrap();
        let inputs = RunInputs::new(
            pair,
            self.base.clone(),
            self.identities,
            self.resolutions.clone(),
        );
        PairRun::prepare(inputs, &mut proceed)
    }

    /// Run with an in-memory base that applies commits like the state store.
    fn run(&mut self, approval: Approval, context: RunContext) -> RunOutcome {
        let prepared = self.prepare();
        let revision = self.revision;
        let base = &mut self.base;
        let identities = &mut self.identities;
        prepared
            .execute(
                approval,
                context,
                STARTED,
                &mut |planned, commit| {
                    if planned != revision {
                        return Err(crate::Error::StaleRevision);
                    }
                    let updates = match commit {
                        Commit::Identities { left, right } => {
                            *identities = RecordedIdentities::new(Some(left), Some(right));
                            return Ok(());
                        }
                        Commit::Base(updates) => updates,
                    };
                    for update in updates {
                        match update {
                            BaseUpdate::Record(path, entry) => {
                                base.insert(path.clone(), *entry);
                            }
                            BaseUpdate::Forget(path) => {
                                base.remove(path);
                            }
                        }
                    }
                    Ok(())
                },
                &mut proceed,
            )
            .unwrap()
    }

    fn sync(&mut self) -> RunReport {
        match self.run(Approval::default(), RunContext::Interactive) {
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
    for index in 0..60 {
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
    assert_eq!(fs::read_dir(pair.left.path()).unwrap().count(), 61);
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
    let report = mirror.sync();
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
        RunContext::Interactive,
        STARTED,
        &mut |planned, _| {
            if planned == pair.revision {
                Ok(())
            } else {
                Err(crate::Error::StaleRevision)
            }
        },
        &mut proceed,
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
            RunContext::Interactive,
            STARTED,
            &mut |_, commit| {
                if let Commit::Base(updates) = commit {
                    for update in updates {
                        if let BaseUpdate::Record(path, entry) = update {
                            base.insert(path.clone(), *entry);
                        }
                    }
                }
                Ok(())
            },
            &mut proceed,
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
            RunContext::Interactive,
            started,
            &mut |_, _| Ok(()),
            &mut proceed,
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
    let second = PairRun::prepare(inputs, &mut proceed).unwrap();
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
