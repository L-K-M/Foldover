//! Pairing with other Macs (ADR 0031). The peer link runs on the agent's
//! runtime beside the engine; the engine lock is taken only to read or
//! record devices, and never across an await.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, UNIX_EPOCH};

use foldover_engine::{
    Answer, Decision, Device, Engine, FolderCache, LinkError, NotRecorded, PeerHost, PeerIdentity,
    PeerLink, PendingPairing, Presence,
};
use foldover_proto::local::{
    INTERNAL_ERROR, INVALID_PARAMS, MAX_PAIRING_MINUTES, PEER_CANCEL, PEER_CONFIRM, PEER_PAIR,
    PEER_PAIRING_CLOSE, PEER_PAIRING_OPEN, PEER_REVOKE, PEER_SHARES, PEER_STATUS, PEER_UNAVAILABLE,
    PeerDeviceRecord, PeerKey, PeerNearbyRecord, PeerPair, PeerPairingOpen, PeerPendingRecord,
    PeerShareList, PeerShareRecord, PeerStatus, Request, RpcError,
};
use foldover_proto::peer::{OfferedShare, PeerCall, PeerReply};
use foldover_types::DeviceId;
use serde_json::Value;
use tokio::sync::OnceCell;

use crate::safe_mode::SafeMode;
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
    safe_mode: &Arc<SafeMode>,
) -> Result<(), RpcError> {
    if devices(engine).await?.is_empty() {
        return Ok(());
    }
    link(engine, peers, safe_mode).await.map(drop)
}

/// The running link, started with this Mac's identity (the Keychain's on
/// macOS) on first use.
async fn link<'a>(
    engine: &Arc<Mutex<Engine>>,
    peers: &'a Peers,
    safe_mode: &Arc<SafeMode>,
) -> Result<&'a Arc<PeerLink>, RpcError> {
    peers
        .get_or_try_init(|| async {
            let identity = tokio::task::spawn_blocking(foldover_engine::peer_identity)
                .await
                .map_err(|_| RpcError::new(INTERNAL_ERROR, "The peer link failed to start"))?
                .map_err(unavailable)?;
            open(engine, identity, LISTEN, Presence::Announced, safe_mode).await
        })
        .await
}

/// A peer link as `identity` on `address`, trusting the devices recorded
/// earlier and recording the ones both people confirm.
pub(crate) async fn open(
    engine: &Arc<Mutex<Engine>>,
    identity: PeerIdentity,
    address: SocketAddr,
    presence: Presence,
    safe_mode: &Arc<SafeMode>,
) -> Result<Arc<PeerLink>, RpcError> {
    let paired = devices(engine).await?;
    let link = PeerLink::start(
        identity,
        foldover_engine::computer_name(),
        address,
        presence,
        paired.iter().map(|device| device.id),
        Arc::new(Host {
            engine: Arc::clone(engine),
            folders: FolderCache::default(),
            safe_mode: Arc::clone(safe_mode),
        }),
    )
    .map_err(unavailable)?;
    // Pairs with a folder on another Mac reach it through this link.
    engine
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .reach_peers(&link);
    Ok(link)
}

/// This agent's side of the link: it records pairings, and tells each
/// paired Mac only what this Mac shares with it.
struct Host {
    engine: Arc<Mutex<Engine>>,
    folders: FolderCache,
    /// In safe mode this Mac's folders are not served either: the work
    /// that keeps crashing the agent may be another Mac's sync.
    safe_mode: Arc<SafeMode>,
}

impl PeerHost for Host {
    fn paired(&self, device: DeviceId, name: String) -> Result<(), NotRecorded> {
        self.engine
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .add_device(device, name)
            .map_err(|error| {
                eprintln!("foldoverd: could not record the paired Mac {device}: {error}");
                NotRecorded
            })
    }

