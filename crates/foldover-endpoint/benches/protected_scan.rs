//! Explicit protected-home comparison benchmark. Kept separate from the
//! ordinary scan budget gate; run with `cargo bench --bench protected_scan`.

use std::fs;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use foldover_endpoint::LocalEndpoint;
use foldover_types::{MetadataPolicy, ScanControl};

const FILE_CONTENT: &[u8] = &[0x5a; 128];

fn scan_protected_home(c: &mut Criterion) {
    let directory = tempfile::tempdir().unwrap();
    let state = directory
        .path()
        .join("Library/Application Support/Foldover");
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("pairs.sqlite"), b"private records").unwrap();
    for folder in 0..20 {
        let folder = directory
            .path()
            .join(format!("d-{folder:02}/one/two/three/four/five"));
        fs::create_dir_all(&folder).unwrap();
        for file in 0..50 {
            fs::write(folder.join(format!("f-{file:02}")), FILE_CONTENT).unwrap();
        }
    }
    let endpoint = LocalEndpoint::open_protecting(directory.path(), Some(&state)).unwrap();
    let policy = MetadataPolicy::default();
    let observation = endpoint
        .observe(policy, &mut || ScanControl::Continue)
        .unwrap();
    assert_eq!(observation.entries().len(), 1_000 + 20 * 6 + 2);
    assert!(observation.issues().is_empty());
    c.bench_function("scan_protected_home_1k", |b| {
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
    targets = scan_protected_home
}
criterion_main!(benches);
