//! Archive concurrency (PLAN.md §4.7): archives run on the folder's workers, copies never overlap them, and folder removals wait.

use std::num::NonZeroUsize;
use std::time::Duration;

use foldover_sim::Quirks;

use super::*;

const PARALLEL: NonZeroUsize = NonZeroUsize::new(4).expect("four is not zero");
const ARCHIVE_TIME: Duration = Duration::from_millis(20);
const ENTRIES: usize = 24;

fn quirks(workers: NonZeroUsize) -> Quirks {
    Quirks {
        workers,
        archive_time: ARCHIVE_TIME,
        ..Quirks::default()
    }
}

/// Seeds `ENTRIES` nested files and links on the left, returning the
/// simulation and each file's content (`None` for a link).
fn seed(workers: NonZeroUsize) -> (Simulation, Vec<Option<Vec<u8>>>) {
    let simulation = Simulation::in_memory(quirks(workers), quirks(workers));
    let left = simulation.memory(Folder::Left).unwrap().clone();
    let mut contents = Vec::new();
    for index in 0..ENTRIES {
        let path = format!("tree/{}/entry {}", index / 4, index);
        let content = format!("file {index}");
        if index % 2 == 0 {
            left.put_file(path.as_bytes(), content.as_bytes()).unwrap();
            contents.push(Some(content.into_bytes()));
        } else {
            left.put_link(path.as_bytes(), b"target").unwrap();
            contents.push(None);
        }
    }
    (simulation, contents)
}

/// Deletes the entries in `folders`, then the folders, and adds a copy.
fn delete_folders(
    simulation: &mut Simulation,
    contents: &[Option<Vec<u8>>],
    folders: &[usize],
) -> (Vec<Vec<u8>>, usize) {
    let left = simulation.memory(Folder::Left).unwrap().clone();
    let (mut deleted, mut entries) = (Vec::new(), 0);
    for &folder in folders {
        for (index, content) in contents.iter().enumerate() {
            if index / 4 == folder {
                left.delete_file(format!("tree/{}/entry {}", folder, index).as_bytes());
                entries += 1;
                if let Some(content) = content {
                    deleted.push(content.clone());
                }
            }
        }
        left.delete_tree(format!("tree/{folder}").as_bytes());
    }
    left.put_file(b"tree/fresh.txt", b"fresh").unwrap();
    (deleted, entries)
}

/// The reviewed sync, the deletion, and the reviewed deletion run.
fn case(workers: NonZeroUsize) -> (Simulation, Vec<Vec<u8>>) {
    let (mut simulation, contents) = seed(workers);
    let Ended::Finished(report) = simulation.run(Interruption::None, RunContext::Reviewed) else {
        panic!("the first run did not finish");
    };
    assert_eq!(report.completion(), Completion::Finished);
    assert!(report.failures().is_empty(), "{:?}", report.failures());
    assert_eq!(report.copied(), (ENTRIES + 7) as u64);
    let (deleted, entries) = delete_folders(&mut simulation, &contents, &[0, 1, 2, 3]);
    let before = simulation.snapshot();
    let Ended::Finished(report) = simulation.run(Interruption::None, RunContext::Reviewed) else {
        panic!("the deletion run did not finish");
    };
    assert_eq!(report.completion(), Completion::Finished);
    assert!(report.failures().is_empty(), "{:?}", report.failures());
    // Every deleted file and link, plus the four folder removals.
    assert_eq!(report.archived(), (entries + 4) as u64);
    assert_eq!(
        simulation.files(Folder::Right),
        simulation.files(Folder::Left)
    );
    assert_eq!(
        simulation
            .files(Folder::Left)
            .get(b"tree/fresh.txt".as_slice())
            .map(Vec::as_slice),
        Some(&b"fresh"[..])
    );
    simulation.check_nothing_lost(&before);
    (simulation, deleted)
}

/// Archives run on the folder's workers: several overlap, every file is
/// kept exactly, and no copy or folder removal crosses an archive. One
/// worker archives one entry at a time, with the same results.
#[test]
fn archives_run_on_the_workers_and_keep_every_file() {
    for (workers, peak) in [(PARALLEL, 2..=PARALLEL.get()), (NonZeroUsize::MIN, 1..=1)] {
        let (mut simulation, deleted) = case(workers);
        let right = simulation.memory(Folder::Right).unwrap().clone();
        let actual = right.peak_archives();
        assert!(peak.contains(&actual), "{actual} archives at once");
        assert!(right.archive_barriers_held());
        assert!(
            simulation
                .memory(Folder::Left)
                .unwrap()
                .archive_barriers_held()
        );
        let archive = right.archived_files();
        for content in &deleted {
            assert!(archive.contains(content), "missing archived content");
        }
        simulation.check_settled();
    }
}

/// Seeds 48 nested files on the left and syncs them.
fn many(workers: NonZeroUsize) -> (Simulation, Vec<Vec<u8>>) {
    let mut simulation = Simulation::in_memory(quirks(workers), quirks(workers));
    let left = simulation.memory(Folder::Left).unwrap().clone();
    let mut contents = Vec::new();
    for index in 0..48 {
        let content = format!("content {index}");
        left.put_file(
            format!("many/{}/entry {}", index / 8, index).as_bytes(),
            content.as_bytes(),
        )
        .unwrap();
        contents.push(content.into_bytes());
    }
    let Ended::Finished(report) = simulation.run(Interruption::None, RunContext::Reviewed) else {
        panic!("the first run did not finish");
    };
    assert!(report.failures().is_empty(), "{:?}", report.failures());
    (simulation, contents)
}

