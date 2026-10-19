//! `foldover-host` — domain services layer. Environment policy:
//! battery, Low Power Mode, metered networks and power assertions.
//! Depends on foldover-os. See PLAN.md §4.4.

pub use foldover_os::computer_name;

use std::io;

use foldover_os::IdleSleepAssertion;

/// Holds the Mac awake for one sync (PLAN.md §3.5) until dropped. A Mac
/// put to sleep on purpose still sleeps; the run then stops like any
/// interrupted run and the next one finishes the work.
pub struct RunAwake {
    /// None where the system has no power assertions, such as Linux:
    /// nothing there idles a running agent to sleep on Foldover's behalf.
    _assertion: Option<IdleSleepAssertion>,
}

/// Keep the Mac awake while `pair` syncs. Fails only where power
/// assertions exist and taking one failed.
pub fn keep_awake(pair: &str) -> io::Result<RunAwake> {
    Ok(RunAwake {
        _assertion: IdleSleepAssertion::take(&format!("Foldover is syncing {pair}"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeping_awake_succeeds_with_or_without_power_assertions() {
        let awake = keep_awake("Photos").unwrap();
        assert_eq!(awake._assertion.is_some(), cfg!(target_os = "macos"));
    }
}
