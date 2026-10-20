//! Criterion benchmark for the state-store hot path (PLAN.md §5.2):
//! committing a 100_000-entry base through `PairStore::update_base`
//! and reading it back through `PairStore::base`, the persistence
//! every run commits through.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use foldover_state::{BaseUpdate, JournalUpdate, PairStore, StateLocation};
use foldover_types::{
    BaselineEntry, Digest, EntryContent, EntryVersion, LocalRoot, PairDraft, SyncMode, SyncPath,
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

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = db_commit_100k, db_read_100k
}
criterion_main!(benches);
