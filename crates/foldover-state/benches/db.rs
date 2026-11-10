//! Criterion benchmark for the state-store hot path (PLAN.md §5.2):
//! committing a 100_000-entry base through `PairStore::update_base`
//! and reading it back through `PairStore::base`, the persistence
//! every run commits through.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use foldover_state::{
    BaseUpdate, JournalAction, JournalEntry, JournalUpdate, PairStore, SnapshotEntry, StateLocation,
};
use foldover_types::{
    BaselineEntry, Digest, EntryContent, EntryVersion, LocalRoot, PairConfig, PairDraft, Side,
    SyncMode, SyncPath,
};

const DIRECTORIES: usize = 1_000;
const FILES_PER_DIRECTORY: usize = 99;
const EXPECTED_ENTRIES: usize = DIRECTORIES + DIRECTORIES * FILES_PER_DIRECTORY;
/// The engine's apply-phase commit batch (`COMMIT_BATCH`,
/// crates/foldover-engine/src/run.rs:52).
const COMMIT_BATCH: usize = 256;

fn path(bytes: &[u8]) -> SyncPath {
    SyncPath::try_from(bytes).unwrap()
}

fn digest(seed: u32, salt: u8) -> Digest {
    let mut bytes = [0u8; Digest::BYTE_LEN];
    bytes[..4].copy_from_slice(&seed.to_be_bytes());
    bytes[Digest::BYTE_LEN - 1] = salt;
    Digest::from_bytes(bytes)
}

fn entry(seed: u32) -> BaselineEntry {
    let version = EntryVersion::new(EntryContent::File(digest(seed, 0)), digest(seed, 0xee));
    BaselineEntry::new(version, version).unwrap()
}

fn updates() -> Vec<BaseUpdate> {
    let mut updates = Vec::with_capacity(EXPECTED_ENTRIES);
    for folder in 0..DIRECTORIES {
        let name = format!("d-{folder:04}");
        updates.push(BaseUpdate::Record(
            path(name.as_bytes()),
            entry((EXPECTED_ENTRIES + folder) as u32),
        ));
        for file in 0..FILES_PER_DIRECTORY {
            let seed = (folder * FILES_PER_DIRECTORY + file) as u32;
            updates.push(BaseUpdate::Record(
                path(format!("{name}/f-{file:02}").as_bytes()),
                entry(seed),
            ));
        }
    }
    updates
}

/// One write's base update and the journal entry a run records with it.
fn write(updates: &mut Vec<BaseUpdate>, journal: &mut Vec<JournalEntry>, bytes: &[u8], seed: u32) {
    let entry = entry(seed);
    let seq = updates.len() as u64;
    updates.push(BaseUpdate::Record(path(bytes), entry));
    journal.push(JournalEntry {
        seq,
        path: path(bytes),
        side: Side::Left,
        action: JournalAction::Replace(entry.left()),
        before: Some(entry),
        moved_from: None,
    });
}

/// The same 100_000 paths `updates` builds, each with the journal entry
/// a run records for writing it.
fn writes() -> (Vec<BaseUpdate>, Vec<JournalEntry>) {
    let mut updates = Vec::with_capacity(EXPECTED_ENTRIES);
    let mut journal = Vec::with_capacity(EXPECTED_ENTRIES);
    for folder in 0..DIRECTORIES {
        let name = format!("d-{folder:04}");
        write(
            &mut updates,
            &mut journal,
            name.as_bytes(),
            (EXPECTED_ENTRIES + folder) as u32,
        );
        for file in 0..FILES_PER_DIRECTORY {
            let seed = (folder * FILES_PER_DIRECTORY + file) as u32;
            write(
                &mut updates,
                &mut journal,
                format!("{name}/f-{file:02}").as_bytes(),
                seed,
            );
        }
    }
    (updates, journal)
}

