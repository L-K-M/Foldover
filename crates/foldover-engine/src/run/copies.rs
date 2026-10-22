//! Copies run on several threads where both folders take them (PLAN.md
//! §4.7: siblings run in parallel; §4.9: parallelism per device). The
//! run's own thread stays the only one that records: it hands copies out
//! in plan order, journals each as it lands, and keeps asking the run's
//! control, so cancelling and lease beats work as before. Folders, moves
//! and archives stay on it, so a copy is handed out only after the folder
//! it goes into was made.
//!
//! ```text
//!               copies            results
//!   run thread ────────▶ workers ────────▶ run thread
//!     └ folders, moves, archives, journal, commits, control
//! ```

use std::num::NonZeroUsize;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{Scope, ScopedJoinHandle};
use std::time::Duration;

use foldover_endpoint::ApplyError;
use foldover_types::{EntryVersion, ScanControl};

use super::{Executor, RunProgress, Writer, writes};
use crate::PlanRow;

/// How often the run's thread asks its control while copies run, so a
/// cancel or a lease beat never waits for a large file.
const POLL: Duration = Duration::from_millis(100);

/// A handed-out copy, as it came back.
enum Landed<'a> {
    Copied(&'a PlanRow, Result<EntryVersion, ApplyError>),
    /// Stopped before it began: nothing to record.
    Skipped,
    /// The copy panicked; the run's thread panics with it.
    Crashed(Box<dyn std::any::Any + Send>),
}

pub(super) struct Copies<'a> {
    /// `None` without workers, when every copy runs inline.
    jobs: Option<Sender<&'a PlanRow>>,
    landed: Receiver<Landed<'a>>,
    stop: &'a AtomicBool,
    workers: usize,
    busy: usize,
    /// Whether a stop kept a handed-out copy from starting.
    skipped: bool,
}

impl<'a> Copies<'a> {
    /// Start `workers` threads in `threads`, unless one would do: then
    /// every copy runs on the run's thread, exactly as in plan order.
    /// Workers stop copying once `stop` is set.
    pub(super) fn start<'scope>(
        threads: &'scope Scope<'scope, '_>,
        writer: Writer<'a>,
        workers: NonZeroUsize,
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
            let _: ScopedJoinHandle<'scope, ()> =
                threads.spawn(move || work(writer, stop, &queue, &landed));
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
        self.jobs.is_some() && executor.copies(row)
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

    /// Whether a stop kept a handed-out copy from starting.
    pub(super) fn skipped(&self) -> bool {
        self.skipped
    }

    /// Record the copies that landed, without waiting.
    pub(super) fn collect(&mut self, executor: &mut Executor<'_>, progress: &dyn RunProgress) {
        while let Ok(landed) = self.landed.try_recv() {
            self.record(landed, executor, progress);
        }
    }

    /// Wait for every handed-out copy and record it.
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
                unreachable!("workers outlive the copies handed to them")
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
            Landed::Skipped => self.skipped = true,
            Landed::Crashed(payload) => {
                self.stop.store(true, Ordering::Relaxed);
                panic::resume_unwind(payload)
            }
        }
    }
}

impl Drop for Copies<'_> {
    /// A run leaving early, on an error or a panic, stops the copies it
    /// will not record.
    fn drop(&mut self) {
        if self.busy > 0 {
            self.stop.store(true, Ordering::Relaxed);
        }
    }
}

/// One worker: copy rows until the run stops handing them out.
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
        // The run drops its sender once every copy is out.
        let Ok(row) = queue.lock().unwrap_or_else(PoisonError::into_inner).recv() else {
            return;
        };
        let outcome = match stop.load(Ordering::Relaxed) {
            true => Landed::Skipped,
            false => match panic::catch_unwind(AssertUnwindSafe(|| writer.copy(row, &mut control)))
            {
                Ok(result) => Landed::Copied(row, result),
                Err(payload) => Landed::Crashed(payload),
            },
        };
        if landed.send(outcome).is_err() {
            return;
        }
    }
}
