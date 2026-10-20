//! Criterion benchmark for the plan hot path (PLAN.md §5.2):
//! `plan_candidates` over two synthetic sides of 100_000 entries each
//! (1_000 directories + 99_000 files) and a base mixing unchanged,
//! edited, conflicting, new and deleted files, as a diverged pair
//! would produce.

use std::collections::BTreeMap;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use foldover_engine::plan_candidates;
use foldover_types::{
    BaselineEntry, Digest, EntryContent, EntryVersion, ScanControl, SyncMode, SyncPath,
};

const DIRECTORIES: usize = 1_000;
const FILES_PER_DIRECTORY: usize = 99;
const EXPECTED_ROWS: usize = DIRECTORIES + DIRECTORIES * FILES_PER_DIRECTORY;

/// Deterministic per-file digests: equal paths compare equal and
/// diverging sides differ, without hashing anything at setup.
fn digest(seed: u32, salt: u8) -> Digest {
    let mut bytes = [0u8; Digest::BYTE_LEN];
    bytes[..4].copy_from_slice(&seed.to_be_bytes());
    bytes[Digest::BYTE_LEN - 1] = salt;
    Digest::from_bytes(bytes)
}

fn path(bytes: &[u8]) -> SyncPath {
    SyncPath::try_from(bytes).unwrap()
}

fn directory() -> EntryVersion {
    EntryVersion::new(
        EntryContent::Directory,
        Digest::from_bytes([0; Digest::BYTE_LEN]),
    )
}

fn file_version(seed: u32, salt: u8) -> EntryVersion {
    EntryVersion::new(EntryContent::File(digest(seed, salt)), digest(seed, 0xee))
}

/// Base, left and right trees. Each file's classification cycles over
/// five kinds, so the plan exercises unchanged entries, one-sided
/// copies, conflicts, creations and deletions at once:
///
/// | n % 5 | base | left | right | decision |
/// |---|---|---|---|---|
/// | 0 | v | v | v | in sync |
/// | 1 | v | v' | v | copy left to right |
/// | 2 | v | v' | v'' | conflict |
/// | 3 | - | v | - | create on right |
/// | 4 | v | v | - | archive on right |
fn trees() -> (
    BTreeMap<SyncPath, BaselineEntry>,
    BTreeMap<SyncPath, EntryVersion>,
    BTreeMap<SyncPath, EntryVersion>,
) {
    let mut base = BTreeMap::new();
    let mut left = BTreeMap::new();
    let mut right = BTreeMap::new();
    for folder in 0..DIRECTORIES {
        let name = format!("d-{folder:04}");
        left.insert(path(name.as_bytes()), directory());
        right.insert(path(name.as_bytes()), directory());
        base.insert(
            path(name.as_bytes()),
            BaselineEntry::new(directory(), directory()).unwrap(),
        );
        for file in 0..FILES_PER_DIRECTORY {
            let seed = (folder * FILES_PER_DIRECTORY + file) as u32;
            let file_name = format!("{name}/f-{file:02}");
            let file_name = file_name.as_bytes();
            let (left_version, right_version, base_version) = match seed % 5 {
                0 => (
                    file_version(seed, 0),
                    Some(file_version(seed, 0)),
                    Some(file_version(seed, 0)),
                ),
                1 => (
                    file_version(seed, 1),
                    Some(file_version(seed, 0)),
                    Some(file_version(seed, 0)),
                ),
                2 => (
                    file_version(seed, 1),
                    Some(file_version(seed, 2)),
                    Some(file_version(seed, 0)),
                ),
                3 => (file_version(seed, 0), None, None),
                _ => (file_version(seed, 0), None, Some(file_version(seed, 0))),
            };
            left.insert(path(file_name), left_version);
            if let Some(version) = right_version {
                right.insert(path(file_name), version);
            }
            if let Some(version) = base_version {
                base.insert(
                    path(file_name),
                    BaselineEntry::new(version, version).unwrap(),
                );
            }
        }
    }
    (base, left, right)
}

fn plan_100k_entries(c: &mut Criterion) {
    let (base, left, right) = trees();
    let plan = plan_candidates(SyncMode::Sync, &base, &left, &right, &mut || {
        ScanControl::Continue
    })
    .unwrap();
    assert_eq!(plan.rows().len(), EXPECTED_ROWS);
    c.bench_function("plan_100k_entries", |b| {
        b.iter(|| {
            black_box(
                plan_candidates(
                    SyncMode::Sync,
                    black_box(&base),
                    black_box(&left),
                    black_box(&right),
                    &mut || ScanControl::Continue,
                )
                .unwrap(),
            )
        })
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = plan_100k_entries
}
criterion_main!(benches);
