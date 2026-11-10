//! Bounded workers for independent file/link copies and archives.
//!
//! The run thread owns barriers, journal, base, commits, progress and
//! control. It drains copies before starting archives, and drains file/link
//! archives before removing any folder. Workers perform only endpoint I/O
//! and return their result; a panic stops the other jobs and reaches the
//! run thread through the same result channel.

use std::num::NonZeroUsize;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{Scope, ScopedJoinHandle};
use std::time::Duration;

use foldover_endpoint::ApplyError;
use foldover_host::ThreadEffort;
use foldover_types::{Effort, EntryVersion, ScanControl};

use super::{Completion, Executor, RunProgress, Writer, is_archive, writes};
use crate::PlanRow;

/// How often the run's thread asks its control while copies run, so a
/// cancel or a lease beat never waits for a large file.
const POLL: Duration = Duration::from_millis(100);

/// A handed-out write, as it came back.
enum Landed<'a> {
    Copied(&'a PlanRow, Result<EntryVersion, ApplyError>),
    Archived(&'a PlanRow, Result<(), ApplyError>),
    /// Stopped before it began: nothing to record.
    Skipped,
    /// The write panicked; the run's thread panics with it.
    Crashed(Box<dyn std::any::Any + Send>),
}

pub(super) struct Workers<'a> {
    /// `None` without workers, when every write runs inline.
    jobs: Option<Sender<&'a PlanRow>>,
    landed: Receiver<Landed<'a>>,
    stop: &'a AtomicBool,
    workers: usize,
    busy: usize,
    /// Whether a stop kept a handed-out write from starting.
    skipped: bool,
}

impl<'a> Workers<'a> {
    /// Start `workers` threads in `threads`, unless one would do: then
    /// every write runs on the run's thread, exactly as in plan order.
    /// Workers stop writing once `stop` is set. Each enters `effort`
    /// itself: threads do not inherit the priority of their creator.
    pub(super) fn start<'scope>(
        threads: &'scope Scope<'scope, '_>,
        writer: Writer<'a>,
        workers: NonZeroUsize,
        effort: Effort,
        stop: &'a AtomicBool,
    ) -> Self
    where
        'a: 'scope,
    {
        let (landed_sender, landed) = mpsc::channel();
        let workers = workers.get();
        if workers == 1 {
            return Self {
                jobs: None,
                landed,
                stop,
                workers,
                busy: 0,
                skipped: false,
            };
        }
        let (jobs, queue) = mpsc::channel();
        let queue = Arc::new(Mutex::new(queue));
        for _ in 0..workers {
            let (queue, landed) = (Arc::clone(&queue), landed_sender.clone());
            let _: ScopedJoinHandle<'scope, ()> = threads.spawn(move || {
                let _effort = ThreadEffort::enter(effort);
                work(writer, stop, &queue, &landed);
            });
        }
        Self {
            jobs: Some(jobs),
            landed,
            stop,
            workers,
            busy: 0,
            skipped: false,
        }
    }

    /// Whether `row` goes to a worker.
    pub(super) fn takes(&self, executor: &Executor<'_>, row: &PlanRow) -> bool {
        self.jobs.is_some() && executor.parallel_write(row)
    }

    /// Give `row` to a worker, first waiting for one to be free.
    pub(super) fn hand(
        &mut self,
        row: &'a PlanRow,
        executor: &mut Executor<'_>,
        control: &mut impl FnMut() -> ScanControl,
        progress: &dyn RunProgress,
    ) {
        while self.busy >= self.workers {
            self.wait(executor, control, progress);
        }
        let jobs = self
            .jobs
            .as_ref()
            .expect("only rows a worker takes are handed out");
        jobs.send(row)
            .expect("workers run until the run drops its sender");
        self.busy += 1;
    }

    /// Whether a stop kept a handed-out write from starting.
    pub(super) fn skipped(&self) -> bool {
        self.skipped
    }

    /// Record the writes that landed, without waiting.
    pub(super) fn collect(&mut self, executor: &mut Executor<'_>, progress: &dyn RunProgress) {
        while let Ok(landed) = self.landed.try_recv() {
            self.record(landed, executor, progress);
        }
    }

    /// Wait for every handed-out write and record it.
    pub(super) fn finish(
        &mut self,
        executor: &mut Executor<'_>,
        control: &mut impl FnMut() -> ScanControl,
        progress: &dyn RunProgress,
    ) {
        while self.busy > 0 {
            self.wait(executor, control, progress);
        }
    }

    fn wait(
        &mut self,
        executor: &mut Executor<'_>,
        control: &mut impl FnMut() -> ScanControl,
        progress: &dyn RunProgress,
    ) {
        match self.landed.recv_timeout(POLL) {
            Ok(landed) => self.record(landed, executor, progress),
            // The control's answer reaches the workers through `stop`.
            Err(RecvTimeoutError::Timeout) => {
                control();
            }
            Err(RecvTimeoutError::Disconnected) => {
                unreachable!("workers outlive the writes handed to them")
            }
        }
    }

    fn record(
        &mut self,
        landed: Landed<'_>,
        executor: &mut Executor<'_>,
        progress: &dyn RunProgress,
    ) {
        self.busy -= 1;
        match landed {
            Landed::Copied(row, result) => {
                // A failed copy wrote nothing the run counts.
                let landed = result.is_ok() && writes(row.decision());
                executor.landed(row, result);
                if landed {
                    progress.applied(executor.copied_bytes(row));
                }
            }
            Landed::Archived(row, result) => {
                let landed = result.is_ok();
                executor.landed_archive(row, result);
                if landed {
                    progress.applied(0);
                }
            }
            Landed::Skipped => self.skipped = true,
            Landed::Crashed(payload) => {
                self.stop.store(true, Ordering::Relaxed);
                panic::resume_unwind(payload)
            }
        }
        if executor.report.completion == Completion::Cancelled {
            self.stop.store(true, Ordering::Relaxed);
        }
    }
}

impl Drop for Workers<'_> {
    /// A run leaving early, on an error or a panic, stops the writes it
    /// will not record.
    fn drop(&mut self) {
        if self.busy > 0 {
            self.stop.store(true, Ordering::Relaxed);
        }
    }
}

/// One worker: write rows until the run stops handing them out.
fn work<'a>(
    writer: Writer<'a>,
    stop: &AtomicBool,
    queue: &Mutex<Receiver<&'a PlanRow>>,
    landed: &Sender<Landed<'a>>,
) {
    let mut control = || match stop.load(Ordering::Relaxed) {
        true => ScanControl::Cancel,
        false => ScanControl::Continue,
    };
    loop {
        // The run drops its sender once every write is out.
        let Ok(row) = queue.lock().unwrap_or_else(PoisonError::into_inner).recv() else {
            return;
        };
        let outcome = match stop.load(Ordering::Relaxed) {
            true => Landed::Skipped,
            false => match panic::catch_unwind(AssertUnwindSafe(|| {
                if is_archive(row.decision()) {
                    Landed::Archived(row, writer.archive(row, &mut control))
                } else {
                    Landed::Copied(row, writer.copy(row, &mut control))
                }
            })) {
                Ok(result) => result,
                Err(payload) => Landed::Crashed(payload),
            },
        };
        if landed.send(outcome).is_err() {
            return;
        }
    }
}
