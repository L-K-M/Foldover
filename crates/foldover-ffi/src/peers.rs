//! Pairing with other Macs, for Swift (ADR 0031). Devices are 64 hex
//! digits, as the agent reports them.

use foldover_client::{DeviceId, PeerPendingRecord, PeerStatus, SocketLocation};

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
