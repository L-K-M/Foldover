use std::fs;
use std::os::unix::ffi::OsStrExt;

use super::*;
use crate::{Engine, StateLocation};
use foldover_state::{BaseUpdate, JournalAction, JournalEntry};
use foldover_types::{
    BaselineEntry, Digest, EntryContent, EntryVersion, LocalRoot, PairDraft, PairId, SyncPath,
};

struct Fixture {
    _state: tempfile::TempDir,
    _source: tempfile::TempDir,
    target: tempfile::TempDir,
    engine: Engine,
    pair: PairId,
}
fn fixture() -> Fixture {
    let state = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(&StateLocation::Directory(state.path().to_owned())).unwrap();
    let root = |path: &std::path::Path| {
        LocalRoot::from_bytes(path.as_os_str().as_bytes().to_vec()).unwrap()
    };
    let pair = engine
        .create_pair(
            &PairDraft::new(
                "Backup".into(),
                root(source.path()),
                root(target.path()),
                SyncMode::Backup { source: Side::Left },
            )
            .unwrap(),
        )
        .unwrap();
    Fixture {
        _state: state,
        _source: source,
        target,
        engine,
        pair: pair.id(),
    }
}
fn path(value: &[u8]) -> SyncPath {
    SyncPath::try_from(value).unwrap()
}
fn version() -> EntryVersion {
    EntryVersion::new(
        EntryContent::File(Digest::from_bytes([2; 32])),
        Digest::from_bytes([3; 32]),
    )
}
fn baseline() -> BaselineEntry {
    BaselineEntry::new(version(), version()).unwrap()
}
fn journal(seq: u64) -> JournalEntry {
    JournalEntry {
        seq,
        path: path(b"file"),
        side: Side::Right,
        action: JournalAction::Create(version()),
        before: None,
        moved_from: None,
    }
}

fn migrate(fixture: &mut Fixture) -> TargetRun {
    fixture.engine.begin_target_catalog(fixture.pair).unwrap();
    let pending = fixture.engine.capture_run_inputs(fixture.pair).unwrap();
    let mut target = TargetRun::open(&pending, &mut || ScanControl::Continue)
        .unwrap()
        .unwrap();
    target
        .decode(pending, &mut || ScanControl::Continue)
        .unwrap();
    target
        .acknowledge(&mut |expected, next, spent| {
            fixture
                .engine
                .acknowledge_target_catalog(fixture.pair, 1, expected, next, spent)
        })
        .unwrap();
    target
}

#[test]
fn migration_retains_base_and_undo_then_removes_local_bulk_rows() {
    let mut fixture = fixture();
    fixture
        .engine
        .commit_run(
            fixture.pair,
            1,
            Commit::Base {
                run: "legacy-run",
                updates: &[BaseUpdate::Record(path(b"file"), baseline())],
                journal: &[journal(7)],
            },
        )
        .unwrap();
    let target = migrate(&mut fixture);
    assert_eq!(target.journal().unwrap().unwrap().entries, [journal(7)]);
    while fixture
        .engine
        .purge_target_catalog_rows(fixture.pair, 1)
        .unwrap()
        == crate::Purge::Pending
    {}
    assert!(fixture.engine.journal(fixture.pair).unwrap().is_none());
    assert!(
        fixture.engine.run_inputs(fixture.pair).is_err(),
        "cannot silently read the emptied local baseline"
    );
    let checkpoint = target.catalog.checkpoint.clone();
    drop(target);
    let pending = fixture.engine.capture_run_inputs(fixture.pair).unwrap();
    let reopened = TargetRun::open(&pending, &mut || ScanControl::Continue)
        .unwrap()
        .unwrap();
    assert_eq!(reopened.catalog.checkpoint, checkpoint);
    let mut rows = Vec::new();
    reopened
        .catalog
        .visit_base(&reopened.store, &mut |p, b| rows.push((p, b)))
        .unwrap();
    assert_eq!(rows, [(path(b"file"), baseline())]);
    assert_eq!(reopened.journal().unwrap().unwrap().entries, [journal(7)]);
}

#[test]
fn published_checkpoint_without_local_ack_is_recovered_exactly_once() {
    let mut fixture = fixture();
    let mut target = migrate(&mut fixture);
    let old = target.catalog.checkpoint.clone();
    let commit = || Commit::Base {
        run: "run",
        updates: &[],
        journal: &[],
    };
    let mut fail = |_: &Checkpoint, _: &Checkpoint, _: &[SyncPath]| Err(Error::StaleRevision);
    assert!(target.commit(commit(), &mut fail).is_err());
    let published = target.catalog.checkpoint.clone();
    assert_eq!(published.generation, old.generation + 1);
    assert!(
        target.commit(commit(), &mut fail).is_err(),
        "executor retries must not publish another successor"
    );
    drop(target);
    let pending = fixture.engine.capture_run_inputs(fixture.pair).unwrap();
    let mut target = TargetRun::open(&pending, &mut || ScanControl::Continue)
        .unwrap()
        .unwrap();
    assert_eq!(target.catalog.checkpoint, published);
    target
        .acknowledge(&mut |expected, next, spent| {
            fixture
                .engine
                .acknowledge_target_catalog(fixture.pair, 1, expected, next, spent)
        })
        .unwrap();
}

