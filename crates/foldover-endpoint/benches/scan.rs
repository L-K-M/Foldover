//! Criterion benchmark for the scan hot path (PLAN.md §5.2):
//! observing a tree of 10_000 small files in nested directories
//! through `LocalEndpoint::observe`, the inventory path every preview
//! and run starts with.

use std::fs;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use foldover_endpoint::LocalEndpoint;
use foldover_types::{MetadataPolicy, ScanControl};

const DIRECTORIES: usize = 25;
const SUBDIRECTORIES: usize = 20;
const FILES_PER_SUBDIRECTORY: usize = 20;
// 25 * 20 * 20 = 10_000 files in 525 directories. The payload is small
// and constant: the scan reads metadata, never content.
const FILE_CONTENT: &[u8] = &[0x5a; 128];
const EXPECTED_ENTRIES: usize = DIRECTORIES
    + DIRECTORIES * SUBDIRECTORIES
    + DIRECTORIES * SUBDIRECTORIES * FILES_PER_SUBDIRECTORY;

fn tree() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    for top in 0..DIRECTORIES {
        for sub in 0..SUBDIRECTORIES {
            let folder = directory.path().join(format!("d-{top:02}/s-{sub:02}"));
            fs::create_dir_all(&folder).unwrap();
            for file in 0..FILES_PER_SUBDIRECTORY {
                fs::write(folder.join(format!("f-{file:02}")), FILE_CONTENT).unwrap();
            }
        }
    }
    directory
}

fn scan_10k_files(c: &mut Criterion) {
    let directory = tree();
    let endpoint = LocalEndpoint::open(directory.path()).unwrap();
    let policy = MetadataPolicy::default();
    let observation = endpoint
        .observe(policy, &mut || ScanControl::Continue)
        .unwrap();
    assert_eq!(observation.entries().len(), EXPECTED_ENTRIES);
    c.bench_function("scan_10k_files", |b| {
        b.iter(|| {
            black_box(
                endpoint
                    .observe(policy, &mut || ScanControl::Continue)
                    .unwrap(),
            )
        })
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = scan_10k_files
}
criterion_main!(benches);
