use super::*;

#[test]
fn file_archives_overlap_after_copies_and_before_folder_removal() {
    let parallel = Quirks {
        workers: PARALLEL,
        copy_time: Duration::from_millis(20),
        archive_time: Duration::from_millis(20),
        ..Quirks::default()
    };
    let mut simulation = Simulation::in_memory(parallel, parallel);
    let left = simulation.memory(Folder::Left).unwrap().clone();
    left.put_file(b"keep", b"keep").unwrap();
    for index in 0..12 {
        let name = format!("remove/file-{index:02}");
        left.put_file(name.as_bytes(), name.as_bytes()).unwrap();
    }
    assert!(matches!(
        simulation.run(Interruption::None, RunContext::Reviewed),
        Ended::Finished(_)
    ));
    left.delete_tree(b"remove");
    left.put_file(b"new-copy", b"copy before archiving")
        .unwrap();
    let before = simulation.snapshot();
    let Ended::Finished(report) = simulation.run(Interruption::None, RunContext::Reviewed) else {
        panic!("the deletion run must finish");
    };
    assert_eq!(report.completion(), Completion::Finished);
    assert!(report.failures().is_empty(), "{:?}", report.failures());
    assert_eq!((report.copied(), report.archived()), (1, 13));
    let right = simulation.memory(Folder::Right).unwrap();
    assert!(right.archive_barriers_held());
    assert!(
        (2..=PARALLEL.get()).contains(&right.peak_archives()),
        "{} archives in flight",
        right.peak_archives()
    );
    simulation.check_nothing_lost(&before);
    simulation.check_settled();
}
