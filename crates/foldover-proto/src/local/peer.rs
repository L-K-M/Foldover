//! Pairing with other Macs (ADR 0031). Device IDs are 64 lowercase hex
//! digits; addresses are `host:port`.

use serde::{Deserialize, Serialize};

/// Never starts Mac-to-Mac, so it never triggers the Local Network prompt.
pub const PEER_STATUS: &str = "peer.status";
/// Start listening and looking for Macs on the network.
pub const PEER_START: &str = "peer.start";
pub const PEER_PAIRING_OPEN: &str = "peer.pairingOpen";
pub const PEER_PAIRING_CLOSE: &str = "peer.pairingClose";
pub const PEER_PAIR: &str = "peer.pair";
pub const PEER_CONFIRM: &str = "peer.confirm";
pub const PEER_CANCEL: &str = "peer.cancel";
pub const PEER_REVOKE: &str = "peer.revoke";
/// The longest pairing window: one hour.
pub const MAX_PAIRING_MINUTES: u32 = 60;

/// Let other Macs pair with this one for `minutes` (10 when absent).
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerPairingOpen {
    pub minutes: Option<u32>,
}

/// Start pairing with `device`, whose pairing window listens at `address`,
/// or where the local network says when absent.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerPair {
    pub device: String,
    pub address: Option<String>,
}

/// Name a device for confirm, cancel or revoke.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerKey {
    pub device: String,
}

/// A code waiting for this Mac's person to compare and confirm.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerPendingRecord {
    pub device: String,
    pub name: String,
    /// Six digits; show with leading zeros.
    pub code: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerDeviceRecord {
    pub device: String,
    pub name: String,
    /// Milliseconds since the Unix epoch.
    pub paired_ms: u64,
}

/// Another Mac running Foldover on the local network.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerNearbyRecord {
    pub device: String,
    pub name: String,
    pub address: String,
    /// Where it accepts pairing, while a window is open.
    pub pairing: Option<String>,
    /// Whether this Mac has paired with it.
    pub paired: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerStatus {
    /// This Mac's device ID once Mac-to-Mac is on; None before.
    pub device: Option<String>,
    pub name: String,
    /// Where paired Macs reach this one, once on.
    pub address: Option<String>,
    /// Where an open pairing window listens.
    pub pairing: Option<String>,
    pub pending: Vec<PeerPendingRecord>,
    pub devices: Vec<PeerDeviceRecord>,
    /// Macs seen on the local network, this one excluded.
    pub nearby: Vec<PeerNearbyRecord>,
    /// Why Macs on the network cannot be seen, if they cannot.
    pub discovery: Option<String>,
}
