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
