//! Pausing automatic syncs for every pair. Manual syncs still run.

use serde::{Deserialize, Serialize};

pub const AUTOMATION_STATUS: &str = "automation.status";
pub const AUTOMATION_PAUSE: &str = "automation.pause";
pub const AUTOMATION_RESUME: &str = "automation.resume";
/// The longest timed pause: one week.
pub const MAX_PAUSE_MINUTES: u32 = 10_080;

/// Pause for `minutes`, or until resumed when absent.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationPause {
    pub minutes: Option<u32>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationStatus {
    pub paused: bool,
    /// When a timed pause ends, in milliseconds since the Unix epoch.
    pub until_ms: Option<u64>,
}
