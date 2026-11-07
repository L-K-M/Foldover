//! Isolated qualification runner, not the user's installed agent. Fixtures and
//! process/RSS sampling are owned by the external perf scripts.

use std::cell::Cell;
use std::error::Error;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use foldover_engine::{
    Approval, Completion, Engine, ObserveStep, PairRun, RunContext, RunOutcome, RunPhase,
    RunProgress, StateLocation,
};
use foldover_types::{
    Durability, MetadataPolicy, PairDraft, PairOptions, PairRoot, Propagation, ScanControl, Side,
    SyncMode,
};

struct Progress {
    iteration: usize,
    checked: Cell<u64>,
    applied: Cell<u64>,
}

#[derive(PartialEq)]
enum RunMode {
    Complete,
    PrepareOnly,
}

impl RunProgress for Progress {
    fn measured(&self, phase: RunPhase, elapsed: Duration) {
        println!(
            "phase\t{}\t{phase:?}\t{}",
            self.iteration,
            elapsed.as_nanos()
        );
    }
    fn observed(&self, side: Side, step: ObserveStep) {
        if step == ObserveStep::Checked {
            let checked = self.checked.get() + 1;
            self.checked.set(checked);
            if checked.is_multiple_of(10_000) {
                println!("progress\t{}\t{side:?}\t{checked}", self.iteration);
            }
        }
    }
    fn planned(&self, changes: u64, bytes: u64) {
        println!("planned\t{}\t{changes}\t{bytes}", self.iteration);
    }
    fn applied(&self, _bytes: u64) {
        let applied = self.applied.get() + 1;
        self.applied.set(applied);
        if applied.is_multiple_of(1_000) {
            println!("applied\t{}\t{applied}", self.iteration);
        }
    }
    fn waiting(&self, holder: Option<&str>) {
        println!("waiting\t{}\t{holder:?}", self.iteration);
    }
}

fn metric(iteration: usize, name: &str, elapsed: Duration) {
    println!("metric\t{iteration}\t{name}\t{}", elapsed.as_nanos());
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("process\t{}", std::process::id());
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(4..=5).contains(&args.len()) {
        return Err(
            "usage: performance STATE LEFT RIGHT ITERATIONS [prepare] (synthetic fixtures only)"
                .into(),
        );
    }
    let mode = match args.get(4).map(|arg| arg.as_bytes()) {
        None => RunMode::Complete,
        Some(b"prepare") => RunMode::PrepareOnly,
        Some(_) => {
            return Err("unknown run mode; use prepare or omit it for a complete run".into());
        }
    };
    // Refuse the ordinary app catalog and folders. The harness can approve
    // every change only because all three paths are explicitly disposable.
    if args[..3].iter().any(|arg| {
        !arg.as_bytes()
            .windows(b"foldover-perf".len())
            .any(|part| part == b"foldover-perf")
    }) {
        return Err("state and both roots must be inside foldover-perf fixture directories".into());
    }
    let iterations: usize = args[3]
        .to_str()
        .ok_or("iterations must be UTF-8")?
        .parse()?;
    if !(1..=100).contains(&iterations) {
        return Err("iterations must be between 1 and 100".into());
    }
    let draft = PairDraft::new(
        "Performance fixture".into(),
        PairRoot::from_bytes(args[1].as_bytes().to_vec())?,
        PairRoot::from_bytes(args[2].as_bytes().to_vec())?,
        SyncMode::Mirror { source: Side::Left },
    )?
    .with_options(PairOptions::new(
        MetadataPolicy::new(
            Propagation::Ignore,
            Propagation::Ignore,
            Propagation::Ignore,
        ),
        Durability::Standard,
    ));
    let mut engine = Engine::open(&StateLocation::Directory(PathBuf::from(&args[0])))?;
    let pairs = engine.pairs()?;
    let pair = match pairs.as_slice() {
        [] => engine.create_pair(&draft)?,
        [pair] if pair.draft() == &draft => pair.clone(),
        _ => return Err("fixture catalog does not contain exactly this pair".into()),
    };
    for iteration in 0..iterations {
        let progress = Progress {
            iteration,
            checked: Cell::new(0),
            applied: Cell::new(0),
        };
        let total = Instant::now();
        let loaded = Instant::now();
        let inputs = engine.run_inputs(pair.id())?;
        metric(iteration, "InputLoad", loaded.elapsed());
        let prepared = Instant::now();
        let run = PairRun::prepare(inputs, &mut || ScanControl::Continue, &progress)?;
        metric(iteration, "Prepare", prepared.elapsed());
        let exported = Instant::now();
        for side in [Side::Left, Side::Right] {
            if run.snapshot_changed(side) {
                let stored = Instant::now();
                engine.store_snapshot_iter(
                    pair.id(),
                    run.revision(),
                    side,
                    run.snapshot_iter(side),
                )?;
                metric(
                    iteration,
                    match side {
                        Side::Left => "SnapshotLeftStore",
                        Side::Right => "SnapshotRightStore",
                    },
                    stored.elapsed(),
                );
            }
        }
        metric(iteration, "SnapshotExportAndStore", exported.elapsed());
        let previewed = Instant::now();
        let preview = run.preview();
        for item in preview.items().iter().take(1) {
            println!(
                "sample\t{iteration}\t{:?}\t{:?}",
                item.path(),
                item.versions()
            );
        }
        let approval = Approval::new(
            preview.deletions(Side::Left).deletions(),
            preview.deletions(Side::Right).deletions(),
        );
        metric(iteration, "Preview", previewed.elapsed());
        drop(preview);
        if mode == RunMode::PrepareOnly {
            println!("prepared\t{iteration}");
            metric(iteration, "Total", total.elapsed());
            continue;
        }
        let mut commits = 0u64;
        let mut commit_time = Duration::ZERO;
        let applied = Instant::now();
        let outcome = run.execute(
            approval,
            RunContext::Reviewed,
            SystemTime::now(),
            &mut |revision, commit| {
                let started = Instant::now();
                let result = engine.commit_run(pair.id(), revision, commit);
                commit_time += started.elapsed();
                commits += 1;
                result
            },
            &mut || ScanControl::Continue,
            &progress,
        )?;
        metric(iteration, "Apply", applied.elapsed());
        metric(iteration, "Commits", commit_time);
        match outcome {
            RunOutcome::Ran(report)
                if report.completion() == Completion::Finished
                    && report.failures().is_empty()
                    && report.conflicts().is_empty() =>
            {
                println!(
                    "result\t{iteration}\t{}\t{}\t{}\t{commits}",
                    report.copied(),
                    report.updated(),
                    report.archived()
                );
            }
            other => return Err(format!("fixture run did not finish cleanly: {other:?}").into()),
        }
        metric(iteration, "Total", total.elapsed());
    }
    Ok(())
}
