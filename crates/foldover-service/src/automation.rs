//! Pausing automatic syncs for every pair, the environment policy
//! (ADR 0036) and how hard automatic syncs work (ADR 0054); the scheduler
//! starts nothing while a pause or a hold is active. Manual syncs still
//! run, at full effort.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use foldover_engine::{Effort, Engine, Environment, EnvironmentHold, NetworkUse, Pause};
use foldover_proto::local::{
    AUTOMATION_PAUSE, AUTOMATION_RESUME, AUTOMATION_SETTINGS, AutomaticEffort, AutomationHold,
    AutomationPause, AutomationSettings, AutomationStatus, INTERNAL_ERROR, INVALID_PARAMS,
    LowPowerSyncs, MAX_PAUSE_MINUTES, Request, RpcError,
};
use serde_json::Value;

use crate::{encode, parameters};

const SECONDS_PER_MINUTE: u64 = 60;

/// A change `request` makes before the status answers it.
enum Change {
    Pause(Pause),
    Settings {
        low_power: Option<foldover_engine::LowPowerSyncs>,
        effort: Option<Effort>,
    },
}

pub(crate) async fn handle(
    engine: &Arc<Mutex<Engine>>,
    request: &Request,
) -> Result<Value, RpcError> {
    let change = match request.method() {
        AUTOMATION_PAUSE => {
            let params: AutomationPause = parameters(request)?;
            Some(match params.minutes {
                None => Change::Pause(Pause::UntilResumed),
                Some(minutes @ 1..=MAX_PAUSE_MINUTES) => Change::Pause(Pause::Until(
                    SystemTime::now()
                        + Duration::from_secs(u64::from(minutes) * SECONDS_PER_MINUTE),
                )),
                Some(_) => {
                    return Err(RpcError::new(
                        INVALID_PARAMS,
                        "Pause for 1 minute to 1 week, or until resumed",
                    ));
                }
            })
        }
        AUTOMATION_RESUME => Some(Change::Pause(Pause::Off)),
        AUTOMATION_SETTINGS => {
            let params: AutomationSettings = parameters(request)?;
            if params.low_power.is_none() && params.effort.is_none() {
                return Err(RpcError::new(INVALID_PARAMS, "Choose a setting to change"));
            }
            Some(Change::Settings {
                low_power: params.low_power.map(|syncs| match syncs {
                    LowPowerSyncs::Pause => foldover_engine::LowPowerSyncs::Pause,
                    LowPowerSyncs::Run => foldover_engine::LowPowerSyncs::Run,
                }),
                effort: params.effort.map(|effort| match effort {
                    AutomaticEffort::Gentle => Effort::Gentle,
                    AutomaticEffort::Full => Effort::Full,
                }),
            })
        }
        _ => None,
    };
    let engine = Arc::clone(engine);
    let (pause, low_power, effort, held) = tokio::task::spawn_blocking(move || {
        let mut engine = engine
            .lock()
            .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))?;
        match change {
            Some(Change::Pause(pause)) => engine.set_pause(pause).map_err(crate::engine_error)?,
            Some(Change::Settings { low_power, effort }) => {
                if let Some(syncs) = low_power {
                    engine
                        .set_low_power_syncs(syncs)
                        .map_err(crate::engine_error)?;
                }
                if let Some(effort) = effort {
                    engine
                        .set_automatic_effort(effort)
                        .map_err(crate::engine_error)?;
                }
            }
            None => {}
        }
        let pause = engine.pause().map_err(crate::engine_error)?;
        let low_power = engine.low_power_syncs().map_err(crate::engine_error)?;
        let effort = engine.automatic_effort().map_err(crate::engine_error)?;
        let pairs = engine.pairs().map_err(crate::engine_error)?;
        // The hold a sync would get now: the network's cost matters when
        // a pair uses it; local pairs only wait for Low Power Mode.
        let network = pairs
            .iter()
            .any(|pair| engine.uses_network(pair) == NetworkUse::Network);
        let environment = Environment::now();
        Ok::<_, RpcError>((
            pause,
            low_power,
            effort,
            environment.hold(
                if network {
                    NetworkUse::Network
                } else {
                    NetworkUse::Local
                },
                low_power,
            ),
        ))
    })
    .await
    .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))??;
    encode(status(pause, low_power, effort, held, SystemTime::now()))
}

fn status(
    pause: Pause,
    low_power: foldover_engine::LowPowerSyncs,
    effort: Effort,
    held: Option<EnvironmentHold>,
    now: SystemTime,
) -> AutomationStatus {
    let until_ms = match pause {
        Pause::Until(until) => until
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|since| u64::try_from(since.as_millis()).unwrap_or(u64::MAX)),
        Pause::Off | Pause::UntilResumed => None,
    };
    let paused = pause.active(now);
    AutomationStatus {
        paused,
        until_ms: until_ms.filter(|_| paused),
        low_power: match low_power {
            foldover_engine::LowPowerSyncs::Pause => LowPowerSyncs::Pause,
            foldover_engine::LowPowerSyncs::Run => LowPowerSyncs::Run,
        },
        effort: match effort {
            Effort::Gentle => AutomaticEffort::Gentle,
            Effort::Full => AutomaticEffort::Full,
        },
        held_by: held.map(|held| match held {
            EnvironmentHold::LowPowerMode => AutomationHold::LowPowerMode,
            EnvironmentHold::ExpensiveNetwork => AutomationHold::ExpensiveNetwork,
            EnvironmentHold::ConstrainedNetwork => AutomationHold::ConstrainedNetwork,
        }),
    }
}
