//! Pausing automatic syncs, for Swift.

use foldover_client::{AutomationStatus, SocketLocation};

use crate::AgentError;

#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Record)]
pub struct AutomationState {
    pub paused: bool,
    /// When a timed pause ends, in milliseconds since the Unix epoch.
    pub until_ms: Option<u64>,
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn automation_state() -> Result<AutomationState, AgentError> {
    state(foldover_client::automation_status(&SocketLocation::Default).await)
}

/// Pause automatic syncs for `minutes`, or until resumed when absent.
#[uniffi::export(async_runtime = "tokio")]
pub async fn pause_automation(minutes: Option<u32>) -> Result<AutomationState, AgentError> {
    state(foldover_client::pause_automation(&SocketLocation::Default, minutes).await)
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn resume_automation() -> Result<AutomationState, AgentError> {
    state(foldover_client::resume_automation(&SocketLocation::Default).await)
}

fn state(
    status: Result<AutomationStatus, foldover_client::Error>,
) -> Result<AutomationState, AgentError> {
    let status = status.map_err(AgentError::from)?;
    Ok(AutomationState {
        paused: status.paused,
        until_ms: status.until_ms,
    })
}
