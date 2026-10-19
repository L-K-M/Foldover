//! Folders this Mac shares with paired Macs, and what they share with it
//! (ADR 0031). Roots are absolute path bytes, like pair roots.

use serde::{Deserialize, Serialize};

pub const SHARE_LIST: &str = "share.list";
pub const SHARE_ADD: &str = "share.add";
pub const SHARE_REMOVE: &str = "share.remove";
/// The folders a paired Mac shares with this one; asks that Mac.
pub const PEER_SHARES: &str = "peer.shares";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShareRecord {
    pub id: String,
    /// The paired Mac that may sync with it.
    pub device: String,
    pub root: Vec<u8>,
    pub name: String,
    /// Milliseconds since the Unix epoch.
    pub created_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShareList {
    pub shares: Vec<ShareRecord>,
}

/// Share `root` with `device` as `name`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShareAdd {
    pub device: String,
    pub root: Vec<u8>,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShareKey {
    pub id: String,
}

/// A folder a paired Mac shares with this one.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerShareRecord {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerShareList {
    pub shares: Vec<PeerShareRecord>,
}
