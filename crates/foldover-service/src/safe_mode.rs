//! Crash-loop safe mode (PLAN.md §4.2): an agent that keeps stopping
//! uncleanly, say on a file that crashes it whenever a run reads it, would
//! otherwise restart into the same crash for ever. After three starts
//! that found the run before stopped uncleanly, within five minutes, it
//! syncs nothing, scheduled or asked for, until a person resumes; status
//! reports it, so the app can say why.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime};

/// Unclean stops that start safe mode, and within how long.
const UNCLEAN_STOPS: usize = 3;
const WINDOW: Duration = Duration::from_secs(5 * 60);

/// Whether the agent is in safe mode, and after how many unclean stops.
#[derive(Debug, Default)]
pub(crate) struct SafeMode(Mutex<Option<u32>>);

impl SafeMode {
    /// Enter safe mode when `stops`, the starts that found an unclean stop
    /// before them, show a crash loop at `now`.
    pub(crate) fn judge(&self, stops: &[SystemTime], now: SystemTime) {
        let recent = stops
            .iter()
            .filter(|stop| now.duration_since(**stop).map_or(true, |age| age <= WINDOW))
            .count();
        if recent >= UNCLEAN_STOPS {
            *self.lock() = Some(u32::try_from(recent).unwrap_or(u32::MAX));
        }
    }

    /// The unclean stops behind safe mode, or `None` outside it.
    pub(crate) fn active(&self) -> Option<u32> {
        *self.lock()
    }

    pub(crate) fn leave(&self) {
        *self.lock() = None;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<u32>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_unclean_stops_within_five_minutes_start_safe_mode() {
        let now = SystemTime::now();
        let ago = |seconds| now - Duration::from_secs(seconds);
        let mode = SafeMode::default();
        mode.judge(&[ago(30), ago(10)], now);
        assert_eq!(mode.active(), None);
        // Old ones do not count.
        mode.judge(&[ago(3_600), ago(30), ago(10)], now);
        assert_eq!(mode.active(), None);
        mode.judge(&[ago(240), ago(30), now], now);
        assert_eq!(mode.active(), Some(3));
        mode.leave();
        assert_eq!(mode.active(), None);
    }
}
