//! Pausing automatic syncs for every pair, and the environment policy
//! (ADR 0036). Manual syncs still run.

use serde::{Deserialize, Serialize};

pub const AUTOMATION_STATUS: &str = "automation.status";
pub const AUTOMATION_PAUSE: &str = "automation.pause";
pub const AUTOMATION_RESUME: &str = "automation.resume";
pub const AUTOMATION_SETTINGS: &str = "automation.settings";
/// The longest timed pause: one week.
pub const MAX_PAUSE_MINUTES: u32 = 10_080;

/// Pause for `minutes`, or until resumed when absent.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationPause {
    pub minutes: Option<u32>,
}

/// The person's choice for automatic syncs in Low Power Mode.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LowPowerSyncs {
    #[default]
    Pause,
    Run,
}

/// The person's automation choices, set with `automation.settings`.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationSettings {
    pub low_power: LowPowerSyncs,
}

/// Why automatic syncs wait right now (PLAN.md §3.5).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AutomationHold {
    LowPowerMode,
    ExpensiveNetwork,
    ConstrainedNetwork,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationStatus {
    pub paused: bool,
    /// When a timed pause ends, in milliseconds since the Unix epoch.
    pub until_ms: Option<u64>,
    pub low_power: LowPowerSyncs,
    /// Why an automatic sync would wait now, if one would.
    pub held_by: Option<AutomationHold>,
}
