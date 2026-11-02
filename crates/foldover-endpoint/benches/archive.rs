//! Criterion benchmark for the archive paging hot path (PLAN.md §5.2):
//! reading a run of 5_000 items 100 at a time through
//! `LocalEndpoint::archived_items`, the walk behind the app's archive
//! search and the CLI listing a whole run page by page.

use std::fs;
use std::hint::black_box;
use std::time::{Duration, UNIX_EPOCH};

use criterion::{Criterion, criterion_group, criterion_main};
use foldover_endpoint::{LocalEndpoint, archive_name};
use foldover_types::SyncPath;

const FOLDERS: usize = 50;
const ITEMS_PER_FOLDER: usize = 100;
const PAGE: usize = 100;
const ITEMS: usize = FOLDERS * ITEMS_PER_FOLDER;
/// Any canonical run name serves; the walk only checks its shape.
const RUN_AT_MILLIS: u64 = 1_700_000_000_000;

/// One run holding `ITEMS` empty files in `FOLDERS` folders.
fn archived_run() -> (tempfile::TempDir, String) {
    let directory = tempfile::tempdir().unwrap();
    let name = archive_name(UNIX_EPOCH + Duration::from_millis(RUN_AT_MILLIS));
    let run = directory.path().join(".foldover/archive").join(&name);
    for folder in 0..FOLDERS {
        let folder = run.join(format!("d-{folder:02}"));
        fs::create_dir_all(&folder).unwrap();
        for item in 0..ITEMS_PER_FOLDER {
            fs::write(folder.join(format!("i-{item:03}")), Vec::new()).unwrap();
        }
    }
    (directory, name)
}

fn archive_pages_5k(c: &mut Criterion) {
    let (directory, run) = archived_run();
    let endpoint = LocalEndpoint::open(directory.path()).unwrap();
    c.bench_function("archive_pages_5k", |b| {
        b.iter(|| {
            let mut after: Option<SyncPath> = None;
            let mut seen = 0;
            loop {
                let (page, more) = endpoint
                    .archived_items(black_box(&run), after.as_ref(), PAGE)
                    .unwrap();
                seen += page.len();
                if !more {
                    break;
                }
                after = page.last().map(|item| item.path().clone());
            }
            assert_eq!(seen, ITEMS);
        })
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = archive_pages_5k
}
criterion_main!(benches);
