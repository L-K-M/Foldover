//! Pausing automatic syncs, the environment policy and how hard
//! automatic syncs work, for Swift.

use foldover_client::{AutomationStatus, SocketLocation};

use crate::AgentError;

/// The person's choice for automatic syncs in Low Power Mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum LowPowerSyncs {
    Pause,
    Run,
}

/// How hard automatic syncs may work the Mac (ADR 0054). Manual syncs
/// always run at full effort.
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum AutomaticEffort {
    /// One at a time, at low priority, one file at a time on this Mac's
    /// own disks.
    Gentle,
    /// Every due pair at once, at normal priority, with every worker.
    Full,
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
    pub effort: AutomaticEffort,
    /// Why an automatic sync would wait now, if one would.
    pub held_by: Option<AutomationHold>,
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn automation_state() -> Result<AutomationState, AgentError> {
    state(
        foldover_client::automation_status(&SocketLocation::Default).await,
        AgentError::read,
    )
}

/// Pause automatic syncs for `minutes`, or until resumed when absent.
#[uniffi::export(async_runtime = "tokio")]
pub async fn pause_automation(minutes: Option<u32>) -> Result<AutomationState, AgentError> {
    state(
        foldover_client::pause_automation(&SocketLocation::Default, minutes).await,
        AgentError::from,
    )
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn resume_automation() -> Result<AutomationState, AgentError> {
    state(
        foldover_client::resume_automation(&SocketLocation::Default).await,
        AgentError::from,
    )
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
        AgentError::from,
    )
}

/// Set how hard automatic syncs may work the Mac.
#[uniffi::export(async_runtime = "tokio")]
pub async fn set_automatic_effort(effort: AutomaticEffort) -> Result<AutomationState, AgentError> {
    state(
        foldover_client::set_automatic_effort(
            &SocketLocation::Default,
            match effort {
                AutomaticEffort::Gentle => foldover_client::AutomaticEffort::Gentle,
                AutomaticEffort::Full => foldover_client::AutomaticEffort::Full,
            },
        )
        .await,
        AgentError::from,
    )
}

fn state(
    status: Result<AutomationStatus, foldover_client::Error>,
    error: impl FnOnce(foldover_client::Error) -> AgentError,
) -> Result<AutomationState, AgentError> {
    let status = status.map_err(error)?;
    Ok(AutomationState {
        paused: status.paused,
        until_ms: status.until_ms,
        low_power: match status.low_power {
            foldover_client::LowPowerSyncs::Pause => LowPowerSyncs::Pause,
            foldover_client::LowPowerSyncs::Run => LowPowerSyncs::Run,
        },
        effort: match status.effort {
            foldover_client::AutomaticEffort::Gentle => AutomaticEffort::Gentle,
            foldover_client::AutomaticEffort::Full => AutomaticEffort::Full,
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