#[test]
fn lost_acknowledged_manifest_and_block_fail_closed() {
    let mut fixture = fixture();
    fixture
        .engine
        .commit_run(
            fixture.pair,
            1,
            Commit::Base {
                run: "run",
                updates: &[BaseUpdate::Record(path(b"file"), baseline())],
                journal: &[],
            },
        )
        .unwrap();
    let target = migrate(&mut fixture);
    let name = target.catalog.checkpoint.filename();
    let block = target.catalog.base.files()[0].clone();
    let directory = fixture
        .target
        .path()
        .join(format!(".foldover/state/{}", fixture.pair));
    drop(target);
    let bytes = fs::read(directory.join(&name)).unwrap();
    fs::remove_file(directory.join(&name)).unwrap();
    let pending = fixture.engine.capture_run_inputs(fixture.pair).unwrap();
    assert!(TargetRun::open(&pending, &mut || ScanControl::Continue).is_err());
    fs::write(directory.join(&name), bytes).unwrap();
    fs::remove_file(directory.join(block)).unwrap();
    let target = TargetRun::open(&pending, &mut || ScanControl::Continue)
        .unwrap()
        .unwrap();
    assert!(
        target
            .decode(pending, &mut || ScanControl::Continue)
            .is_err()
    );
}

#[test]
fn undo_batches_update_target_base_and_journal_atomically() {
    let mut fixture = fixture();
    let mut target = migrate(&mut fixture);
    let mut ack = |expected: &_, next: &_, spent: &[_]| {
        fixture
            .engine
            .acknowledge_target_catalog(fixture.pair, 1, expected, next, spent)
    };
    target
        .commit(
            Commit::Base {
                run: "run",
                updates: &[BaseUpdate::Record(path(b"file"), baseline())],
                journal: &[journal(1), journal(2)],
            },
            &mut ack,
        )
        .unwrap();
    target
        .commit(
            Commit::Undid {
                run: "run",
                seqs: &[1],
            },
            &mut ack,
        )
        .unwrap();
    assert_eq!(target.journal().unwrap().unwrap().entries, [journal(2)]);
    target
        .commit(
            Commit::Undone {
                run: "run",
                updates: &[BaseUpdate::Forget(path(b"file"))],
            },
            &mut ack,
        )
        .unwrap();
    assert!(target.journal().unwrap().is_none());
    let mut count = 0;
    target
        .catalog
        .visit_base(&target.store, &mut |_, _| count += 1)
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn config_reset_uses_new_epoch_and_does_not_adopt_old_target_state() {
    let mut fixture = fixture();
    let target = migrate(&mut fixture);
    let epoch = target.catalog.checkpoint.epoch;
    drop(target);
    let pair = fixture.engine.pair(fixture.pair).unwrap();
    let draft = PairDraft::new(
        "Mirror".into(),
        pair.draft().left().clone(),
        pair.draft().right().clone(),
        SyncMode::Mirror { source: Side::Left },
    )
    .unwrap();
    fixture.engine.update_pair(fixture.pair, 1, &draft).unwrap();
    fixture.engine.begin_target_catalog(fixture.pair).unwrap();
    let pending = fixture.engine.capture_run_inputs(fixture.pair).unwrap();
    let target = TargetRun::open(&pending, &mut || ScanControl::Continue)
        .unwrap()
        .unwrap();
    assert_ne!(target.catalog.checkpoint.epoch, epoch);
    assert_eq!(target.catalog.checkpoint.generation, 1);
}

#[test]
fn undo_can_finish_more_than_one_table_batch_of_refilled_folders() {
    let mut fixture = fixture();
    let mut target = migrate(&mut fixture);
    let mut ack = |expected: &_, next: &_, spent: &[_]| {
        fixture
            .engine
            .acknowledge_target_catalog(fixture.pair, 1, expected, next, spent)
    };
    target
        .commit(
            Commit::Base {
                run: "run",
                updates: &[],
                journal: &[journal(1)],
            },
            &mut ack,
        )
        .unwrap();
    let updates: Vec<_> = (0..5000)
        .map(|i| BaseUpdate::Record(path(format!("folder-{i:05}").as_bytes()), baseline()))
        .collect();
    target
        .commit(
            Commit::Undone {
                run: "run",
                updates: &updates,
            },
            &mut ack,
        )
        .unwrap();
    assert!(target.journal().unwrap().is_none());
    let mut count = 0;
    target
        .catalog
        .visit_base(&target.store, &mut |_, _| count += 1)
        .unwrap();
    assert_eq!(count, 5000);
}
