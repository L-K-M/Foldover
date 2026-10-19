//! Messages between two Macs' agents (ADR 0031). Each is JSON in one
//! length-prefixed frame; bulk data travels in binary frames beside them.

use serde::{Deserialize, Serialize};

use crate::local::{PairDurability, PairPropagation};

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
    /// One operation on a folder shared with the asking Mac; refused for
    /// any other folder.
    Folder { share: String, op: FolderOp },
}

/// What the asking Mac's endpoint needs from a shared folder. Each is
/// answered on the answering Mac by its own local endpoint.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum FolderOp {
    /// Open the folder and read its first entry: `done`.
    Probe,
    /// `identity`, none until a first sync records one.
    Identity,
    /// `identity`, created if absent.
    EnsureIdentity { durability: PairDurability },
    /// `rules` from the folder's ignore file.
    IgnoreRules,
    /// `names`: how the folder's volume compares names.
    NameMatching,
    /// `space`: bytes free.
    Space,
    /// `progress` replies while scanning, entries as data frames, then
    /// `observed`.
    Observe { scan: ScanSettings },
}

/// What an observation includes, from the asking Mac's pair options.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScanSettings {
    pub modification_times: PairPropagation,
    pub extended_attributes: PairPropagation,
    pub permissions: PairPropagation,
    /// Exclusion rules: the pair's, then both folders' ignore files.
    pub rules: Vec<String>,
    pub size_limit: Option<u64>,
    /// Files older than this, in milliseconds since the Unix epoch, are old.
    pub cutoff_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NameCase {
    Exact,
    Insensitive,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NameUnicode {
    Exact,
    Normalized,
}

/// Why a folder operation failed, so the asking Mac can treat it like
/// its own endpoint's error.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FolderErrorKind {
    /// The shared folder is gone, e.g. its disk is not mounted.
    Missing,
    AccessDenied,
    Cancelled,
    /// An entry changed while it was read.
    Changed,
    /// An ignore rule or another setting is invalid.
    Invalid,
    Other,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FolderError {
    pub kind: FolderErrorKind,
    /// The entry it concerns, if one.
    pub path: Option<Vec<u8>>,
    pub message: String,
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
    /// The folder operation failed on the answering Mac.
    Failed {
        error: FolderError,
    },
    Done,
    Identity {
        /// 32 hex digits.
        id: Option<String>,
    },
    Rules {
        rules: Vec<String>,
    },
    Names {
        case: NameCase,
        unicode: NameUnicode,
    },
    Space {
        bytes: u64,
    },
    /// Entries found and versioned so far.
    Progress {
        listed: u64,
        checked: u64,
    },
    /// An observation's entries came as data frames; these are the rest.
    Observed {
        leftovers: Vec<Vec<u8>>,
        oversized: Vec<Vec<u8>>,
        old: Vec<Vec<u8>>,
    },
}

/// A folder shared with the asking Mac. `id` is 32 hex digits.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OfferedShare {
    pub id: String,
    pub name: String,
}