/// Deletes all 48 files and their six folders.
fn delete_all(simulation: &mut Simulation) {
    let left = simulation.memory(Folder::Left).unwrap().clone();
    for index in 0..48 {
        left.delete_file(format!("many/{}/entry {}", index / 8, index).as_bytes());
    }
    for folder in 0..6 {
        left.delete_tree(format!("many/{folder}").as_bytes());
    }
}

/// A run that finishes after a stop or a failure must converge on retry.
fn retry_converges(simulation: &mut Simulation, contents: &[Vec<u8>]) {
    let Ended::Finished(report) = simulation.run(Interruption::None, RunContext::Reviewed) else {
        panic!("the retry did not finish");
    };
    assert_eq!(report.completion(), Completion::Finished);
    assert!(report.failures().is_empty(), "{:?}", report.failures());
    assert_eq!(
        simulation.files(Folder::Right),
        simulation.files(Folder::Left)
    );
    let archive = simulation.memory(Folder::Right).unwrap().archived_files();
    for content in contents {
        assert!(archive.contains(content), "missing archived content");
    }
    simulation.check_settled();
}

/// A folder that fails mid-run reports the failures and a retry converges.
#[test]
fn a_failing_folder_reports_its_failures_and_a_retry_converges() {
    let (mut simulation, contents) = many(PARALLEL);
    delete_all(&mut simulation);
    let before = simulation.snapshot();
    match simulation.run(Interruption::Fault(Folder::Right, 5), RunContext::Reviewed) {
        Ended::Finished(report) => assert!(
            report.completion() == Completion::Cancelled || !report.failures().is_empty(),
            "the fault should cancel or fail: {report:?}"
        ),
        Ended::Interrupted => {}
        Ended::NeedsReview => panic!("a reviewed run needs no review"),
    }
    simulation.check_nothing_lost(&before);
    retry_converges(&mut simulation, &contents);
}

/// A cancelled run leaves the rest undone and a retry converges.
#[test]
fn a_cancelled_run_stops_the_archives_and_a_retry_converges() {
    let (mut simulation, contents) = many(PARALLEL);
    delete_all(&mut simulation);
    let before = simulation.snapshot();
    let Ended::Finished(report) = simulation.run(Interruption::Cancel(12), RunContext::Reviewed)
    else {
        panic!("a cancelled run is not an error");
    };
    assert_eq!(report.completion(), Completion::Cancelled);
    assert!(
        report.archived() < (contents.len() + 6) as u64,
        "the cancel stopped nothing"
    );
    simulation.check_nothing_lost(&before);
    retry_converges(&mut simulation, &contents);
}

/// An injected archive panic propagates and a retry converges.
#[test]
fn an_injected_archive_panic_propagates_and_a_retry_converges() {
    let (mut simulation, contents) = many(PARALLEL);
    delete_all(&mut simulation);
    let before = simulation.snapshot();
    simulation
        .memory(Folder::Right)
        .unwrap()
        .inject_archive_panic();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        simulation.run(Interruption::None, RunContext::Reviewed)
    }));
    let payload = match result {
        Ok(_) => panic!("the injected panic should propagate"),
        Err(payload) => payload,
    };
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str));
    assert_eq!(message, Some("injected archive panic"));
    simulation.check_nothing_lost(&before);
    retry_converges(&mut simulation, &contents);
}

/// Undo uses the real archive/restore boundary; the memory endpoint does
/// not implement archive restoration. Compare every file, link and folder.
#[test]
fn undo_restores_the_concurrent_nested_archives() {
    let mut pair = fixture(SyncMode::Sync);
    let left = pair.left.path().to_path_buf();
    let right = pair.right.path().to_path_buf();
    for index in 0..ENTRIES {
        let path = left.join(format!("tree/{}/entry {}", index / 4, index));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        if index % 2 == 0 {
            std::fs::write(path, format!("file {index}")).unwrap();
        } else {
            std::os::unix::fs::symlink("target", path).unwrap();
        }
    }
    let first = pair.sync();
    assert!(first.failures().is_empty(), "{:?}", first.failures());
    let original_base = pair.base.clone();
    for folder in 0..4 {
        std::fs::remove_dir_all(left.join(format!("tree/{folder}"))).unwrap();
    }
    std::fs::write(left.join("tree/fresh.txt"), b"fresh").unwrap();
    let report = pair.ran(Approval::new(0, 20), at(1));
    assert!(report.failures().is_empty(), "{:?}", report.failures());
    assert_eq!((report.copied(), report.archived()), (1, 20));
    let versions = |path: &std::path::Path| {
        Endpoint::open(&PairRoot::Local(root(path)), &Reach::default())
            .unwrap()
            .observe_from(
                foldover_types::PairOptions::default().metadata(),
                &Filter::none(),
                &BTreeMap::new(),
                &mut proceed,
                &mut |_| {},
            )
            .unwrap()
            .versions()
    };
    let source = versions(&left);
    let undo = pair.undo_at(at(2));
    assert_eq!(undo.completion(), Completion::Finished);
    assert!(undo.not_undone().is_empty(), "{:?}", undo.not_undone());
    assert_eq!((undo.restored(), undo.removed()), (20, 1));
    assert_eq!(versions(&left), source);
    assert_eq!(pair.base, original_base);
    assert!(pair.journal.is_none());
    let expected: BTreeMap<_, _> = original_base
        .iter()
        .map(|(path, base)| (path.clone(), base.right()))
        .collect();
    assert_eq!(versions(&right), expected);
    assert!(!right.join("tree/fresh.txt").exists());
    let prepared = pair.prepare();
    assert!(prepared.preview().deletions(Side::Right).deletions() > 0);
}
