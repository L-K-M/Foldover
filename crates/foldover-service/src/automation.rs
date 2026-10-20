//! Pausing automatic syncs for every pair, and the environment policy
//! (ADR 0036); the scheduler starts nothing while a pause or a hold is
//! active. Manual syncs still run.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use foldover_engine::{Engine, Environment, EnvironmentHold, NetworkUse, Pause};
use foldover_proto::local::{
    AUTOMATION_PAUSE, AUTOMATION_RESUME, AUTOMATION_SETTINGS, AutomationHold, AutomationPause,
    AutomationSettings, AutomationStatus, INTERNAL_ERROR, INVALID_PARAMS, LowPowerSyncs,
    MAX_PAUSE_MINUTES, Request, RpcError,
};
use serde_json::Value;

use crate::{encode, parameters};

const SECONDS_PER_MINUTE: u64 = 60;

/// A change `request` makes before the status answers it.
enum Change {
    Pause(Pause),
    LowPower(foldover_engine::LowPowerSyncs),
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
            Some(Change::LowPower(match params.low_power {
                LowPowerSyncs::Pause => foldover_engine::LowPowerSyncs::Pause,
                LowPowerSyncs::Run => foldover_engine::LowPowerSyncs::Run,
            }))
        }
        _ => None,
    };
    let engine = Arc::clone(engine);
    let (pause, low_power, held) = tokio::task::spawn_blocking(move || {
        let mut engine = engine
            .lock()
            .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))?;
        match change {
            Some(Change::Pause(pause)) => engine.set_pause(pause).map_err(crate::engine_error)?,
            Some(Change::LowPower(syncs)) => engine
                .set_low_power_syncs(syncs)
                .map_err(crate::engine_error)?,
            None => {}
        }
        let pause = engine.pause().map_err(crate::engine_error)?;
        let low_power = engine.low_power_syncs().map_err(crate::engine_error)?;
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
    encode(status(pause, low_power, held, SystemTime::now()))
}

fn status(
    pause: Pause,
    low_power: foldover_engine::LowPowerSyncs,
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
        held_by: held.map(|held| match held {
            EnvironmentHold::LowPowerMode => AutomationHold::LowPowerMode,
            EnvironmentHold::ExpensiveNetwork => AutomationHold::ExpensiveNetwork,
            EnvironmentHold::ConstrainedNetwork => AutomationHold::ConstrainedNetwork,
        }),
    }
}
