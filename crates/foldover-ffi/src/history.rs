//! Finished runs for Swift, newest first.

use foldover_client::{History, HistoryOutcome, HistoryTrigger, SocketLocation};

use crate::{AgentError, invalid_configuration};

#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum RunTriggerKind {
    Manual,
    Scheduled,
    /// An undo of the sync before it: `archived` counts what that sync
    /// created, `recovered` what came back, `failures` what was left.
    Undo,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum RunOutcomeKind {
    Completed,
    Cancelled,
    /// Stopped before writing: too many deletions to make without asking.
    NeedsApproval,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct RunHistoryEntry {
    /// Milliseconds since the Unix epoch.
    pub started_ms: u64,
    pub finished_ms: u64,
    pub trigger: RunTriggerKind,
    pub outcome: RunOutcomeKind,
    pub copied: u64,
    pub updated: u64,
    pub archived: u64,
    pub recovered: u64,
    pub conflicts: u64,
    pub failures: u64,
    /// The run's folder under `.foldover/archive`.
    pub archive: Option<String>,
    pub message: Option<String>,
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn pair_history(id: String) -> Result<Vec<RunHistoryEntry>, AgentError> {
    let id = id.parse().map_err(invalid_configuration)?;
    let history = foldover_client::history(&SocketLocation::Default, id)
        .await
        .map_err(AgentError::from)?;
    Ok(entries(history))
}

fn entries(history: History) -> Vec<RunHistoryEntry> {
    history
        .runs
        .into_iter()
        .map(|run| RunHistoryEntry {
            started_ms: run.started_ms,
            finished_ms: run.finished_ms,
            trigger: match run.trigger {
                HistoryTrigger::Manual => RunTriggerKind::Manual,
                HistoryTrigger::Scheduled => RunTriggerKind::Scheduled,
                HistoryTrigger::Undo => RunTriggerKind::Undo,
            },
            outcome: match run.outcome {
                HistoryOutcome::Completed => RunOutcomeKind::Completed,
                HistoryOutcome::Cancelled => RunOutcomeKind::Cancelled,
                HistoryOutcome::NeedsApproval => RunOutcomeKind::NeedsApproval,
                HistoryOutcome::Failed => RunOutcomeKind::Failed,
            },
            copied: run.copied,
            updated: run.updated,
            archived: run.archived,
            recovered: run.recovered,
            conflicts: run.conflicts,
            failures: run.failures,
            archive: run.archive,
            message: run.message,
        })
        .collect()
}
