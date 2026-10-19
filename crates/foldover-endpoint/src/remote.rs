//! A folder another Mac shares (ADR 0031), reached over the peer link.
//! Each operation is one request; that Mac answers it with its own local
//! endpoint, so its staging, verification and archive rules hold there.
//!
//! ```text
//! this Mac: PeerEndpoint ── Folder { share, op } ──▶ other Mac: serve_folder
//!                         ◀── replies, entry batches ── LocalEndpoint(share root)
//! ```
//!
//! Calls block: run them on blocking threads, never on the link's runtime
//! workers. Writing into another Mac's folder comes in a later slice.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use foldover_proto::local::PairDurability;
use foldover_proto::peer::{FolderOp, NameCase, NameUnicode, PeerCall, PeerReply, ScanSettings};
use foldover_types::{
    CaseMatching, Durability, EndpointId, MetadataPolicy, NameMatching, PeerRoot, ScanControl,
    UnicodeMatching,
};

use crate::link::{Exchange, Incoming, LinkError, PeerLink};
use crate::{ApplyError, Filter, Observation, ObserveStep, RootRelation, ScanError};

mod serve;
mod wire;

pub use serve::{FolderCache, serve_folder};

/// What another Mac's folder cannot do yet.
const WRITES_PENDING: &str = "Changing files in another Mac's folder is not available yet";

pub struct PeerEndpoint {
    link: Arc<PeerLink>,
    root: PeerRoot,
    names: NameMatching,
}

impl PeerEndpoint {
    /// Reach the shared folder and read its first entry, so a refusal or
    /// a missing folder shows here.
    pub fn open(link: Arc<PeerLink>, root: &PeerRoot) -> Result<Self, ScanError> {
        let mut endpoint = Self {
            link,
            root: root.clone(),
            names: NameMatching::EXACT,
        };
        match endpoint.reply(FolderOp::Probe)? {
            PeerReply::Done => {}
            _ => return Err(unexpected()),
        }
        endpoint.names = match endpoint.reply(FolderOp::NameMatching)? {
            PeerReply::Names { case, unicode } => NameMatching::new(
                match case {
                    NameCase::Exact => CaseMatching::Exact,
                    NameCase::Insensitive => CaseMatching::Insensitive,
                },
                match unicode {
                    NameUnicode::Exact => UnicodeMatching::Exact,
                    NameUnicode::Normalized => UnicodeMatching::Normalized,
                },
            ),
            _ => return Err(unexpected()),
        };
        Ok(endpoint)
    }

    pub fn identity(&self) -> Result<Option<EndpointId>, ScanError> {
        match self.reply(FolderOp::Identity)? {
            PeerReply::Identity { id } => id
                .map(|id| EndpointId::from_hex(id.as_bytes()).ok_or(ScanError::InvalidIdentity))
                .transpose(),
            _ => Err(unexpected()),
        }
    }

    pub fn ensure_identity(&self, durability: Durability) -> Result<EndpointId, ApplyError> {
        let durability = match durability {
            Durability::Standard => PairDurability::Standard,
            Durability::Full => PairDurability::Full,
        };
        match self.reply(FolderOp::EnsureIdentity { durability })? {
            PeerReply::Identity { id: Some(id) } => {
                Ok(EndpointId::from_hex(id.as_bytes()).ok_or(ScanError::InvalidIdentity)?)
            }
            _ => Err(unexpected().into()),
        }
    }

    pub fn ignore_rules(&self) -> Result<Vec<String>, ScanError> {
        match self.reply(FolderOp::IgnoreRules)? {
            PeerReply::Rules { rules } => Ok(rules),
            _ => Err(unexpected()),
        }
    }

    /// How the other Mac's volume compares names, learned when opened.
    pub fn name_matching(&self) -> NameMatching {
        self.names
    }

