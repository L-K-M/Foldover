//! Messages between two Macs' agents (ADR 0031). Each is JSON in one
//! length-prefixed frame; bulk data travels in binary frames beside them.

use serde::{Deserialize, Serialize};

use crate::local::{PairDurability, PairPropagation, PairSettings};

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
    Folder { share: String, op: Box<FolderOp> },
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
    /// Prove `path` still holds `entry`, then send it: `reading`, a file's
    /// data frames and their end, then `done`.
    Read {
        path: Vec<u8>,
        entry: Vec<u8>,
        run: RunSettings,
    },
    /// `reading` alone, for a metadata update from this folder.
    Metadata {
        path: Vec<u8>,
        entry: Vec<u8>,
        run: RunSettings,
    },
    /// Make `path` hold what the asking Mac sends: a file's data frames
    /// and their end follow the request. `received`.
    Receive { path: Vec<u8>, entry: Incoming },
    /// Give `path`, which must still hold `current`, new synced metadata.
    /// `received`.
    ReceiveMetadata {
        path: Vec<u8>,
        current: Vec<u8>,
        metadata: WireMetadata,
        run: RunSettings,
    },
    /// Archive `path`, which must still hold `current`. `done`.
    Remove {
        path: Vec<u8>,
        current: Vec<u8>,
        run: RunSettings,
    },
    /// Archive an interrupted run's temporary file. `done`.
    RecoverLeftover { path: Vec<u8>, run: RunSettings },
    /// `pruned`. The answering Mac's clock decides what is old, and it
    /// keeps at least its own retention.
    PruneArchive { retention_seconds: u64 },
    /// `runs`, newest first.
    ArchivedRuns,
    /// `items` after `after`, and whether more follow.
    ArchivedItems {
        run: String,
        after: Option<Vec<u8>>,
        limit: u64,
    },
    /// `placed`.
    PutBack {
        run: String,
        path: Vec<u8>,
        durability: PairDurability,
    },
    /// Save `path`, which must still hold `version`, beside itself. `placed`.
    KeepCopy {
        path: Vec<u8>,
        version: Vec<u8>,
        from: String,
        options: PairSettings,
    },
}

/// What a write belongs to: the run's archive folder and the pair's
/// options.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunSettings {
    pub name: String,
    pub options: PairSettings,
}

/// Permissions and modification time, as streamed copies carry them.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WireMetadata {
    pub permissions: u32,
    /// Since the Unix epoch.
    pub modified_seconds: i64,
    pub modified_nanos: u32,
}

/// What a receiving folder gets.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Incoming {
    pub kind: IncomingKind,
    /// The entry the plan expects there now, if any.
    pub current: Option<Vec<u8>>,
    pub metadata: WireMetadata,
    pub run: RunSettings,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum IncomingKind {
    /// Data frames follow; their content must hash to `content`.
    File {
        content: Vec<u8>,
    },
    Folder,
    Link {
        target: Vec<u8>,
    },
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
    /// An entry being read: its metadata, and a link's target.
    Reading {
        metadata: WireMetadata,
        target: Option<Vec<u8>>,
    },
    /// The version now stored.
    Received {
        version: Vec<u8>,
    },
    Pruned {
        count: u64,
    },
    Runs {
        runs: Vec<WireRun>,
    },
    Items {
        items: Vec<WireItem>,
        more: bool,
    },
    /// Where an entry was put.
    Placed {
        path: Vec<u8>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WireRun {
    pub name: String,
    pub started_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WireItem {
    pub path: Vec<u8>,
    pub size: u64,
}

/// A folder shared with the asking Mac. `id` is 32 hex digits.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OfferedShare {
    pub id: String,
    pub name: String,
}