    fn serve(&self, device: DeviceId, call: PeerCall, answer: &mut Answer) {
        let refused = |reason: &str| PeerReply::Refused {
            reason: reason.to_owned(),
        };
        match call {
            PeerCall::Shares => {
                let shares = self
                    .engine
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .shares_for(device);
                let reply = match shares {
                    Ok(shares) => PeerReply::Shares {
                        shares: shares
                            .into_iter()
                            .map(|share| OfferedShare {
                                id: share.id.to_string(),
                                name: share.name,
                            })
                            .collect(),
                    },
                    Err(_) => refused("That Mac could not read its shared folders"),
                };
                let _ = answer.reply(&reply);
            }
            PeerCall::Folder { .. } if self.safe_mode.active().is_some() => {
                let _ = answer.reply(&refused(
                    "That Mac paused syncing after its agent stopped unexpectedly; resume it \
                     there",
                ));
            }
            PeerCall::Folder { share, op } => {
                // Only what is shared with the asking Mac; the engine lock is
                // released before the folder is touched. The agent's own
                // state directory stays protected wherever it is shared.
                let granted = share.parse().ok().and_then(|share| {
                    let engine = self.engine.lock().unwrap_or_else(PoisonError::into_inner);
                    engine
                        .shared_folder(device, share)
                        .ok()
                        .flatten()
                        .map(|root| (root, engine.state_path().to_path_buf()))
                });
                match granted {
                    Some((root, state)) => foldover_engine::serve_folder(
                        &root,
                        *op,
                        &self.folders,
                        answer,
                        Some(&state),
                    ),
                    None => {
                        let _ = answer.reply(&refused("That folder is not shared with this Mac"));
                    }
                }
            }
        }
    }
}

pub(crate) async fn handle(
    engine: &Arc<Mutex<Engine>>,
    peers: &Peers,
    safe_mode: &Arc<SafeMode>,
    request: &Request,
) -> Result<Value, RpcError> {
    if request.method() == PEER_STATUS {
        return match peers.get() {
            Some(link) => encode(status(engine, link).await?),
            None => encode(off(engine).await?),
        };
    }
    let link = link(engine, peers, safe_mode).await?;
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
            let address = params
                .address
                .map(|address| address.parse::<SocketAddr>())
                .transpose()
                .map_err(|_| {
                    RpcError::new(
                        INVALID_PARAMS,
                        "An address is ip:port, e.g. 192.168.1.5:50123",
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
        PEER_SHARES => {
            let params: PeerKey = parameters(request)?;
            let shares = match link
                .request(device(&params.device)?, PeerCall::Shares)
                .await
                .map_err(unavailable)?
            {
                PeerReply::Shares { shares } => shares,
                PeerReply::Refused { reason } => {
                    return Err(RpcError::new(PEER_UNAVAILABLE, reason));
                }
                _ => {
                    return Err(RpcError::new(
                        PEER_UNAVAILABLE,
                        "That Mac answered out of order",
                    ));
                }
            };
            return encode(PeerShareList {
                shares: shares
                    .into_iter()
                    .map(|share| PeerShareRecord {
                        id: share.id,
                        name: share.name,
                    })
                    .collect(),
            });
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
    let (nearby, discovery) = match link.nearby() {
        Ok(nearby) => (nearby, None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };
    Ok(PeerStatus {
        nearby: nearby
            .into_iter()
            .map(|nearby| PeerNearbyRecord {
                device: nearby.device.to_string(),
                name: nearby.name,
                address: nearby.address.to_string(),
                pairing: nearby.pairing.map(|address| address.to_string()),
                paired: link.is_paired(nearby.device),
            })
            .collect(),
        discovery,
        device: Some(link.device().to_string()),
        name: foldover_engine::computer_name(),
        address: Some(link.address().map_err(unavailable)?.to_string()),
        pairing: link.pairing_address().map(|address| address.to_string()),
        pending: link.pending().iter().map(record).collect(),
        devices: devices.iter().map(device_record).collect(),
    })
}

/// Status before Mac-to-Mac is on: only the devices recorded earlier.
async fn off(engine: &Arc<Mutex<Engine>>) -> Result<PeerStatus, RpcError> {
    Ok(PeerStatus {
        device: None,
        name: foldover_engine::computer_name(),
        address: None,
        pairing: None,
        pending: Vec::new(),
        devices: devices(engine).await?.iter().map(device_record).collect(),
        nearby: Vec::new(),
        discovery: None,
    })
}

fn device_record(device: &Device) -> PeerDeviceRecord {
    PeerDeviceRecord {
        device: device.id.to_string(),
        name: device.name.clone(),
        paired_ms: device.paired.duration_since(UNIX_EPOCH).map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        }),
    }
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
