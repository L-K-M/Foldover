//! Messages between two Macs' agents (ADR 0031). Each is JSON in one
//! length-prefixed frame; bulk data travels in binary frames beside them.

use serde::{Deserialize, Serialize};

/// The peer protocol version; agents of other versions do not pair.
pub const PEER_PROTOCOL: u32 = 1;

/// Pairing, after both sides derived the six-digit code. Each side sends
/// its hello, then exactly one decision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum PairingMessage {
    Hello {
        protocol: u32,
        name: String,
    },
    /// The person on this side confirmed that both Macs show one code.
    Confirm,
    Cancel,
}

/// One request between paired Macs, on its own stream; the reply follows
/// on the same stream.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerRequest {
    /// `PEER_PROTOCOL` of the asking Mac; another version is refused.
    pub protocol: u32,
    pub call: PeerCall,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum PeerCall {
    /// The folders the answering Mac shares with the asking one.
    Shares,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum PeerReply {
    Shares {
        shares: Vec<OfferedShare>,
    },
    /// Not answered, e.g. another version or a folder not shared with the
    /// asking Mac.
    Refused {
        reason: String,
    },
}

/// A folder shared with the asking Mac. `id` is 32 hex digits.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OfferedShare {
    pub id: String,
    pub name: String,
}
