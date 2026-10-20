//! Folders this Mac shares with paired Macs (ADR 0031). The engine
//! checks that the device is paired and the folder exists.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::UNIX_EPOCH;

use foldover_engine::{Engine, Share};
use foldover_proto::local::{
    INTERNAL_ERROR, INVALID_PARAMS, Request, RpcError, SHARE_ADD, SHARE_REMOVE, ShareAdd, ShareKey,
    ShareList, ShareRecord,
};
use foldover_types::{DeviceId, LocalRoot, ShareId};
use serde_json::Value;

use crate::{encode, parameters};

pub(crate) async fn handle(
    engine: &Arc<Mutex<Engine>>,
    request: &Request,
) -> Result<Value, RpcError> {
    let change = match request.method() {
        SHARE_ADD => {
            let params: ShareAdd = parameters(request)?;
            let device: DeviceId = params
                .device
                .parse()
                .map_err(|error| RpcError::new(INVALID_PARAMS, format!("{error}")))?;
            let root = LocalRoot::from_bytes(params.root).map_err(|_| {
                RpcError::new(INVALID_PARAMS, "Share a folder by its absolute path")
            })?;
            Change::Add(device, root, params.name)
        }
        SHARE_REMOVE => {
            let params: ShareKey = parameters(request)?;
            let id: ShareId = params
                .id
                .parse()
                .map_err(|error| RpcError::new(INVALID_PARAMS, format!("{error}")))?;
            Change::Remove(id)
        }
        _ => Change::List,
    };
    let engine = Arc::clone(engine);
    let outcome = tokio::task::spawn_blocking(move || {
        let mut engine = engine.lock().unwrap_or_else(PoisonError::into_inner);
        match change {
            Change::Add(device, root, name) => {
                engine.add_share(device, root, &name).map(Outcome::Added)
            }
            Change::Remove(id) => engine
                .remove_share(id)
                .and_then(|_| engine.shares())
                .map(Outcome::Listed),
            Change::List => engine.shares().map(Outcome::Listed),
        }
        .map_err(crate::engine_error)
    })
    .await
    .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))??;
    match outcome {
        Outcome::Added(share) => encode(record(&share)),
        Outcome::Listed(shares) => encode(ShareList {
            shares: shares.iter().map(record).collect(),
        }),
    }
}

/// What a request changes.
enum Change {
    List,
    Add(DeviceId, LocalRoot, String),
    Remove(ShareId),
}

/// The new share, or every share after the change.
enum Outcome {
    Added(Share),
    Listed(Vec<Share>),
}

fn record(share: &Share) -> ShareRecord {
    ShareRecord {
        id: share.id.to_string(),
        device: share.device.to_string(),
        root: share.root.as_bytes().to_vec(),
        name: share.name.clone(),
        created_ms: share.created.duration_since(UNIX_EPOCH).map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        }),
    }
}
