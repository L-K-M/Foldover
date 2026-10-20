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
//! workers. Copies stream data and carry mode and modification time, as
//! with SFTP; the receiving Mac stages and verifies them.

use std::cell::RefCell;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use foldover_fs::{ForeignMetadata, NativeMetadata, WriteError};
use foldover_proto::local::PairDurability;
use foldover_proto::peer::{
    FolderOp, IncomingKind, LeaseAction, LeaseFile, NameCase, NameUnicode, PeerCall, PeerReply,
    ScanSettings,
};
use foldover_types::{
    CaseMatching, Digest, Durability, EndpointId, EntryVersion, MetadataPolicy, NameMatching,
    PairOptions, PeerRoot, ScanControl, SyncPath, UnicodeMatching,
};

use crate::lease::LockFile;
use crate::link::{Exchange, Incoming, LinkError, PeerLink, WatchHandle, Watched};
use crate::local::Carried;
use crate::{
    ApplyError, ArchivedItem, ArchivedRun, Expected, Filter, Observation, ObserveStep,
    ObservedEntry, RootRelation, RunScope, ScanError,
};

mod serve;
mod wire;

pub use serve::{FolderCache, serve_folder};

/// Report changes in a folder a paired Mac shares until the handle drops.
/// Changes while the request was broken are unknown, so the first report
/// after a break counts as a change.
pub fn watch_folder(
    link: &Arc<PeerLink>,
    root: &PeerRoot,
    changed: impl Fn() + Send + Sync + 'static,
) -> WatchHandle {
    let call = PeerCall::Folder {
        share: root.share().to_string(),
        op: Box::new(FolderOp::Watch),
    };
    let broken = AtomicBool::new(false);
    link.watch(root.device(), call, move |watched| match watched {
        Watched::Reply(PeerReply::Changed) => changed(),
        Watched::Reply(PeerReply::Watching) => {
            if broken.swap(false, Ordering::Relaxed) {
                changed();
            }
        }
        Watched::Reply(_) | Watched::Lost => broken.store(true, Ordering::Relaxed),
    })
}

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
        let durability = pair_durability(durability);
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
            cutoff_ms: filter.cutoff().map(wire::milliseconds),
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

    pub(crate) fn read_lock_file(&self, file: LockFile) -> Result<Option<Vec<u8>>, ScanError> {
        match self.reply(lease_op(file, LeaseAction::Read))? {
            PeerReply::LeaseRead { content } => Ok(content),
            _ => Err(unexpected()),
        }
    }

    pub(crate) fn create_lock_file(
        &self,
        file: LockFile,
        content: &[u8],
    ) -> Result<bool, ApplyError> {
        let action = LeaseAction::Create {
            content: content.to_vec(),
        };
        match self.reply(lease_op(file, action))? {
            PeerReply::LeaseCreated { created } => Ok(created),
            _ => Err(unexpected().into()),
        }
    }

    pub(crate) fn replace_lock_file(
        &self,
        file: LockFile,
        content: &[u8],
    ) -> Result<(), ApplyError> {
        let action = LeaseAction::Replace {
            content: content.to_vec(),
        };
        done(self.reply(lease_op(file, action))?)
    }

    pub(crate) fn remove_lock_file(&self, file: LockFile) -> Result<(), ApplyError> {
        done(self.reply(lease_op(file, LeaseAction::Remove))?)
    }

    pub(crate) fn promote_claim(&self) -> Result<(), ApplyError> {
        done(self.reply(lease_op(LockFile::Lock, LeaseAction::Promote))?)
    }

    /// Prove `path` still holds `planned` there, and start reading it
    /// from byte `from`.
    pub(crate) fn read(
        &self,
        path: &SyncPath,
        planned: &ObservedEntry,
        run: &RunScope,
        from: u64,
    ) -> Result<PeerSource, ApplyError> {
        let mut exchange = self.ask(FolderOp::Read {
            path: path.as_bytes().to_vec(),
            entry: wire::entry_bytes(planned),
            run: wire::run_wire(run),
            from,
        })?;
        let (metadata, native, target) = reading(exchange.reply().map_err(unreachable)?)?;
        Ok(PeerSource {
            exchange: RefCell::new(Some(exchange)),
            metadata,
            native,
            target,
        })
    }

    /// Prove `path` still holds `planned`, for its metadata only.
    pub(crate) fn source_metadata(
        &self,
        path: &SyncPath,
        planned: &ObservedEntry,
        run: &RunScope,
    ) -> Result<(ForeignMetadata, Option<NativeMetadata>), ApplyError> {
        let reply = self.reply(FolderOp::Metadata {
            path: path.as_bytes().to_vec(),
            entry: wire::entry_bytes(planned),
            run: wire::run_wire(run),
        });
        let (metadata, native, _) = reading(reply_or_source_changed(reply, path)?)?;
        Ok((metadata, native))
    }

    /// How much of the file with `content` a broken-off copy to this
    /// folder kept.
    pub(crate) fn partial_length(&self, content: &Digest) -> Result<u64, ApplyError> {
        match self.reply(FolderOp::Partial {
            content: content.as_bytes().to_vec(),
        })? {
            PeerReply::Partial { length } => Ok(length),
            _ => Err(unexpected().into()),
        }
    }

    /// Make `path` hold an entry of `kind`; `send` pushes a file's data.
    pub(crate) fn receive(
        &self,
        path: &SyncPath,
        current: Expected<'_>,
        kind: IncomingKind,
        carried: Carried<'_>,
        run: &RunScope,
        send: impl FnOnce(&mut dyn FnMut(&[u8]) -> io::Result<()>) -> io::Result<()>,
    ) -> Result<EntryVersion, ApplyError> {
        let file = matches!(kind, IncomingKind::File { .. });
        let current = match current {
            Expected::Absent => None,
            Expected::Present(current) => Some(wire::entry_bytes(current)),
        };
        let mut exchange = self.ask(FolderOp::Receive {
            path: path.as_bytes().to_vec(),
            entry: foldover_proto::peer::Incoming {
                kind,
                current,
                metadata: wire::metadata_wire(carried.metadata, carried.native),
                run: wire::run_wire(run),
            },
        })?;
        // The other Mac may refuse before reading the data, e.g. for a
        // changed destination; its reply says why better than a broken
        // stream.
        let stream_broke = std::cell::Cell::new(false);
        let broke = |error| {
            stream_broke.set(true);
            io::Error::other(error)
        };
        let sent = if file {
            send(&mut |chunk| exchange.send_data(chunk).map_err(broke))
                .and_then(|()| exchange.end_data().map_err(broke))
        } else {
            Ok(())
        };
        // This Mac's source failed partway: the other Mac still waits for
        // the rest and will not reply. Dropping the request resets its
        // stream, so it gives up too.
        let sent = match sent {
            Err(source) if !stream_broke.get() => {
                drop(exchange);
                return Err(ApplyError::Write {
                    path: path.clone(),
                    source: WriteError::Io(source),
                });
            }
            sent => sent,
        };
        match (exchange.reply(), sent) {
            (Ok(PeerReply::Received { version }), Ok(())) => {
                wire::version_from(&version).ok_or_else(|| unexpected().into())
            }
            (Ok(PeerReply::Failed { error }), _) => {
                Err(wire::apply_error(error, ApplyError::DestinationChanged))
            }
            (Ok(PeerReply::Refused { reason }), _) => Err(ScanError::MacUnreachable(reason).into()),
            (_, Err(source)) => Err(ApplyError::Write {
                path: path.clone(),
                source: WriteError::Io(source),
            }),
            (Ok(_), Ok(())) => Err(unexpected().into()),
            (Err(error), Ok(())) => Err(unreachable(error).into()),
        }
    }

    pub(crate) fn receive_metadata(
        &self,
        path: &SyncPath,
        current: &ObservedEntry,
        carried: Carried<'_>,
        run: &RunScope,
    ) -> Result<EntryVersion, ApplyError> {
        let reply = self.reply(FolderOp::ReceiveMetadata {
            path: path.as_bytes().to_vec(),
            current: wire::entry_bytes(current),
            metadata: wire::metadata_wire(carried.metadata, carried.native),
            run: wire::run_wire(run),
        });
        match reply_or_destination_changed(reply, path)? {
            PeerReply::Received { version } => {
                wire::version_from(&version).ok_or_else(|| unexpected().into())
            }
            _ => Err(unexpected().into()),
        }
    }

    /// Archive `path`, which must still hold `current`, for the run.
    pub fn remove(
        &self,
        path: &SyncPath,
        current: &ObservedEntry,
        run: &RunScope,
    ) -> Result<(), ApplyError> {
        let reply = self.reply(FolderOp::Remove {
            path: path.as_bytes().to_vec(),
            current: wire::entry_bytes(current),
            run: wire::run_wire(run),
        });
        done(reply_or_destination_changed(reply, path)?)
    }

    pub fn recover_leftover(&self, leftover: &SyncPath, run: &RunScope) -> Result<(), ApplyError> {
        let reply = self.reply(FolderOp::RecoverLeftover {
            path: leftover.as_bytes().to_vec(),
            run: wire::run_wire(run),
        });
        done(reply_or_destination_changed(reply, leftover)?)
    }

    /// Prune the other Mac's archive; its own clock decides what is old,
    /// so `now` is not sent.
    pub fn prune_archive(&self, retention: Duration, _now: SystemTime) -> Result<u64, ApplyError> {
        match self.reply(FolderOp::PruneArchive {
            retention_seconds: retention.as_secs(),
        })? {
            PeerReply::Pruned { count } => Ok(count),
            _ => Err(unexpected().into()),
        }
    }

    pub fn archived_runs(&self) -> Result<Vec<ArchivedRun>, ScanError> {
        match self.reply(FolderOp::ArchivedRuns)? {
            PeerReply::Runs { runs } => Ok(runs
                .into_iter()
                .map(|run| {
                    ArchivedRun::new(
                        run.name,
                        SystemTime::UNIX_EPOCH + Duration::from_millis(run.started_ms),
                    )
                })
                .collect()),
            _ => Err(unexpected()),
        }
    }

    pub fn archived_items(
        &self,
        run: &str,
        after: Option<&SyncPath>,
        limit: usize,
    ) -> Result<(Vec<ArchivedItem>, bool), ScanError> {
        match self.reply(FolderOp::ArchivedItems {
            run: run.to_owned(),
            after: after.map(|after| after.as_bytes().to_vec()),
            limit: u64::try_from(limit).unwrap_or(u64::MAX),
        })? {
            PeerReply::Items { items, more } => Ok((
                items
                    .into_iter()
                    .map(|item| {
                        Ok(ArchivedItem::new(
                            SyncPath::try_from(item.path.as_slice())?,
                            item.size,
                        ))
                    })
                    .collect::<Result<_, ScanError>>()?,
                more,
            )),
            _ => Err(unexpected()),
        }
    }

    pub fn put_back(
        &self,
        run: &str,
        path: &SyncPath,
        durability: Durability,
    ) -> Result<SyncPath, ApplyError> {
        let reply = self.reply(FolderOp::PutBack {
            run: run.to_owned(),
            path: path.as_bytes().to_vec(),
            durability: pair_durability(durability),
        });
        placed(reply_or_destination_changed(reply, path)?)
    }

    pub fn keep_copy(
        &self,
        path: &SyncPath,
        expected: EntryVersion,
        from: &str,
        options: &PairOptions,
    ) -> Result<SyncPath, ApplyError> {
        let reply = self.reply(FolderOp::KeepCopy {
            path: path.as_bytes().to_vec(),
            version: wire::version_bytes(expected),
            from: from.to_owned(),
            options: options.into(),
        });
        placed(reply_or_source_changed(reply, path)?)
    }

    fn ask(&self, op: FolderOp) -> Result<Exchange, ScanError> {
        let call = PeerCall::Folder {
            share: self.root.share().to_string(),
            op: Box::new(op),
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

/// An entry on the other Mac proven unchanged, as a copy's source.
pub(crate) struct PeerSource {
    /// The request still sending a file's data.
    exchange: RefCell<Option<Exchange>>,
    metadata: ForeignMetadata,
    native: Option<NativeMetadata>,
    target: Option<Vec<u8>>,
}

impl PeerSource {
    pub(crate) fn metadata(&self) -> ForeignMetadata {
        self.metadata
    }

    pub(crate) fn native(&self) -> Option<&NativeMetadata> {
        self.native.as_ref()
    }

    pub(crate) fn link_target(&self) -> Option<&[u8]> {
        self.target.as_deref()
    }

    /// Stream the file's data into `sink`, checking `control` per chunk.
    pub(crate) fn push(
        &self,
        path: &SyncPath,
        control: &mut impl FnMut() -> ScanControl,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<(), ApplyError> {
        let mut exchange = self.exchange.borrow_mut();
        let exchange = exchange.as_mut().ok_or_else(unexpected)?;
        loop {
            if control() == ScanControl::Cancel {
                return Err(ScanError::Cancelled.into());
            }
            match exchange.next().map_err(unreachable)? {
                Incoming::Data(chunk) => sink(&chunk).map_err(|source| ApplyError::Write {
                    path: path.clone(),
                    source: WriteError::Io(source),
                })?,
                Incoming::End => break,
                Incoming::Reply(_) => return Err(unexpected().into()),
            }
        }
        match exchange.reply().map_err(unreachable)? {
            PeerReply::Done => Ok(()),
            PeerReply::Failed { error } => Err(wire::apply_error(error, ApplyError::SourceChanged)),
            _ => Err(unexpected().into()),
        }
    }
}

/// What a `reading` reply says of the entry.
type Reading = (ForeignMetadata, Option<NativeMetadata>, Option<Vec<u8>>);

fn reading(reply: PeerReply) -> Result<Reading, ApplyError> {
    match reply {
        PeerReply::Reading { metadata, target } => {
            let (metadata, native) = wire::metadata_from(metadata);
            Ok((metadata, native, target))
        }
        PeerReply::Failed { error } => Err(wire::apply_error(error, ApplyError::SourceChanged)),
        PeerReply::Refused { reason } => Err(ScanError::MacUnreachable(reason).into()),
        _ => Err(unexpected().into()),
    }
}

/// A reply, with a changed entry reported as the read side's change.
fn reply_or_source_changed(
    reply: Result<PeerReply, ScanError>,
    path: &SyncPath,
) -> Result<PeerReply, ApplyError> {
    reply.map_err(|error| match error {
        ScanError::Changed(_) => ApplyError::SourceChanged(path.clone()),
        error => error.into(),
    })
}

/// A reply, with a changed entry reported as the written side's change.
fn reply_or_destination_changed(
    reply: Result<PeerReply, ScanError>,
    path: &SyncPath,
) -> Result<PeerReply, ApplyError> {
    reply.map_err(|error| match error {
        ScanError::Changed(_) => ApplyError::DestinationChanged(path.clone()),
        error => error.into(),
    })
}

fn done(reply: PeerReply) -> Result<(), ApplyError> {
    match reply {
        PeerReply::Done => Ok(()),
        _ => Err(unexpected().into()),
    }
}

fn placed(reply: PeerReply) -> Result<SyncPath, ApplyError> {
    match reply {
        PeerReply::Placed { path } => {
            Ok(SyncPath::try_from(path.as_slice()).map_err(ScanError::from)?)
        }
        _ => Err(unexpected().into()),
    }
}

fn lease_op(file: LockFile, action: LeaseAction) -> FolderOp {
    let file = match file {
        LockFile::Lock => LeaseFile::Lock,
        LockFile::Claim => LeaseFile::Claim,
    };
    FolderOp::Lease { file, action }
}

fn pair_durability(durability: Durability) -> PairDurability {
    match durability {
        Durability::Standard => PairDurability::Standard,
        Durability::Full => PairDurability::Full,
    }
}

fn unreachable(error: LinkError) -> ScanError {
    ScanError::MacUnreachable(error.to_string())
}

fn unexpected() -> ScanError {
    ScanError::Peer("The other Mac answered out of order".into())
}

#[cfg(test)]
mod tests;