    pub fn available_space(&self) -> Result<u64, ScanError> {
        match self.reply(FolderOp::Space)? {
            PeerReply::Space { bytes } => Ok(bytes),
            _ => Err(unexpected()),
        }
    }

    /// Observe the folder on the other Mac. Cancelling drops the request,
    /// which stops the scan there.
    pub fn observe_reporting(
        &self,
        policy: MetadataPolicy,
        filter: &Filter,
        control: &mut impl FnMut() -> ScanControl,
        progress: &mut impl FnMut(ObserveStep),
    ) -> Result<Observation, ScanError> {
        let scan = ScanSettings {
            modification_times: policy.modification_times().into(),
            extended_attributes: policy.extended_attributes().into(),
            permissions: policy.permissions().into(),
            rules: filter.texts().to_vec(),
            size_limit: filter.size_limit(),
            cutoff_ms: filter.cutoff().map(milliseconds),
        };
        let mut exchange = self.ask(FolderOp::Observe { scan })?;
        let mut observation = Observation::default();
        let (mut listed, mut checked) = (0, 0);
        loop {
            if control() == ScanControl::Cancel {
                return Err(ScanError::Cancelled);
            }
            match exchange.next().map_err(unreachable)? {
                Incoming::Reply(PeerReply::Progress {
                    listed: now_listed,
                    checked: now_checked,
                }) => {
                    for _ in listed..now_listed {
                        progress(ObserveStep::Listed);
                    }
                    for _ in checked..now_checked {
                        progress(ObserveStep::Checked);
                    }
                    (listed, checked) = (now_listed.max(listed), now_checked.max(checked));
                }
                Incoming::Data(batch) => {
                    let entries = wire::entries(&batch).ok_or_else(unexpected)?;
                    observation.entries.extend(entries);
                }
                Incoming::End => {}
                Incoming::Reply(PeerReply::Observed {
                    leftovers,
                    oversized,
                    old,
                }) => {
                    observation.leftovers = paths(leftovers)?;
                    observation.oversized = paths(oversized)?;
                    observation.old = paths(old)?;
                    return Ok(observation);
                }
                Incoming::Reply(PeerReply::Failed { error }) => {
                    return Err(wire::scan_error(error));
                }
                Incoming::Reply(_) => return Err(unexpected()),
            }
        }
    }

    /// Two folders on other Macs are the same only by device and share.
    pub fn relation_to(&self, other: &Self) -> RootRelation {
        if self.root == other.root {
            RootRelation::Same
        } else {
            RootRelation::Disjoint
        }
    }

    /// Writes are not served yet; report it instead of failing silently.
    pub(crate) fn writes_pending() -> ApplyError {
        ApplyError::Scan(ScanError::Peer(WRITES_PENDING.into()))
    }

    fn ask(&self, op: FolderOp) -> Result<Exchange, ScanError> {
        let call = PeerCall::Folder {
            share: self.root.share().to_string(),
            op,
        };
        self.link.ask(self.root.device(), call).map_err(unreachable)
    }

    /// The single reply to `op`, with failures as this Mac's errors.
    fn reply(&self, op: FolderOp) -> Result<PeerReply, ScanError> {
        match self.ask(op)?.reply().map_err(unreachable)? {
            PeerReply::Failed { error } => Err(wire::scan_error(error)),
            PeerReply::Refused { reason } => Err(ScanError::MacUnreachable(reason)),
            reply => Ok(reply),
        }
    }
}

fn paths(paths: Vec<Vec<u8>>) -> Result<Vec<foldover_types::SyncPath>, ScanError> {
    paths
        .into_iter()
        .map(|path| foldover_types::SyncPath::try_from(path.as_slice()).map_err(ScanError::from))
        .collect()
}

fn milliseconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
    })
}

fn unreachable(error: LinkError) -> ScanError {
    ScanError::MacUnreachable(error.to_string())
}

fn unexpected() -> ScanError {
    ScanError::Peer("The other Mac answered out of order".into())
}
