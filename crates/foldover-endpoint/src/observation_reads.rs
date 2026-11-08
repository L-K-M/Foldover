//! Reads entries on several threads at once (PLAN.md §4.9). Neither an
//! SFTP stat nor a local one can prove a file unchanged, so an observation
//! reads every file, and many small files would otherwise wait one at a
//! time. As while copies run (ADR 0043), the run's own thread is the only
//! one that records and asks the run's control, so cancelling and lease
//! beats never wait for a large file.
//!
//! ```text
//!               files             results
//!   run thread ───────▶ readers ──────────▶ run thread
//!     └ control, progress, the observation
//! ```

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use foldover_types::ScanControl;

use crate::ScanError;

/// How often the run's thread asks its control while reads run.
const POLL: Duration = Duration::from_millis(100);

/// Stops every reader if its holder panics, so the others do not read on
/// for a result nobody will use.
struct StopOnPanic<'a>(&'a AtomicBool);

impl Drop for StopOnPanic<'_> {
    fn drop(&mut self) {
        if thread::panicking() {
            self.0.store(true, Ordering::Relaxed);
        }
    }
}

/// Bounded handoff for either an iterator or a metadata walk's callback.
/// Only this thread records results and checks the caller's control.
/// A failed or unwinding producer drops the sender and stops every reader.
pub(crate) struct Reads<'a, T, R> {
    jobs: Option<SyncSender<T>>,
    results: Receiver<(T, R)>,
    stop: &'a AtomicBool,
    busy: usize,
    limit: usize,
}

impl<'a, T, R> Reads<'a, T, R> {
    pub(crate) fn start<'scope>(
        threads: &'scope thread::Scope<'scope, '_>,
        workers: NonZeroUsize,
        stop: &'a AtomicBool,
        read: &'a (impl Fn(&T, &dyn Fn() -> ScanControl) -> R + Sync),
    ) -> Self
    where
        'a: 'scope,
        T: Send + 'scope,
        R: Send + 'scope,
    {
        let limit = workers.get().checked_mul(2).expect("bounded worker count");
        let (jobs, queue) = mpsc::sync_channel(limit);
        let queue = Arc::new(Mutex::new(queue));
        let (results, received) = mpsc::channel();
        for _ in 0..workers.get() {
            let (queue, results) = (Arc::clone(&queue), results.clone());
            threads.spawn(move || reader(&queue, stop, read, &results));
        }
        drop(results);
        Self {
            jobs: Some(jobs),
            results: received,
            stop,
            busy: 0,
            limit,
        }
    }

    pub(crate) fn submit(
        &mut self,
        item: T,
        landed: &mut impl FnMut(&T, R) -> Result<(), ScanError>,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<(), ScanError> {
        self.make_room(landed, control)?;
        // busy counts queued, running and completed-but-unrecorded rows, so
        // the channel cannot fill while busy is below its capacity.
        if self
            .jobs
            .as_ref()
            .expect("producer still holds its sender")
            .send(item)
            .is_err()
        {
            self.stop.store(true, Ordering::Relaxed);
            return Err(readers_gone());
        }
        self.busy += 1;
        while let Ok((item, result)) = self.results.try_recv() {
            self.check(control)?;
            self.busy -= 1;
            if let Err(error) = landed(&item, result) {
                self.stop.store(true, Ordering::Relaxed);
                return Err(error);
            }
        }
        Ok(())
    }

    pub(crate) fn finish(
        &mut self,
        landed: &mut impl FnMut(&T, R) -> Result<(), ScanError>,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<(), ScanError> {
        self.check(control)?;
        while self.busy > 0 {
            self.wait(landed, control)?;
        }
        Ok(())
    }

    fn check(&self, control: &mut impl FnMut() -> ScanControl) -> Result<(), ScanError> {
        if control() == ScanControl::Cancel || self.stop.load(Ordering::Relaxed) {
            self.stop.store(true, Ordering::Relaxed);
            return Err(ScanError::Cancelled);
        }
        Ok(())
    }

    fn make_room(
        &mut self,
        landed: &mut impl FnMut(&T, R) -> Result<(), ScanError>,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<(), ScanError> {
        self.check(control)?;
        while self.busy >= self.limit {
            self.wait(landed, control)?;
        }
        Ok(())
    }

    fn wait(
        &mut self,
        landed: &mut impl FnMut(&T, R) -> Result<(), ScanError>,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<(), ScanError> {
        self.check(control)?;
        match self.results.recv_timeout(POLL) {
            Ok((item, result)) => {
                self.busy -= 1;
                let outcome = landed(&item, result);
                if outcome.is_err() {
                    self.stop.store(true, Ordering::Relaxed);
                }
                outcome
            }
            Err(RecvTimeoutError::Timeout) => Ok(()),
            Err(RecvTimeoutError::Disconnected) => {
                self.stop.store(true, Ordering::Relaxed);
                Err(readers_gone())
            }
        }
    }
}

impl<T, R> Drop for Reads<'_, T, R> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.jobs.take();
    }
}

fn readers_gone() -> ScanError {
    ScanError::Io {
        path: None,
        source: std::io::Error::other("Observation readers stopped before returning every result"),
    }
}

fn reader<T, R>(
    queue: &Mutex<Receiver<T>>,
    stop: &AtomicBool,
    read: &impl Fn(&T, &dyn Fn() -> ScanControl) -> R,
    results: &Sender<(T, R)>,
) {
    let _stop = StopOnPanic(stop);
    let asked = || match stop.load(Ordering::Relaxed) {
        true => ScanControl::Cancel,
        false => ScanControl::Continue,
    };
    while asked() == ScanControl::Continue {
        let Ok(item) = queue.lock().unwrap_or_else(PoisonError::into_inner).recv() else {
            return;
        };
        if asked() == ScanControl::Cancel {
            return;
        }
        let result = read(&item, &asked);
        if results.send((item, result)).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKERS: NonZeroUsize = NonZeroUsize::new(4).expect("four is not zero");

    fn proceed() -> ScanControl {
        ScanControl::Continue
    }

    #[test]
    fn a_recording_failure_makes_the_handoff_unusable() {
        let stop = AtomicBool::new(false);
        let read = |_: &u32, _: &dyn Fn() -> ScanControl| ();
        thread::scope(|threads| {
            let mut pending = Reads::start(threads, WORKERS, &stop, &read);
            let mut refuse = |_: &u32, ()| Err(ScanError::RootMissing);
            let result = pending
                .submit(0, &mut refuse, &mut proceed)
                .and_then(|()| pending.finish(&mut refuse, &mut proceed));
            assert!(matches!(result, Err(ScanError::RootMissing)));
            assert!(matches!(
                pending.submit(1, &mut |_, ()| Ok(()), &mut proceed),
                Err(ScanError::Cancelled)
            ));
        });
    }
}