/// Commit one whole run: `updates` and `journal` in `COMMIT_BATCH`
/// transactions, the base updates and journal entries of each batch
/// together. `run` names the run, as a real archive folder does.
fn commit_run(
    store: &mut PairStore,
    pair: &PairConfig,
    updates: &[BaseUpdate],
    journal: &[JournalEntry],
    run: &str,
) {
    for (index, batch) in updates.chunks(COMMIT_BATCH).enumerate() {
        let start = index * COMMIT_BATCH;
        store
            .update_base(
                pair.id(),
                pair.revision(),
                batch,
                JournalUpdate::Record {
                    run,
                    entries: &journal[start..start + batch.len()],
                },
            )
            .unwrap();
    }
}

/// A run's archive-folder name; every run has its own.
fn run_name(run: u64) -> String {
    format!("2026-10-20T10-00-00.000Z-{run:016}")
}

/// An exclusive store in a fresh 0o700 directory, with one pair; the
/// temp directory must outlive the benchmark that uses it.
fn store() -> (tempfile::TempDir, PairStore, foldover_types::PairConfig) {
    let directory = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let location = StateLocation::Directory(directory.path().to_owned());
    let mut store = PairStore::open(&location).unwrap();
    let draft = PairDraft::new(
        "bench".into(),
        LocalRoot::from_bytes(b"/left".to_vec()).unwrap(),
        LocalRoot::from_bytes(b"/right".to_vec()).unwrap(),
        SyncMode::Sync,
    )
    .unwrap();
    let pair = store.create(&draft).unwrap();
    (directory, store, pair)
}

fn db_commit_100k(c: &mut Criterion) {
    let (_directory, mut store, pair) = store();
    let updates = updates();
    // One warm commit, so every measured iteration is a steady-state
    // upsert over the same 100_000 paths rather than a first insert.
    store
        .update_base(pair.id(), pair.revision(), &updates, JournalUpdate::None)
        .unwrap();
    c.bench_function("db_commit_100k", |b| {
        b.iter(|| {
            store
                .update_base(pair.id(), pair.revision(), &updates, JournalUpdate::None)
                .unwrap();
        })
    });
}

/// The real apply-phase shape: a run's 100_000 writes committed in
/// `COMMIT_BATCH` transactions, each batch carrying its journal
/// entries. Every iteration is one whole run under a fresh run id, so
/// the first batch replaces the previous run's journal. A per-batch
/// journal cleanup that scans the run's own entries instead of seeking
/// the primary key shows up here as a quadratic median.
fn db_commit_batches_100k(c: &mut Criterion) {
    let (_directory, mut store, pair) = store();
    let (updates, journal) = writes();
    // One warm run, so every measured iteration is a steady-state
    // rewrite of the same 100_000 paths rather than a first insert.
    commit_run(&mut store, &pair, &updates, &journal, &run_name(0));
    let mut run = 1;
    c.bench_function("db_commit_batches_100k", |b| {
        b.iter(|| {
            commit_run(&mut store, &pair, &updates, &journal, &run_name(run));
            run += 1;
        })
    });
}

/// The same paths `updates` builds, as one side's stored observation;
/// `salt` rewrites every fingerprint without touching the paths.
fn snapshots(salt: u8) -> Vec<SnapshotEntry> {
    let mut entries = Vec::with_capacity(EXPECTED_ENTRIES);
    for folder in 0..DIRECTORIES {
        let name = format!("d-{folder:04}");
        entries.push(snapshot_entry(
            name.as_bytes(),
            (EXPECTED_ENTRIES + folder) as u32,
            salt,
        ));
        for file in 0..FILES_PER_DIRECTORY {
            let seed = (folder * FILES_PER_DIRECTORY + file) as u32;
            entries.push(snapshot_entry(
                format!("{name}/f-{file:02}").as_bytes(),
                seed,
                salt,
            ));
        }
    }
    entries
}

fn snapshot_entry(bytes: &[u8], seed: u32, salt: u8) -> SnapshotEntry {
    SnapshotEntry {
        path: path(bytes),
        fingerprint: digest(seed, salt).as_bytes().to_vec(),
        version: entry(seed).left(),
    }
}

