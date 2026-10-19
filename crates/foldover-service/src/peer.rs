//! Pairing with other Macs (ADR 0031). The peer link runs on the agent's
//! runtime beside the engine; the engine lock is taken only to read or
//! record devices, and never across an await.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, UNIX_EPOCH};

use foldover_engine::{
    Decision, Device, Engine, LinkError, PeerIdentity, PeerLink, PendingPairing,
};
use foldover_proto::local::{
    INTERNAL_ERROR, INVALID_PARAMS, MAX_PAIRING_MINUTES, PEER_CANCEL, PEER_CONFIRM, PEER_PAIR,
    PEER_PAIRING_CLOSE, PEER_PAIRING_OPEN, PEER_REVOKE, PEER_UNAVAILABLE, PeerDeviceRecord,
    PeerKey, PeerPair, PeerPairingOpen, PeerPendingRecord, PeerStatus, Request, RpcError,
};
use foldover_types::DeviceId;
use serde_json::Value;
use tokio::sync::OnceCell;

use crate::{encode, parameters};

const DEFAULT_PAIRING_MINUTES: u32 = 10;
const SECONDS_PER_MINUTE: u64 = 60;

/// Every IPv4 interface, on a port the system picks.
const LISTEN: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);

/// This agent's peer link, once started. It starts at launch only when
/// Macs are paired, otherwise on the first `peer.*` request, so a Mac
/// that never pairs never listens on the network.
pub(crate) type Peers = Arc<OnceCell<Arc<PeerLink>>>;

/// Listen for paired Macs, if this Mac has any.
pub(crate) async fn start_if_paired(
    engine: &Arc<Mutex<Engine>>,
    peers: &Peers,
) -> Result<(), RpcError> {
    if devices(engine).await?.is_empty() {
        return Ok(());
    }
    link(engine, peers).await.map(drop)
}

/// The running link, started with this Mac's identity (the Keychain's on
/// macOS) on first use.
async fn link<'a>(
    engine: &Arc<Mutex<Engine>>,
    peers: &'a Peers,
) -> Result<&'a Arc<PeerLink>, RpcError> {
    peers
        .get_or_try_init(|| async {
            let identity = tokio::task::spawn_blocking(foldover_engine::peer_identity)
                .await
                .map_err(|_| RpcError::new(INTERNAL_ERROR, "The peer link failed to start"))?
                .map_err(unavailable)?;
            open(engine, identity, LISTEN).await
        })
        .await
}

/// A peer link as `identity` on `address`, trusting the devices recorded
/// earlier and recording the ones both people confirm.
pub(crate) async fn open(
    engine: &Arc<Mutex<Engine>>,
    identity: PeerIdentity,
    address: SocketAddr,
) -> Result<Arc<PeerLink>, RpcError> {
    let paired = devices(engine).await?;
    let recording = Arc::clone(engine);
    PeerLink::start(
        identity,
        foldover_engine::computer_name(),
        address,
        paired.iter().map(|device| device.id),
        move |device, name| {
            let _ = recording
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .add_device(device, name);
        },
    )
    .map_err(unavailable)
}

pub(crate) async fn handle(
    engine: &Arc<Mutex<Engine>>,
    peers: &Peers,
    request: &Request,
) -> Result<Value, RpcError> {
    let link = link(engine, peers).await?;
    match request.method() {
        PEER_PAIRING_OPEN => {
            let params: PeerPairingOpen = parameters(request)?;
            let minutes = match params.minutes {
                None => DEFAULT_PAIRING_MINUTES,
                Some(minutes @ 1..=MAX_PAIRING_MINUTES) => minutes,
                Some(_) => {
                    return Err(RpcError::new(
                        INVALID_PARAMS,
                        "Open pairing for 1 to 60 minutes",
                    ));
                }
            };
            link.open_pairing(Duration::from_secs(u64::from(minutes) * SECONDS_PER_MINUTE))
                .map_err(unavailable)?;
        }
        PEER_PAIRING_CLOSE => link.close_pairing(),
        PEER_PAIR => {
            let params: PeerPair = parameters(request)?;
            let device = device(&params.device)?;
            let address: SocketAddr = params.address.parse().map_err(|_| {
                RpcError::new(
                    INVALID_PARAMS,
                    "An address is host:port, e.g. 192.168.1.5:50123",
                )
            })?;
            let pending = link.pair(device, address).await.map_err(unavailable)?;
            return encode(record(&pending));
        }
        PEER_CONFIRM | PEER_CANCEL => {
            let params: PeerKey = parameters(request)?;
            let decision = if request.method() == PEER_CONFIRM {
                Decision::Confirm
            } else {
                Decision::Cancel
            };
            link.decide(device(&params.device)?, decision)
                .map_err(|error| RpcError::new(INVALID_PARAMS, error.to_string()))?;
        }
        PEER_REVOKE => {
            let params: PeerKey = parameters(request)?;
            let device = device(&params.device)?;
            link.revoke(device);
            let engine = Arc::clone(engine);
            tokio::task::spawn_blocking(move || {
                engine
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove_device(device)
                    .map_err(crate::engine_error)
            })
            .await
            .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))??;
        }
        _ => {}
    }
    encode(status(engine, link).await?)
}

async fn status(engine: &Arc<Mutex<Engine>>, link: &PeerLink) -> Result<PeerStatus, RpcError> {
    let devices = devices(engine).await?;
    Ok(PeerStatus {
        device: link.device().to_string(),
        name: foldover_engine::computer_name(),
        address: link.address().map_err(unavailable)?.to_string(),
        pairing: link.pairing_address().map(|address| address.to_string()),
        pending: link.pending().iter().map(record).collect(),
        devices: devices
            .into_iter()
            .map(|device| PeerDeviceRecord {
                device: device.id.to_string(),
                name: device.name,
                paired_ms: device.paired.duration_since(UNIX_EPOCH).map_or(0, |since| {
                    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
                }),
            })
            .collect(),
    })
}

async fn devices(engine: &Arc<Mutex<Engine>>) -> Result<Vec<Device>, RpcError> {
    let engine = Arc::clone(engine);
    tokio::task::spawn_blocking(move || {
        engine
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .devices()
            .map_err(crate::engine_error)
    })
    .await
    .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))?
}

fn record(pending: &PendingPairing) -> PeerPendingRecord {
    PeerPendingRecord {
        device: pending.device.to_string(),
        name: pending.name.clone(),
        code: pending.code,
    }
}

fn device(text: &str) -> Result<DeviceId, RpcError> {
    text.parse()
        .map_err(|error: foldover_types::InvalidDeviceId| {
            RpcError::new(INVALID_PARAMS, error.to_string())
        })
}

fn unavailable(error: LinkError) -> RpcError {
    RpcError::new(PEER_UNAVAILABLE, error.to_string())
}
