//! Criterion benchmark for run-input capture and decode at 100_000 base
//! rows and 2 x 100_000 snapshot entries: the engine's mutex hold against
//! the decoding that moved outside it.

use std::hint::black_box;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;

use criterion::{Criterion, criterion_group, criterion_main};
use foldover_engine::{Commit, Engine};
use foldover_state::{BaseUpdate, SnapshotEntry, StateLocation};
use foldover_types::{
    BaselineEntry, Digest, EntryContent, EntryVersion, LocalRoot, PairDraft, Side, SyncMode,
    SyncPath,
};

const DIRECTORIES: usize = 1_000;
const FILES_PER_DIRECTORY: usize = 99;
const EXPECTED_ENTRIES: usize = DIRECTORIES + DIRECTORIES * FILES_PER_DIRECTORY;

fn path(bytes: &[u8]) -> SyncPath {
    SyncPath::try_from(bytes).unwrap()
}

fn digest(seed: u32, salt: u8) -> Digest {
    let mut bytes = [0u8; Digest::BYTE_LEN];
    bytes[..4].copy_from_slice(&seed.to_be_bytes());
    bytes[Digest::BYTE_LEN - 1] = salt;
    Digest::from_bytes(bytes)
}

fn file_version(seed: u32) -> EntryVersion {
    EntryVersion::new(EntryContent::File(digest(seed, 0)), digest(seed, 0xee))
}

fn base_updates() -> Vec<BaseUpdate> {
    let mut updates = Vec::with_capacity(EXPECTED_ENTRIES);
    for folder in 0..DIRECTORIES {
        let name = format!("d-{folder:04}");
        let version = file_version((EXPECTED_ENTRIES + folder) as u32);
        updates.push(BaseUpdate::Record(
            path(name.as_bytes()),
            BaselineEntry::new(version, version).unwrap(),
        ));
        for file in 0..FILES_PER_DIRECTORY {
            let seed = (folder * FILES_PER_DIRECTORY + file) as u32;
            let version = file_version(seed);
            updates.push(BaseUpdate::Record(
                path(format!("{name}/f-{file:02}").as_bytes()),
                BaselineEntry::new(version, version).unwrap(),
            ));
        }
    }
    updates
}

fn snapshot_entry(bytes: &[u8], seed: u32) -> SnapshotEntry {
    SnapshotEntry {
        path: path(bytes),
        fingerprint: digest(seed, 0).as_bytes().to_vec(),
        version: file_version(seed),
    }
}

fn snapshots() -> Vec<SnapshotEntry> {
    let mut entries = Vec::with_capacity(EXPECTED_ENTRIES);
    for folder in 0..DIRECTORIES {
        let name = format!("d-{folder:04}");
        entries.push(snapshot_entry(
            name.as_bytes(),
            (EXPECTED_ENTRIES + folder) as u32,
        ));
        for file in 0..FILES_PER_DIRECTORY {
            let seed = (folder * FILES_PER_DIRECTORY + file) as u32;
            entries.push(snapshot_entry(
                format!("{name}/f-{file:02}").as_bytes(),
                seed,
            ));
        }
    }
    entries
}

/// An engine holding a 100_000-row base and both 100_000-entry snapshot
/// caches, the state a run's inputs are captured from.
fn engine_with_inputs() -> (tempfile::TempDir, Engine, foldover_types::PairConfig) {
    let directory = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let left = directory.path().join("left");
    let right = directory.path().join("right");
    std::fs::create_dir(&left).unwrap();
    std::fs::create_dir(&right).unwrap();
    let location = StateLocation::Directory(directory.path().join("state"));
    let mut engine = Engine::open(&location).unwrap();
    let draft = PairDraft::new(
        "bench".into(),
        LocalRoot::from_bytes(left.as_os_str().as_bytes().to_vec()).unwrap(),
        LocalRoot::from_bytes(right.as_os_str().as_bytes().to_vec()).unwrap(),
        SyncMode::Sync,
    )
    .unwrap();
    let pair = engine.create_pair(&draft).unwrap();
    let updates = base_updates();
    engine
        .commit_run(
            pair.id(),
            pair.revision(),
            Commit::Base {
                run: "bench",
                updates: &updates,
                journal: &[],
            },
        )
        .unwrap();
    let entries = snapshots();
    engine
        .store_snapshot(pair.id(), pair.revision(), Side::Left, &entries)
        .unwrap();
    engine
        .store_snapshot(pair.id(), pair.revision(), Side::Right, &entries)
        .unwrap();
    (directory, engine, pair)
}

/// The wrapper's whole read and decode, the work the engine lock used to
/// hold.
fn run_inputs_100k(c: &mut Criterion) {
    let (_directory, engine, pair) = engine_with_inputs();
    c.bench_function("run_inputs_100k", |b| {
        b.iter(|| black_box(engine.run_inputs(pair.id()).unwrap()))
    });
}

/// The raw-row capture the engine lock holds now.
fn capture_run_inputs_100k(c: &mut Criterion) {
    let (_directory, engine, pair) = engine_with_inputs();
    c.bench_function("capture_run_inputs_100k", |b| {
        b.iter(|| black_box(engine.capture_run_inputs(pair.id()).unwrap()))
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = run_inputs_100k, capture_run_inputs_100k
}
criterion_main!(benches);