fn db_snapshot_unchanged_100k(c: &mut Criterion) {
    let (_directory, mut store, pair) = store();
    let entries = snapshots(0);
    // One warm store, so every measured iteration is the steady state:
    // the same 100_000 observations stored again after a run.
    store
        .replace_snapshot(pair.id(), pair.revision(), Side::Left, &entries)
        .unwrap();
    c.bench_function("db_snapshot_unchanged_100k", |b| {
        b.iter(|| {
            store
                .replace_snapshot(pair.id(), pair.revision(), Side::Left, &entries)
                .unwrap();
        })
    });
}

fn db_snapshot_rewrite_100k(c: &mut Criterion) {
    let (_directory, mut store, pair) = store();
    // Alternating sets with every fingerprint different makes each
    // iteration update all 100_000 rows, the worst case for the merge.
    let sets = [snapshots(0), snapshots(1)];
    let mut turn = 0;
    store
        .replace_snapshot(pair.id(), pair.revision(), Side::Left, &sets[0])
        .unwrap();
    c.bench_function("db_snapshot_rewrite_100k", |b| {
        b.iter(|| {
            turn = 1 - turn;
            store
                .replace_snapshot(pair.id(), pair.revision(), Side::Left, &sets[turn])
                .unwrap();
        })
    });
}

fn db_read_100k(c: &mut Criterion) {
    let (_directory, mut store, pair) = store();
    let updates = updates();
    store
        .update_base(pair.id(), pair.revision(), &updates, JournalUpdate::None)
        .unwrap();
    assert_eq!(store.base(pair.id()).unwrap().len(), EXPECTED_ENTRIES);
    c.bench_function("db_read_100k", |b| {
        b.iter(|| black_box(store.base(pair.id()).unwrap().len()))
    });
}

/// A store holding a 100_000-row base and both 100_000-entry snapshot
/// caches, the state a run's inputs are captured from.
fn populated() -> (tempfile::TempDir, PairStore, foldover_types::PairConfig) {
    let (directory, mut store, pair) = store();
    store
        .update_base(pair.id(), pair.revision(), &updates(), JournalUpdate::None)
        .unwrap();
    for side in [Side::Left, Side::Right] {
        store
            .replace_snapshot(pair.id(), pair.revision(), side, &snapshots(0))
            .unwrap();
    }
    (directory, store, pair)
}

/// The reads `Engine::run_inputs` did under the engine lock: decode the
/// base and both snapshots.
fn run_inputs_read_100k(c: &mut Criterion) {
    let (_directory, store, pair) = populated();
    c.bench_function("run_inputs_read_100k", |b| {
        b.iter(|| {
            black_box(store.base(pair.id()).unwrap().len());
            black_box(store.snapshot(pair.id(), Side::Left).unwrap().len());
            black_box(store.snapshot(pair.id(), Side::Right).unwrap().len());
        })
    });
}

/// The raw-row capture the engine performs under its lock now.
fn run_records_capture_100k(c: &mut Criterion) {
    let (_directory, store, pair) = populated();
    c.bench_function("run_records_capture_100k", |b| {
        b.iter(|| black_box(store.run_records(pair.id()).unwrap()))
    });
}

/// Capture plus the decode that moved outside the lock, the same work
/// the old wrapper did.
fn run_records_capture_decode_100k(c: &mut Criterion) {
    let (_directory, store, pair) = populated();
    c.bench_function("run_records_capture_decode_100k", |b| {
        b.iter(|| black_box(store.run_records(pair.id()).unwrap().decode().unwrap()))
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = db_commit_100k, db_commit_batches_100k, db_read_100k, db_snapshot_unchanged_100k, db_snapshot_rewrite_100k,
        run_inputs_read_100k, run_records_capture_100k, run_records_capture_decode_100k
}
criterion_main!(benches);
