//! Reads files on several sessions at once (PLAN.md §4.9). SFTP stat
//! cannot prove a file unchanged, so an observation reads every file, and
//! many small files would otherwise wait on the network one at a time.
//! As while copies run (ADR 0043), the run's own thread is the only one
//! that records and asks the run's control, so cancelling and lease beats
//! never wait for a large file.
//!
//! ```text
//!               files             results
//!   run thread ───────▶ readers ──────────▶ run thread
//!     └ control, progress, the observation
//! ```

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
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

/// Run `read` on every item, at most `workers` at a time, and hand each
/// result to `landed` on this thread, in the order they finish. `read`
/// is given a control that turns to cancel once this stops early.
///
/// `control` cancelling or `landed` failing stops the readers and ends
/// with that error, after they wound down. A reader's panic is raised
/// here once all of them have stopped.
pub(super) fn read_each<T: Sync, R: Send>(
    items: &[T],
    workers: NonZeroUsize,
    read: impl Fn(&T, &dyn Fn() -> ScanControl) -> R + Sync,
    landed: impl FnMut(&T, R) -> Result<(), ScanError>,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<(), ScanError> {
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let (sender, results) = mpsc::channel();
    thread::scope(|threads| {
        let _stop = StopOnPanic(&stop);
        for _ in 0..workers.get().min(items.len()) {
            let (sender, next, stop, read) = (sender.clone(), &next, &stop, &read);
            threads.spawn(move || reader(items, next, stop, read, &sender));
        }
        // Only the readers hold senders now, so the results end with them.
        drop(sender);

        let outcome = collect(items, &results, landed, control);
        stop.store(true, Ordering::Relaxed);
        outcome
    })
}

/// Take items until they run out or the run stops.
fn reader<T, R>(
    items: &[T],
    next: &AtomicUsize,
    stop: &AtomicBool,
    read: &impl Fn(&T, &dyn Fn() -> ScanControl) -> R,
    results: &Sender<(usize, R)>,
) {
    let _stop = StopOnPanic(stop);
    let asked = || match stop.load(Ordering::Relaxed) {
        true => ScanControl::Cancel,
        false => ScanControl::Continue,
    };
    while asked() == ScanControl::Continue {
        let index = next.fetch_add(1, Ordering::Relaxed);
        let Some(item) = items.get(index) else {
            return;
        };
        if results.send((index, read(item, &asked))).is_err() {
            return;
        }
    }
}

/// Record results as they land, asking `control` between them and at
/// least every `POLL`.
fn collect<T, R>(
    items: &[T],
    results: &Receiver<(usize, R)>,
    mut landed: impl FnMut(&T, R) -> Result<(), ScanError>,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<(), ScanError> {
    loop {
        if control() == ScanControl::Cancel {
            return Err(ScanError::Cancelled);
        }
        match results.recv_timeout(POLL) {
            Ok((index, result)) => landed(&items[index], result)?,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{self, AssertUnwindSafe};
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
    fn every_item_lands_once_with_its_result() {
        let items: Vec<u32> = (0..100).collect();
        let mut seen = Vec::new();

        read_each(
            &items,
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
            &[] as &[u32],
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
            &items,
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
            &items,
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
            &items,
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

        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            read_each(
                &items,
                WORKERS,
                |item, asked| {
                    assert_ne!(*item, 0, "the read failed");
                    until_cancelled(asked);
                },
                |_, ()| Ok(()),
                &mut proceed,
            )
        }));

        assert!(outcome.is_err());
    }
}
