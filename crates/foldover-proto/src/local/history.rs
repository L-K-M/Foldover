//! Finished runs per pair, newest first.

use serde::{Deserialize, Serialize};

pub const HISTORY_LIST: &str = "history.list";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HistoryTrigger {
    Manual,
    Scheduled,
    /// An undo of the sync before it: `archived` counts what that sync
    /// created, `recovered` what came back, `failures` what was left.
    Undo,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HistoryOutcome {
    Completed,
    Cancelled,
    /// Stopped before writing: too many deletions to make without asking.
    NeedsApproval,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryRecord {
    /// Milliseconds since the Unix epoch.
    pub started_ms: u64,
    pub finished_ms: u64,
    pub trigger: HistoryTrigger,
    pub outcome: HistoryOutcome,
    pub copied: u64,
    pub updated: u64,
    pub archived: u64,
    pub recovered: u64,
    pub conflicts: u64,
    pub failures: u64,
    /// The run's folder under `.foldover/archive`, when it archived anything.
    pub archive: Option<String>,
    pub message: Option<String>,
}

/// Up to `HISTORY_LIMIT` runs, newest first.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct History {
    pub runs: Vec<HistoryRecord>,
}
