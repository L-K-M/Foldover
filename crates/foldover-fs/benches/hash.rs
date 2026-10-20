//! Criterion benchmark for the content-hash hot path (PLAN.md §5.2):
//! `Root::hash_file` over a 64 MiB regular file, the bounded-read
//! BLAKE3 function `LocalEndpoint::hash_file` forwards to.

use std::fs;
use std::io::Write;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use foldover_fs::Root;
use foldover_types::{ScanControl, SyncPath};

const SIZE_BYTES: usize = 64 * 1024 * 1024;
// A repeating 4 KiB block of varying bytes: realistic hashing input
// without paying for random data generation at setup.
const BLOCK: &[u8] = &[
    0x28, 0x4d, 0x7c, 0x1f, 0xa3, 0x95, 0xe6, 0x0b, 0x59, 0xd2, 0x38, 0x8e, 0xc4, 0x71, 0x06, 0xb9,
    0xfd, 0x13, 0x6a, 0x84, 0x3e, 0xc7, 0x50, 0x29, 0x96, 0x0d, 0xeb, 0x62, 0x1a, 0xf8, 0x43, 0x77,
    0xb1, 0x05, 0xce, 0x69, 0x20, 0x9d, 0x34, 0x88, 0x56, 0xdf, 0x02, 0x7b, 0xa9, 0x16, 0xe1, 0x63,
    0x0f, 0x92, 0x4c, 0xda, 0x37, 0x85, 0x58, 0xab, 0x03, 0x74, 0xc2, 0x19, 0x60, 0xee, 0x31, 0xb6,
];

fn file() -> (tempfile::TempDir, Vec<u8>) {
    let directory = tempfile::tempdir().unwrap();
    let mut content = Vec::with_capacity(SIZE_BYTES);
    while content.len() < SIZE_BYTES {
        content.extend_from_slice(BLOCK);
    }
    content.truncate(SIZE_BYTES);
    let mut file = fs::File::create(directory.path().join("file")).unwrap();
    file.write_all(&content).unwrap();
    (directory, content)
}

fn hash_64_mib(c: &mut Criterion) {
    let (directory, content) = file();
    let path = SyncPath::try_from(b"file".as_slice()).unwrap();
    let root = Root::open(directory.path()).unwrap();
    let metadata = root.metadata(&path).unwrap();
    let digest = root
        .hash_file(&path, &metadata, &mut |_| ScanControl::Continue)
        .unwrap();
    assert_eq!(digest.as_bytes(), blake3::hash(&content).as_bytes());
    c.bench_function("hash_64_mib", |b| {
        b.iter(|| {
            black_box(
                root.hash_file(&path, &metadata, &mut |_| ScanControl::Continue)
                    .unwrap(),
            )
        })
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = hash_64_mib
}
criterion_main!(benches);
