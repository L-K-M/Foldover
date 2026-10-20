//! Pausing automatic syncs, and the environment policy, for Swift.

use foldover_client::{AutomationStatus, SocketLocation};

use crate::AgentError;

/// The person's choice for automatic syncs in Low Power Mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum LowPowerSyncs {
    Pause,
    Run,
}

/// Why automatic syncs wait right now (PLAN.md §3.5).
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum AutomationHold {
    LowPowerMode,
    ExpensiveNetwork,
    ConstrainedNetwork,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Record)]
pub struct AutomationState {
    pub paused: bool,
    /// When a timed pause ends, in milliseconds since the Unix epoch.
    pub until_ms: Option<u64>,
    pub low_power: LowPowerSyncs,
    /// Why an automatic sync would wait now, if one would.
    pub held_by: Option<AutomationHold>,
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

/// Set the person's choice for automatic syncs in Low Power Mode.
#[uniffi::export(async_runtime = "tokio")]
pub async fn set_low_power_syncs(syncs: LowPowerSyncs) -> Result<AutomationState, AgentError> {
    state(
        foldover_client::set_low_power_syncs(
            &SocketLocation::Default,
            match syncs {
                LowPowerSyncs::Pause => foldover_client::LowPowerSyncs::Pause,
                LowPowerSyncs::Run => foldover_client::LowPowerSyncs::Run,
            },
        )
        .await,
    )
}

fn state(
    status: Result<AutomationStatus, foldover_client::Error>,
) -> Result<AutomationState, AgentError> {
    let status = status.map_err(AgentError::from)?;
    Ok(AutomationState {
        paused: status.paused,
        until_ms: status.until_ms,
        low_power: match status.low_power {
            foldover_client::LowPowerSyncs::Pause => LowPowerSyncs::Pause,
            foldover_client::LowPowerSyncs::Run => LowPowerSyncs::Run,
        },
        held_by: status.held_by.map(|held| match held {
            foldover_client::AutomationHold::LowPowerMode => AutomationHold::LowPowerMode,
            foldover_client::AutomationHold::ExpensiveNetwork => AutomationHold::ExpensiveNetwork,
            foldover_client::AutomationHold::ConstrainedNetwork => {
                AutomationHold::ConstrainedNetwork
            }
        }),
    })
}
