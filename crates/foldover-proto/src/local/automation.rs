//! Pausing automatic syncs for every pair, the environment policy
//! (ADR 0036) and how hard automatic syncs work (ADR 0054). Manual syncs
//! still run, at full effort.

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

/// How hard automatic syncs may work the Mac (ADR 0054).
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomaticEffort {
    /// One at a time and at low priority, reading and copying one file at
    /// a time where this Mac's own disks limit the work.
    #[default]
    Gentle,
    /// Every due pair at once, at normal priority, with every worker.
    Full,
}

/// The person's automation choices, set with `automation.settings`. Each
/// field present changes that choice; at least one must be.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low_power: Option<LowPowerSyncs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<AutomaticEffort>,
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
    pub effort: AutomaticEffort,
    /// Why an automatic sync would wait now, if one would.
    pub held_by: Option<AutomationHold>,
}
