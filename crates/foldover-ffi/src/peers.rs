//! Pairing with other Macs, for Swift (ADR 0031). Devices are 64 hex
//! digits, as the agent reports them.

use foldover_client::{DeviceId, PeerPendingRecord, PeerStatus, ShareRecord, SocketLocation};

use crate::AgentError;

/// Mac-to-Mac on this Mac, as last reported.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct MacToMac {
    /// This Mac's device ID once Mac-to-Mac is on; None before.
    pub device: Option<String>,
    /// This Mac's name, as other Macs see it.
    pub name: String,
    /// Whether other Macs can pair with this one now.
    pub pairing_open: bool,
    /// Codes waiting for a person on this Mac to compare.
    pub pending: Vec<PairingCode>,
    pub paired: Vec<PairedMac>,
    /// Other Macs running Foldover on this network.
    pub nearby: Vec<NearbyMac>,
    /// Why Macs on the network cannot be seen, if they cannot.
    pub discovery_problem: Option<String>,
}

/// A six-digit code both Macs show; show it with leading zeros.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct PairingCode {
    pub device: String,
    pub name: String,
    pub code: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct PairedMac {
    pub device: String,
    pub name: String,
    /// Milliseconds since the Unix epoch.
    pub paired_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct NearbyMac {
    pub device: String,
    pub name: String,
    pub open_for_pairing: bool,
    pub paired: bool,
}

/// Never turns Mac-to-Mac on, so it never shows the Local Network prompt.
#[uniffi::export(async_runtime = "tokio")]
pub async fn mac_to_mac_status() -> Result<MacToMac, AgentError> {
    state(foldover_client::peer_status(&SocketLocation::Default).await)
}

/// Turn Mac-to-Mac on: listen, and look for Macs on the network.
#[uniffi::export(async_runtime = "tokio")]
pub async fn start_mac_to_mac() -> Result<MacToMac, AgentError> {
    state(foldover_client::start_peers(&SocketLocation::Default).await)
}

/// Let other Macs pair with this one for `minutes` (10 when absent).
#[uniffi::export(async_runtime = "tokio")]
pub async fn open_mac_pairing(minutes: Option<u32>) -> Result<MacToMac, AgentError> {
    state(foldover_client::open_pairing(&SocketLocation::Default, minutes).await)
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn close_mac_pairing() -> Result<MacToMac, AgentError> {
    state(foldover_client::close_pairing(&SocketLocation::Default).await)
}

/// Start pairing with a nearby Mac that is open for pairing.
#[uniffi::export(async_runtime = "tokio")]
pub async fn pair_with_mac(device: String) -> Result<PairingCode, AgentError> {
    let pending = foldover_client::pair_device(&SocketLocation::Default, parse(&device)?, None)
        .await
        .map_err(AgentError::from)?;
    Ok(code(pending))
}

/// The person saw the same code on both Macs.
#[uniffi::export(async_runtime = "tokio")]
pub async fn confirm_mac_pairing(device: String) -> Result<MacToMac, AgentError> {
    state(foldover_client::confirm_pairing(&SocketLocation::Default, parse(&device)?).await)
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn cancel_mac_pairing(device: String) -> Result<MacToMac, AgentError> {
    state(foldover_client::cancel_pairing(&SocketLocation::Default, parse(&device)?).await)
}

/// Forget a paired Mac and close its connections.
#[uniffi::export(async_runtime = "tokio")]
pub async fn forget_mac(device: String) -> Result<MacToMac, AgentError> {
    state(foldover_client::revoke_device(&SocketLocation::Default, parse(&device)?).await)
}

fn parse(device: &str) -> Result<DeviceId, AgentError> {
    device.parse().map_err(|_| AgentError::Request {
        message: "Invalid device".into(),
    })
}

fn code(pending: PeerPendingRecord) -> PairingCode {
    PairingCode {
        device: pending.device,
        name: pending.name,
        code: pending.code,
    }
}

fn state(status: Result<PeerStatus, foldover_client::Error>) -> Result<MacToMac, AgentError> {
    let status = status.map_err(AgentError::from)?;
    Ok(MacToMac {
        device: status.device,
        name: status.name,
        pairing_open: status.pairing.is_some(),
        pending: status.pending.into_iter().map(code).collect(),
        paired: status
            .devices
            .into_iter()
            .map(|device| PairedMac {
                device: device.device,
                name: device.name,
                paired_ms: device.paired_ms,
            })
            .collect(),
        nearby: status
            .nearby
            .into_iter()
            .map(|nearby| NearbyMac {
                device: nearby.device,
                name: nearby.name,
                open_for_pairing: nearby.pairing.is_some(),
                paired: nearby.paired,
            })
            .collect(),
        discovery_problem: status.discovery,
    })
}

/// A folder this Mac shares with one paired Mac.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct SharedFolder {
    pub id: String,
    /// The paired Mac that may sync with it.
    pub device: String,
    /// Absolute path bytes.
    pub path: Vec<u8>,
    /// What that Mac shows, e.g. "Photos".
    pub name: String,
}

/// A folder a paired Mac shares with this one.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct OfferedFolder {
    pub id: String,
    pub name: String,
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn shared_folders() -> Result<Vec<SharedFolder>, AgentError> {
    let listed = foldover_client::shares(&SocketLocation::Default)
        .await
        .map_err(AgentError::from)?;
    Ok(listed.shares.into_iter().map(shared).collect())
}

/// Share the folder at `path` with paired `device`, shown there as `name`.
#[uniffi::export(async_runtime = "tokio")]
pub async fn share_folder(
    device: String,
    path: Vec<u8>,
    name: String,
) -> Result<SharedFolder, AgentError> {
    let record =
        foldover_client::add_share(&SocketLocation::Default, parse(&device)?, &path, &name)
            .await
            .map_err(AgentError::from)?;
    Ok(shared(record))
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn stop_sharing(id: String) -> Result<Vec<SharedFolder>, AgentError> {
    let id = id.parse().map_err(|_| AgentError::Request {
        message: "Invalid shared folder".into(),
    })?;
    let listed = foldover_client::remove_share(&SocketLocation::Default, id)
        .await
        .map_err(AgentError::from)?;
    Ok(listed.shares.into_iter().map(shared).collect())
}

/// The folders paired `device` shares with this Mac; asks that Mac.
#[uniffi::export(async_runtime = "tokio")]
pub async fn offered_folders(device: String) -> Result<Vec<OfferedFolder>, AgentError> {
    let offered = foldover_client::peer_shares(&SocketLocation::Default, parse(&device)?)
        .await
        .map_err(AgentError::from)?;
    Ok(offered
        .shares
        .into_iter()
        .map(|share| OfferedFolder {
            id: share.id,
            name: share.name,
        })
        .collect())
}

fn shared(record: ShareRecord) -> SharedFolder {
    SharedFolder {
        id: record.id,
        device: record.device,
        path: record.root,
        name: record.name,
    }
}
