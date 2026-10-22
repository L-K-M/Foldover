//! A pair's bandwidth limit (PLAN.md §4.9). Every copied byte takes its
//! share of time on one clock that all of a run's copies share, so
//! together they never go faster than the limit, however many run at
//! once:
//!
//! ```text
//! limit 1 MB/s   copy A sends 500 kB ─▶ due at +0.5 s
//!                copy B sends 500 kB ─▶ due at +1.0 s, after A's share
//! ```
//!
//! Time spent idle is not saved up for a burst later.

use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use foldover_types::ScanControl;

const NANOS_PER_SECOND: u128 = 1_000_000_000;
/// The longest a wait goes without asking the run's control.
const POLL: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub(crate) struct Pace {
    bytes_per_second: u64,
    /// When the bytes taken so far are due.
    due: Mutex<Instant>,
}

impl Pace {
    pub(crate) fn new(bytes_per_second: u64) -> Self {
        Self {
            bytes_per_second: bytes_per_second.max(1),
            due: Mutex::new(Instant::now()),
        }
    }

    /// Wait until `bytes` more may go, asking `control` along the way; a
    /// cancel ends the wait early and is returned.
    pub(crate) fn wait(
        &self,
        bytes: u64,
        control: &mut impl FnMut() -> ScanControl,
    ) -> ScanControl {
        let until = self.take(bytes, Instant::now());
        loop {
            let now = Instant::now();
            if now >= until {
                return ScanControl::Continue;
            }
            thread::sleep(until.duration_since(now).min(POLL));
            if control() == ScanControl::Cancel {
                return ScanControl::Cancel;
            }
        }
    }

    /// Book `bytes` at `now`: when they are due.
    fn take(&self, bytes: u64, now: Instant) -> Instant {
        let share = u128::from(bytes) * NANOS_PER_SECOND / u128::from(self.bytes_per_second);
        let share = Duration::from_nanos(u64::try_from(share).unwrap_or(u64::MAX));
        // ~95 years: past any real due, but inside every platform's
        // clock. `due` still accumulates across calls, so the sum can
        // overflow Instant's range: fall back rather than panic.
        let share = share.min(Duration::from_secs(3_000_000_000));
        let mut due = self.due.lock().unwrap_or_else(PoisonError::into_inner);
        *due = (*due).max(now).checked_add(share).unwrap_or(now);
        *due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_share_one_clock_and_idle_time_is_not_saved() {
        let pace = Pace::new(1_000_000);
        let start = Instant::now();
        let ms = Duration::from_millis;

        assert_eq!(pace.take(500_000, start), start + ms(500));
        // A second copy at the same moment queues behind the first.
        assert_eq!(pace.take(500_000, start), start + ms(1_000));
        // After a quiet spell the clock starts from now, without credit.
        let later = start + ms(5_000);
        assert_eq!(pace.take(1_000, later), later + ms(1));
    }

    #[test]
    fn waits_end_when_due_or_cancelled() {
        let pace = Pace::new(1_000_000);
        let start = Instant::now();
        assert_eq!(
            pace.wait(50_000, &mut || ScanControl::Continue),
            ScanControl::Continue
        );
        assert!(start.elapsed() >= Duration::from_millis(50));

        // Ten seconds of data, cancelled at the first check.
        let start = Instant::now();
        assert_eq!(
            pace.wait(10_000_000, &mut || ScanControl::Cancel),
            ScanControl::Cancel
        );
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
