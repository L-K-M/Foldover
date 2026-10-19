//! Pausing automatic syncs for every pair; the scheduler starts nothing
//! while a pause is active. Manual syncs still run.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use foldover_engine::{Engine, Pause};
use foldover_proto::local::{
    AUTOMATION_PAUSE, AUTOMATION_RESUME, AutomationPause, AutomationStatus, INTERNAL_ERROR,
    INVALID_PARAMS, MAX_PAUSE_MINUTES, Request, RpcError,
};
use serde_json::Value;

use crate::{encode, parameters};

const SECONDS_PER_MINUTE: u64 = 60;

pub(crate) async fn handle(
    engine: &Arc<Mutex<Engine>>,
    request: &Request,
) -> Result<Value, RpcError> {
    let change = match request.method() {
        AUTOMATION_PAUSE => {
            let params: AutomationPause = parameters(request)?;
            Some(match params.minutes {
                None => Pause::UntilResumed,
                Some(minutes @ 1..=MAX_PAUSE_MINUTES) => Pause::Until(
                    SystemTime::now()
                        + Duration::from_secs(u64::from(minutes) * SECONDS_PER_MINUTE),
                ),
                Some(_) => {
                    return Err(RpcError::new(
                        INVALID_PARAMS,
                        "Pause for 1 minute to 1 week, or until resumed",
                    ));
                }
            })
        }
        AUTOMATION_RESUME => Some(Pause::Off),
        _ => None,
    };
    let engine = Arc::clone(engine);
    let pause = tokio::task::spawn_blocking(move || {
        let mut engine = engine
            .lock()
            .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))?;
        if let Some(pause) = change {
            engine.set_pause(pause).map_err(crate::engine_error)?;
        }
        engine.pause().map_err(crate::engine_error)
    })
    .await
    .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))??;
    encode(status(pause, SystemTime::now()))
}

fn status(pause: Pause, now: SystemTime) -> AutomationStatus {
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
    }
}
