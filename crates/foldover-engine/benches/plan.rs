//! Criterion benchmark for the plan hot path (PLAN.md §5.2):
//! `plan_candidates` over two synthetic sides of 100_000 entries each
//! (1_000 directories + 99_000 files) and a base mixing unchanged,
//! edited, conflicting, new and deleted files, as a diverged pair
//! would produce.

use std::collections::BTreeMap;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use foldover_engine::{Choices, Moves, plan_candidates, plan_resolved};
use foldover_types::{
    BaselineEntry, CaseMatching, Digest, EntryContent, EntryVersion, NameMatching, PathPrefixIndex,
    ScanControl, Skip, SyncMode, SyncPath, UnicodeMatching,
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

/// `plan_resolved` over the same 100_000-row pair, with every one of the
/// 1_000 top-level folders skipped. A skip covers its whole branch, so the
/// pass is a focused 100k-entry, 1k-prefix workload.
fn plan_skipped_100k_entries(c: &mut Criterion) {
    let (base, left, right) = trees();
    let skips: std::collections::BTreeMap<SyncPath, Skip> = (0..DIRECTORIES)
        .map(|folder| {
            let name = format!("d-{folder:04}");
            (
                path(name.as_bytes()),
                Skip::new(Some(directory()), Some(directory())),
            )
        })
        .collect();
    let resolutions = std::collections::BTreeMap::new();
    let choices = Choices {
        resolutions: &resolutions,
        skips: &skips,
        left_names: NameMatching::EXACT,
        right_names: NameMatching::EXACT,
        left_moves: Moves::Copy,
        right_moves: Moves::Copy,
    };
    let plan = plan_resolved(SyncMode::Sync, &base, &left, &right, &choices, &mut || {
        ScanControl::Continue
    })
    .unwrap();
    assert_eq!(plan.rows().len(), EXPECTED_ROWS);
    c.bench_function("plan_skipped_100k_entries", |b| {
        b.iter(|| {
            black_box(
                plan_resolved(
                    SyncMode::Sync,
                    black_box(&base),
                    black_box(&left),
                    black_box(&right),
                    black_box(&choices),
                    &mut || ScanControl::Continue,
                )
                .unwrap(),
            )
        })
    });
}

/// The shared component-prefix index itself: 100_000 paths queried
/// against 1_000 roots, the shape SFTP unavailable roots, executor
/// failures and skipped branches all feed it.
fn prefix_index_100k_entries_1000_prefixes(c: &mut Criterion) {
    const ROOTS: usize = 1_000;
    const QUERIES: usize = 100_000;
    let roots: Vec<SyncPath> = (0..ROOTS)
        .map(|index| path(format!("root-{index:04}/branch").as_bytes()))
        .collect();
    let index = PathPrefixIndex::from_paths(&roots);
    let queries: Vec<SyncPath> = (0..QUERIES)
        .map(|query| {
            let root = query % ROOTS;
            path(format!("root-{root:04}/branch/file-{query:06}").as_bytes())
        })
        .collect();
    assert_eq!(index.len(), ROOTS);
    c.bench_function("prefix_index_100k_entries_1000_prefixes", |b| {
        b.iter(|| {
            let mut hits = 0;
            for query in black_box(&queries) {
                hits += usize::from(black_box(index.contains_ancestor(query)));
            }
            black_box(hits)
        })
    });
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

/// The same 100_000-row pair under the matching a case-insensitive,
/// normalizing folder uses. Every name is ASCII, so each `name_key` borrows
/// the path bytes instead of collecting NFC and lower case into a fresh
/// allocation; this is the per-row hot path `mark_name_clashes` walks.
fn plan_100k_entries_unicode_names(c: &mut Criterion) {
    let (base, left, right) = trees();
    let names = NameMatching::new(CaseMatching::Insensitive, UnicodeMatching::Normalized);
    let resolutions = std::collections::BTreeMap::new();
    let skips = std::collections::BTreeMap::new();
    let choices = Choices {
        resolutions: &resolutions,
        skips: &skips,
        left_names: names,
        right_names: names,
        left_moves: Moves::Copy,
        right_moves: Moves::Copy,
    };
    let plan = plan_resolved(SyncMode::Sync, &base, &left, &right, &choices, &mut || {
        ScanControl::Continue
    })
    .unwrap();
    assert_eq!(plan.rows().len(), EXPECTED_ROWS);
    c.bench_function("plan_100k_entries_unicode_names", |b| {
        b.iter(|| {
            black_box(
                plan_resolved(
                    SyncMode::Sync,
                    black_box(&base),
                    black_box(&left),
                    black_box(&right),
                    black_box(&choices),
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
    targets = plan_100k_entries, plan_skipped_100k_entries, prefix_index_100k_entries_1000_prefixes,
        plan_100k_entries_unicode_names
}
criterion_main!(benches);
