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
}

fn fixture(mode: SyncMode) -> Fixture {
    Fixture {
        left: tempfile::tempdir().unwrap(),
        right: tempfile::tempdir().unwrap(),
        mode,
        revision: 1,
        base: BTreeMap::new(),
    }
}

fn root(path: &Path) -> LocalRoot {
    LocalRoot::from_bytes(path.as_os_str().as_bytes().to_vec()).unwrap()
}

fn proceed() -> ScanControl {
    ScanControl::Continue
}

fn path(bytes: &[u8]) -> SyncPath {
    SyncPath::try_from(bytes).unwrap()
}

impl Fixture {
    fn prepare(&self) -> PairRun {
        let draft = PairDraft::new(
            "pair".into(),
            root(self.left.path()),
            root(self.right.path()),
            self.mode,
        )
        .unwrap();
        let pair =
            PairConfig::from_stored(PairId::from_bytes([1; 16]).unwrap(), self.revision, draft)
                .unwrap();
        PairRun::prepare(pair, &self.base, &mut proceed).unwrap()
    }

    /// Run with an in-memory base that applies commits like the state store.
    fn run(&mut self, approval: Approval, context: RunContext) -> RunOutcome {
        let prepared = self.prepare();
        let revision = self.revision;
        let base = &mut self.base;
        prepared
            .execute(
                approval,
                context,
                "run",
                &mut |planned, updates| {
                    if planned != revision {
                        return Err(crate::Error::StaleRevision);
                    }
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

    let report = pair.sync();
    assert_eq!((report.copied(), report.archived()), (1, 1));
    assert_eq!(fs::read(pair.right.path().join("edit")).unwrap(), b"v2");
    assert_eq!(
        fs::read(pair.right.path().join(".foldover/archive/run/edit")).unwrap(),
        b"v1"
    );
    assert!(!pair.left.path().join("delete").exists());
    assert_eq!(
        fs::read(pair.left.path().join(".foldover/archive/run/delete")).unwrap(),
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
    assert_eq!(fs::read_dir(pair.left.path()).unwrap().count(), 60);
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
            .join(".foldover/archive/run/extra")
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
        "run",
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
            "run",
            &mut |_, updates| {
                for update in updates {
                    if let BaseUpdate::Record(path, entry) = update {
                        base.insert(path.clone(), *entry);
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
                .join(".foldover/archive/run/.foldover-tmp-left")
        )
        .unwrap(),
        b"displaced"
    );
}
