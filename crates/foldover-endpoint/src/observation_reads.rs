//! Runs file reads on bounded workers (PLAN.md §4.9). SFTP observation
//! verifies every file's content. Local settled cache hits bypass the queue;
//! other local reads use the same handoff. As while copies run (ADR 0043),
//! the run's own thread records results and asks its control, so cancelling
//! and lease beats never wait for a large file.
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

use foldover_os::ThreadEffort;
use foldover_types::{Effort, ScanControl};

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

/// Run `read` on every item, at most `workers` at a time, and hand each
/// result to `landed` on this thread, in the order they finish. `read`
/// is given a control that turns to cancel once this stops early.
///
/// `control` cancelling or `landed` failing stops the readers and ends
/// with that error, after they wound down. A reader's panic is raised
/// here once all of them have stopped. At most twice the worker count is
/// produced but not yet recorded; iterator inputs can release completed rows.
#[cfg(test)]
pub(crate) fn read_each<I, T: Send, R: Send>(
    items: I,
    workers: NonZeroUsize,
    read: impl Fn(&T, &dyn Fn() -> ScanControl) -> R + Sync,
    mut landed: impl FnMut(&T, R) -> Result<(), ScanError>,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<(), ScanError>
where
    I: IntoIterator<Item = T>,
{
    let stop = AtomicBool::new(false);
    thread::scope(|threads| {
        let mut pending = Reads::start(threads, workers, Effort::Full, &stop, &read);
        let mut items = items.into_iter();
        loop {
            pending.make_room(&mut landed, control)?;
            let Some(item) = items.next() else { break };
            pending.submit(item, &mut landed, control)?;
        }
        pending.finish(&mut landed, control)
    })
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
    /// Each reader enters `effort` itself: threads do not inherit the
    /// priority of the thread that starts them.
    pub(crate) fn start<'scope>(
        threads: &'scope thread::Scope<'scope, '_>,
        workers: NonZeroUsize,
        effort: Effort,
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
            threads.spawn(move || {
                let _effort = ThreadEffort::enter(effort);
                reader(&queue, stop, read, &results);
            });
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
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    use super::*;

    const WORKERS: NonZeroUsize = NonZeroUsize::new(4).expect("four is not zero");
    /// Long enough for a loaded machine; only a failing test waits it out.
    const PATIENCE: Duration = Duration::from_secs(60);

    fn proceed() -> ScanControl {
        ScanControl::Continue
    }

    /// Wait until `asked` cancels; a reader nobody stops fails the test.
    fn until_cancelled(asked: &dyn Fn() -> ScanControl) {
        let started = Instant::now();
        while asked() == ScanControl::Continue {
            assert!(started.elapsed() < PATIENCE, "the reader was never stopped");
            thread::yield_now();
        }
    }

    #[test]
    fn owned_rows_are_produced_with_bounded_live_storage() {
        use std::sync::Arc;
        struct OwnedRow(Arc<AtomicUsize>);
        impl Drop for OwnedRow {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let live = Arc::new(AtomicUsize::new(0));
        let mut landed = 0;
        let rows = (0..10_000).map(|_| {
            let count = live.fetch_add(1, Ordering::SeqCst) + 1;
            assert!(
                count <= 2 * WORKERS.get(),
                "too many owned rows alive: {count}"
            );
            OwnedRow(Arc::clone(&live))
        });
        read_each(
            rows,
            WORKERS,
            |_, _| (),
            |_, ()| {
                landed += 1;
                Ok(())
            },
            &mut proceed,
        )
        .unwrap();
        assert_eq!(landed, 10_000);
        assert_eq!(live.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn owned_rows_can_contain_values_that_are_not_sync() {
        use std::cell::Cell;
        let mut seen = Vec::new();
        read_each(
            (0..10).map(Cell::new),
            WORKERS,
            |value, _| value.get(),
            |_, value| {
                seen.push(value);
                Ok(())
            },
            &mut proceed,
        )
        .unwrap();
        seen.sort_unstable();
        assert_eq!(seen, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn a_panicking_producer_stops_readers_already_in_progress() {
        let started = AtomicBool::new(false);
        let stopped = AtomicBool::new(false);
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            let items = (0..10).inspect(|&index| {
                if index > 0 {
                    let waiting = Instant::now();
                    while !started.load(Ordering::SeqCst) {
                        assert!(waiting.elapsed() < PATIENCE);
                        thread::yield_now();
                    }
                    panic!("the producer failed");
                }
            });
            read_each(
                items,
                WORKERS,
                |_, asked| {
                    started.store(true, Ordering::SeqCst);
                    until_cancelled(asked);
                    stopped.store(true, Ordering::SeqCst);
                },
                |_, ()| Ok(()),
                &mut proceed,
            )
        }));
        assert!(outcome.is_err());
        assert!(stopped.load(Ordering::SeqCst));
    }

    #[test]
    fn a_recording_failure_makes_the_handoff_unusable() {
        let stop = AtomicBool::new(false);
        let read = |_: &u32, _: &dyn Fn() -> ScanControl| ();
        thread::scope(|threads| {
            let mut pending = Reads::start(threads, WORKERS, Effort::Full, &stop, &read);
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

    #[test]
    fn every_item_lands_once_with_its_result() {
        let items: Vec<u32> = (0..100).collect();
        let mut seen = Vec::new();

        read_each(
            items.iter().copied(),
            WORKERS,
            |item, _| item * 2,
            |item, doubled| {
                assert_eq!(doubled, item * 2);
                seen.push(*item);
                Ok(())
            },
            &mut proceed,
        )
        .unwrap();

        seen.sort_unstable();
        assert_eq!(seen, items);
    }

    #[test]
    fn nothing_to_read_is_fine() {
        let mut landed = 0;

        read_each(
            std::iter::empty::<u32>(),
            WORKERS,
            |_, _| (),
            |_, ()| {
                landed += 1;
                Ok(())
            },
            &mut proceed,
        )
        .unwrap();

        assert_eq!(landed, 0);
    }

    #[test]
    fn reads_overlap_up_to_the_worker_count() {
        let items = [0u32; 8];
        let running = AtomicUsize::new(0);
        let most = AtomicUsize::new(0);

        // Each of the first readers holds on until all of them are in.
        read_each(
            items.iter().copied(),
            WORKERS,
            |_, _| {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
                let started = Instant::now();
                while most.load(Ordering::SeqCst) < WORKERS.get() {
                    assert!(started.elapsed() < PATIENCE, "reads did not overlap");
                    thread::yield_now();
                }
                running.fetch_sub(1, Ordering::SeqCst);
            },
            |_, ()| Ok(()),
            &mut proceed,
        )
        .unwrap();

        assert_eq!(most.load(Ordering::SeqCst), WORKERS.get());
    }

    #[test]
    fn cancelling_stops_a_read_in_progress() {
        let items = [0u32; 8];
        let started = AtomicBool::new(false);

        let outcome = read_each(
            items.iter().copied(),
            WORKERS,
            |_, asked| {
                started.store(true, Ordering::SeqCst);
                until_cancelled(asked);
            },
            |_, ()| Ok(()),
            &mut || match started.load(Ordering::SeqCst) {
                true => ScanControl::Cancel,
                false => ScanControl::Continue,
            },
        );

        assert!(matches!(outcome, Err(ScanError::Cancelled)), "{outcome:?}");
    }

    #[test]
    fn a_result_that_cannot_be_recorded_stops_the_rest() {
        let items: Vec<u32> = (0..8).collect();
        let reads = AtomicUsize::new(0);

        // The first read fails to record; the others wait to be stopped.
        let outcome = read_each(
            items.iter().copied(),
            WORKERS,
            |_, asked| {
                if reads.fetch_add(1, Ordering::SeqCst) > 0 {
                    until_cancelled(asked);
                }
            },
            |_, ()| Err(ScanError::RootMissing),
            &mut proceed,
        );

        assert!(
            matches!(outcome, Err(ScanError::RootMissing)),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_panicking_read_stops_the_others_and_is_raised() {
        let items: Vec<u32> = (0..8).collect();
        let waiting = AtomicUsize::new(0);
        let stopped = AtomicUsize::new(0);

        // The first read fails once another is in progress. The patience
        // panic in `until_cancelled` would pass for the raised one, so what
        // counts is that the other read ended on its own.
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            read_each(
                items.iter().copied(),
                WORKERS,
                |item, asked| {
                    if *item == 0 {
                        let started = Instant::now();
                        while waiting.load(Ordering::SeqCst) == 0 {
                            assert!(started.elapsed() < PATIENCE, "no other read started");
                            thread::yield_now();
                        }
                        panic!("the read failed");
                    }
                    waiting.fetch_add(1, Ordering::SeqCst);
                    until_cancelled(asked);
                    stopped.fetch_add(1, Ordering::SeqCst);
                },
                |_, ()| Ok(()),
                &mut proceed,
            )
        }));

        assert!(outcome.is_err());
        let waited = waiting.load(Ordering::SeqCst);
        assert!(waited > 0);
        assert_eq!(stopped.load(Ordering::SeqCst), waited);
    }
}
