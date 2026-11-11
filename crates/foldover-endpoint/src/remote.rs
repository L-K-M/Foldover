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

use std::cell::{Cell, RefCell};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use foldover_fs::{ForeignMetadata, NativeMetadata, WriteError};
use foldover_proto::local::{PairDurability, PairPropagation};
use foldover_proto::peer::{
    CatalogOp, FolderError, FolderErrorKind, FolderOp, IncomingKind, LeaseAction, LeaseFile,
    NameCase, NameUnicode, PeerCall, PeerReply, ScanSettings, VolumeKind,
};
use foldover_types::{
    CaseMatching, Digest, Durability, EndpointId, EntryVersion, MetadataPolicy, NameMatching,
    PairId, PairOptions, PeerRoot, Propagation, ScanControl, SyncPath, UnicodeMatching,
};

use crate::catalog::{CATALOG_ENTRIES_LIMIT, CATALOG_FILE_LIMIT, invalid};
use crate::lease::LockFile;
use crate::link::{Exchange, Incoming, LinkError, PeerLink, WatchHandle, Watched};
use crate::local::{Archiving, Carried};
use crate::{
    ApplyError, ArchivedItem, ArchivedRun, Expected, Filter, Observation, ObserveStep,
    ObservedEntry, RootRelation, RunScope, ScanError,
};

mod serve;
mod wire;

pub use serve::{FolderCache, serve_folder};

/// The most scan steps one progress report's counter can stand for. The
/// counters come from the other Mac and drive display only (PLAN.md
/// §4.3), so a buggy or hostile peer must not make the asking side
/// iterate over its numbers. No real scan lists this many entries
/// between two reports.
const MAX_STEPS_PER_REPORT: u64 = 1_000_000;
/// How often a source that has not produced data proves it is still alive
/// to a peer destination waiting for the next chunk.
const STREAM_HEARTBEAT_INTERVAL: Duration = Duration::from_millis(200);

/// The most scan steps all of one scan's reports together can stand for,
/// per counter, however many reports the other Mac sends: a hundred times
/// the million files PLAN.md §5.3 budgets for.
const MAX_STEPS_PER_SCAN: u64 = 100_000_000;

/// The most bytes one catalog data frame carries. A catalog file may hold
/// the whole 16 MiB blob limit, so it travels in 1 MiB blocks rather than
/// one frame.
pub(super) const CATALOG_CHUNK: usize = 1024 * 1024;

/// Report the steps a peer's cumulative counter grew, at most
/// `MAX_STEPS_PER_REPORT` of them and none past `MAX_STEPS_PER_SCAN`,
/// then adopt the counter as it stands. A call reports at most
/// `min(now, MAX_STEPS_PER_SCAN) - adopted` steps and leaves `adopted` at
/// least `now`, so all of a scan's calls report `MAX_STEPS_PER_SCAN` at
/// most. Progress is what "Comparing… N items" counts; it decides nothing.
fn report_steps(
    adopted: &mut u64,
    now: u64,
    step: ObserveStep,
    progress: &mut impl FnMut(ObserveStep),
) {
    let bounded = now.min(MAX_STEPS_PER_SCAN);
    for _ in 0..bounded.saturating_sub(*adopted).min(MAX_STEPS_PER_REPORT) {
        progress(step);
    }
    *adopted = (*adopted).max(now);
}

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
    /// The pair's target catalog scope, when this endpoint is one. Every
    /// lease operation then rides on a catalog request, so the catalog's
    /// lease lives in its private directory rather than the shared
    /// folder's own.
    catalog: Option<CatalogScope>,
}

/// The private catalog directory of one pair inside a shared folder.
#[derive(Clone, Copy)]
struct CatalogScope {
    pair: PairId,
}

impl PeerEndpoint {
    /// Reach the shared folder and read its first entry, so a refusal or
    /// a missing folder shows here.
    pub fn open(link: Arc<PeerLink>, root: &PeerRoot) -> Result<Self, ScanError> {
        let mut endpoint = Self {
            link,
            root: root.clone(),
            names: NameMatching::EXACT,
            catalog: None,
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

    /// Whether the other Mac keeps the folder on one of its network
    /// volumes.
    pub fn on_network_volume(&self) -> Result<bool, ScanError> {
        match self.reply(FolderOp::Volume)? {
            PeerReply::Volume { volume } => Ok(volume == VolumeKind::Network),
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
            quarantine: (policy.extended_attributes() == Propagation::Sync
                && policy.quarantine() == Propagation::Ignore)
                .then_some(PairPropagation::Ignore),
        };
        let mut exchange = self.ask_unless(FolderOp::Observe { scan }, control)?;
        let mut observation = Observation::default();
        let (mut listed, mut checked) = (0, 0);
        loop {
            if control() == ScanControl::Cancel {
                return Err(ScanError::Cancelled);
            }
            match exchange.next_unless(control).map_err(unreachable)? {
                Incoming::Reply(PeerReply::Progress {
                    listed: now_listed,
                    checked: now_checked,
                }) => {
                    report_steps(&mut listed, now_listed, ObserveStep::Listed, progress);
                    report_steps(&mut checked, now_checked, ObserveStep::Checked, progress);
                }
                Incoming::Data(batch) => {
                    let entries = wire::entries(&batch).ok_or_else(unexpected)?;
                    for (path, entry) in entries {
                        observation.record(path, entry);
                    }
                }
                Incoming::Reply(PeerReply::ScanIssue { path, message }) => {
                    let path = SyncPath::try_from(path)?;
                    observation.unavailable.push(path.clone());
                    observation.issues.push(crate::ScanIssue { path, message });
                }
                Incoming::Reply(PeerReply::UnstableDirectory { path }) => {
                    observation
                        .unstable
                        .push(path.map(SyncPath::try_from).transpose()?);
                }
                Incoming::End => {}
                Incoming::Reply(PeerReply::Observed {
                    leftovers,
                    oversized,
                    old,
                    unavailable,
                }) => {
                    observation.leftovers = paths(leftovers)?;
                    observation.oversized = paths(oversized)?;
                    observation.old = paths(old)?;
                    observation.unavailable.extend(paths(unavailable)?);
                    observation.unavailable.sort();
                    observation.unavailable.dedup();
                    observation.finish_entries();
                    let unavailable = &observation.unavailable;
                    observation.entries.retain(|path, _| {
                        !unavailable
                            .iter()
                            .any(|root| root == path || root.is_ancestor_of(path))
                    });
                    return Ok(observation);
                }
                Incoming::Reply(PeerReply::Failed { error }) => {
                    return Err(wire::scan_error(error));
                }
                // As `reply` does: the other Mac says why, e.g. it paused
                // syncing or stopped sharing the folder since it opened.
                Incoming::Reply(PeerReply::Refused { reason }) => {
                    return Err(ScanError::MacUnreachable(reason));
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

    /// Scope this endpoint to the pair's target catalog: the private
    /// directory `.foldover/state/<pair>` inside the shared folder. The
    /// directory is opened and checked there, so a peer of another
    /// version refuses here rather than mid-sync; no source-side
    /// database is ever substituted.
    pub(crate) fn catalog_endpoint(
        &self,
        pair: PairId,
        durability: Durability,
    ) -> io::Result<Self> {
        let scoped = Self {
            link: Arc::clone(&self.link),
            root: self.root.clone(),
            names: self.names,
            catalog: Some(CatalogScope { pair }),
        };
        let op = scoped.catalog_call(CatalogOp::Open {
            durability: pair_durability(durability),
        })?;
        match scoped.reply(op).map_err(scan_io)? {
            PeerReply::Done => Ok(scoped),
            _ => Err(scan_io(unexpected())),
        }
    }

    /// Read one catalog file, at most `limit` bytes. The file's bytes
    /// travel as data frames, not a JSON array.
    pub(crate) fn catalog_read(&self, name: &str, limit: usize) -> io::Result<Option<Vec<u8>>> {
        self.catalog_scope()?;
        if limit > CATALOG_FILE_LIMIT {
            return Err(invalid("Catalog read exceeds its limit"));
        }
        let mut exchange = self
            .ask_catalog(CatalogOp::Read {
                name: name.to_owned(),
                limit: u64::try_from(limit).unwrap_or(u64::MAX),
            })
            .map_err(scan_io)?;
        match catalog_reply(exchange.reply())? {
            PeerReply::CatalogRead { found: false } => Ok(None),
            PeerReply::CatalogRead { found: true } => {
                let mut bytes = Vec::new();
                loop {
                    match exchange.next().map_err(link_io)? {
                        Incoming::Data(chunk) => {
                            if chunk.len() > limit.saturating_sub(bytes.len()) {
                                return Err(invalid("Catalog file exceeds its limit"));
                            }
                            bytes.extend_from_slice(&chunk);
                        }
                        Incoming::End => break,
                        Incoming::Reply(_) => return Err(scan_io(unexpected())),
                    }
                }
                match catalog_reply(exchange.reply())? {
                    PeerReply::Done => Ok(Some(bytes)),
                    _ => Err(scan_io(unexpected())),
                }
            }
            _ => Err(scan_io(unexpected())),
        }
    }

    /// Write one catalog file, creating it only. The bytes travel as
    /// data frames; an existing file is reported, never replaced.
    pub(crate) fn catalog_write(
        &self,
        name: &str,
        bytes: &[u8],
        durability: Durability,
    ) -> io::Result<()> {
        self.catalog_scope()?;
        if bytes.len() > CATALOG_FILE_LIMIT {
            return Err(invalid("Catalog file exceeds its limit"));
        }
        let mut exchange = self
            .ask_catalog(CatalogOp::Write {
                name: name.to_owned(),
                durability: pair_durability(durability),
            })
            .map_err(scan_io)?;
        for chunk in bytes.chunks(CATALOG_CHUNK) {
            exchange.send_data(chunk).map_err(link_io)?;
        }
        exchange.end_data().map_err(link_io)?;
        match catalog_reply(exchange.reply())? {
            PeerReply::CatalogCreated { created: true } => Ok(()),
            PeerReply::CatalogCreated { created: false } => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Catalog file already exists",
            )),
            _ => Err(scan_io(unexpected())),
        }
    }

    /// List the catalog's file names. The other Mac pages them, so a
    /// catalog of the 250,000-file limit never overflows one frame.
    pub(crate) fn catalog_list(&self) -> io::Result<Vec<String>> {
        self.catalog_scope()?;
        let mut exchange = self.ask_catalog(CatalogOp::List).map_err(scan_io)?;
        let mut names = Vec::new();
        loop {
            match catalog_reply(exchange.reply())? {
                PeerReply::CatalogListed { names: page, more } => {
                    if page.len() > CATALOG_ENTRIES_LIMIT.saturating_sub(names.len()) {
                        return Err(invalid("Too many catalog files"));
                    }
                    if page.is_empty() && more {
                        return Err(invalid("Empty catalog page promises more files"));
                    }
                    for name in &page {
                        crate::catalog::validate_name(name)?;
                    }
                    names.extend(page);
                    if !more {
                        return Ok(names);
                    }
                }
                _ => return Err(scan_io(unexpected())),
            }
        }
    }

    /// Remove one catalog file; nothing to do if it is absent.
    pub(crate) fn catalog_remove(&self, name: &str) -> io::Result<()> {
        self.catalog_scope()?;
        let mut exchange = self
            .ask_catalog(CatalogOp::Remove {
                name: name.to_owned(),
            })
            .map_err(scan_io)?;
        match catalog_reply(exchange.reply())? {
            PeerReply::Done => Ok(()),
            _ => Err(scan_io(unexpected())),
        }
    }

    pub(crate) fn read_lock_file(&self, file: LockFile) -> Result<Option<Vec<u8>>, ScanError> {
        match self.reply(self.lease_call(file, LeaseAction::Read)?)? {
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
        match self.reply(self.lease_call(file, action)?)? {
            PeerReply::LeaseCreated { created } => Ok(created),
            _ => Err(unexpected().into()),
        }
    }

    pub(crate) fn remove_lock_file(&self, file: LockFile) -> Result<(), ApplyError> {
        done(self.reply(self.lease_call(file, LeaseAction::Remove)?)?)
    }

    pub(crate) fn promote_claim(&self) -> Result<(), ApplyError> {
        done(self.reply(self.lease_call(LockFile::Lock, LeaseAction::Promote)?)?)
    }

    /// Prove `path` still holds `planned` there, and start reading it
    /// from byte `from`.
    pub(crate) fn read(
        &self,
        path: &SyncPath,
        planned: &ObservedEntry,
        run: &RunScope,
        from: u64,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<PeerSource, ApplyError> {
        let mut exchange = self.ask_unless(
            FolderOp::Read {
                path: path.as_bytes().to_vec(),
                entry: wire::entry_bytes(planned),
                run: wire::run_wire(run),
                from,
            },
            control,
        )?;
        let reply = exchange.reply_unless(control).map_err(unreachable)?;
        let (metadata, native, target) = reading(reply)?;
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
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<(ForeignMetadata, Option<NativeMetadata>), ApplyError> {
        let reply = self.reply_unless(
            FolderOp::Metadata {
                path: path.as_bytes().to_vec(),
                entry: wire::entry_bytes(planned),
                run: wire::run_wire(run),
            },
            control,
        );
        let (metadata, native, _) = reading(reply_or_source_changed(reply, path)?)?;
        Ok((metadata, native))
    }

    /// How much of the file with `content` a broken-off copy to this
    /// folder kept.
    pub(crate) fn partial_length(
        &self,
        content: &Digest,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<u64, ApplyError> {
        match self.reply_unless(
            FolderOp::Partial {
                content: content.as_bytes().to_vec(),
            },
            control,
        )? {
            PeerReply::Partial { length } => Ok(length),
            _ => Err(unexpected().into()),
        }
    }

    /// Make `path` hold an entry of `kind`; `send` pushes a file's data.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn receive(
        &self,
        path: &SyncPath,
        current: Expected<'_>,
        kind: IncomingKind,
        carried: Carried<'_>,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
        send: impl FnOnce(
            &mut dyn FnMut(&[u8]) -> io::Result<()>,
            &mut dyn FnMut() -> ScanControl,
        ) -> io::Result<()>,
    ) -> Result<EntryVersion, ApplyError> {
        archived(run, path)?;
        let file = matches!(kind, IncomingKind::File { .. });
        let current = match current {
            Expected::Absent => None,
            Expected::Present(current) => Some(wire::entry_bytes(current)),
        };
        let exchange = self.ask_unless(
            FolderOp::Receive {
                path: path.as_bytes().to_vec(),
                entry: foldover_proto::peer::Incoming {
                    kind,
                    current,
                    metadata: wire::metadata_wire(carried.metadata, carried.native),
                    run: wire::run_wire(run),
                },
            },
            control,
        )?;
        let exchange = RefCell::new(exchange);
        let control = RefCell::new(control);
        // The other Mac may refuse before reading the data, e.g. for a
        // changed destination; its reply says why better than a broken
        // stream.
        let stream_failure = RefCell::new(None);
        let broke = |error: LinkError| {
            let message = error.to_string();
            stream_failure.borrow_mut().get_or_insert(error);
            io::Error::other(message)
        };
        let sent = if file {
            let mut destination_control = || (*control.borrow_mut())();
            let mut sink = |chunk: &[u8]| {
                exchange
                    .borrow_mut()
                    .send_data_unless(chunk, &mut destination_control)
                    .map_err(broke)
            };
            let heartbeat = Cell::new(Instant::now());
            let mut source_control = || {
                if stream_failure.borrow().is_some() {
                    return ScanControl::Cancel;
                }
                let state = (*control.borrow_mut())();
                if state == ScanControl::Cancel {
                    stream_failure
                        .borrow_mut()
                        .get_or_insert(LinkError::Cancelled);
                    return state;
                }
                if heartbeat.get().elapsed() < STREAM_HEARTBEAT_INTERVAL {
                    return state;
                }
                heartbeat.set(Instant::now());
                let mut checkpoint = || (*control.borrow_mut())();
                match exchange.borrow_mut().heartbeat_unless(&mut checkpoint) {
                    Ok(()) => ScanControl::Continue,
                    Err(error) => {
                        stream_failure.borrow_mut().get_or_insert(error);
                        ScanControl::Cancel
                    }
                }
            };
            send(&mut sink, &mut source_control).and_then(|()| {
                let mut checkpoint = || (*control.borrow_mut())();
                exchange
                    .borrow_mut()
                    .end_data_unless(&mut checkpoint)
                    .map_err(broke)
            })
        } else {
            Ok(())
        };
        // This Mac's source failed partway: the other Mac still waits for
        // the rest and will not reply. Dropping the request resets its
        // stream, so it gives up too.
        let sent = match sent {
            Err(source) if stream_failure.borrow().is_none() => {
                drop(exchange);
                return Err(ApplyError::Write {
                    path: path.clone(),
                    source: WriteError::Io(source),
                });
            }
            Err(_) => {
                let error = stream_failure
                    .borrow_mut()
                    .take()
                    .expect("a failed stream records its link error");
                return Err(unreachable(error).into());
            }
            sent => sent,
        };
        let reply = {
            let mut checkpoint = || (*control.borrow_mut())();
            exchange.borrow_mut().reply_unless(&mut checkpoint)
        };
        match (reply, sent) {
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
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<EntryVersion, ApplyError> {
        let reply = self.reply_unless(
            FolderOp::ReceiveMetadata {
                path: path.as_bytes().to_vec(),
                current: wire::entry_bytes(current),
                metadata: wire::metadata_wire(carried.metadata, carried.native),
                run: wire::run_wire(run),
            },
            control,
        );
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
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<(), ApplyError> {
        archived(run, path)?;
        let reply = self.reply_unless(
            FolderOp::Remove {
                path: path.as_bytes().to_vec(),
                current: wire::entry_bytes(current),
                run: wire::run_wire(run),
            },
            control,
        );
        done(reply_or_destination_changed(reply, path)?)
    }

    pub fn recover_leftover(&self, leftover: &SyncPath, run: &RunScope) -> Result<(), ApplyError> {
        archived(run, leftover)?;
        let reply = self.reply(FolderOp::RecoverLeftover {
            path: leftover.as_bytes().to_vec(),
            run: wire::run_wire(run),
        });
        done(reply_or_destination_changed(reply, leftover)?)
    }

    /// Prune the other Mac's archive; its own clock decides what is old,
    /// so `now` is not sent. `control` ends the wait, and the prune stops
    /// at the other Mac's next heartbeat.
    pub fn prune_archive(
        &self,
        retention: Duration,
        _now: SystemTime,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<u64, ApplyError> {
        match self.reply_unless(
            FolderOp::PruneArchive {
                retention_seconds: retention.as_secs(),
            },
            control,
        )? {
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
            PeerReply::Items { items, more } => {
                let items = items
                    .into_iter()
                    .map(|item| {
                        Ok(ArchivedItem::new(
                            SyncPath::try_from(item.path.as_slice())?,
                            item.size,
                        ))
                    })
                    .collect::<Result<Vec<_>, ScanError>>()?;
                // A peer must honor the same page contract as local listings;
                // repeated or empty non-final pages would keep recovery looping.
                let wrong_count = items.len() > limit || (limit > 0 && more && items.is_empty());
                let unordered = items
                    .windows(2)
                    .any(|pair| pair[0].path() >= pair[1].path());
                let stale = after
                    .is_some_and(|after| items.first().is_some_and(|item| item.path() <= after));
                if wrong_count || unordered || stale {
                    return Err(ScanError::Peer(
                        "The other Mac returned an invalid archive page".into(),
                    ));
                }
                Ok((items, more))
            }
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
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<SyncPath, ApplyError> {
        let reply = self.reply_unless(
            FolderOp::KeepCopy {
                path: path.as_bytes().to_vec(),
                version: wire::version_bytes(expected),
                from: from.to_owned(),
                options: options.into(),
            },
            control,
        );
        placed(reply_or_source_changed(reply, path)?)
    }

    /// The pair's catalog scope, or why this endpoint is not one.
    fn catalog_scope(&self) -> io::Result<CatalogScope> {
        self.catalog.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "This endpoint is not a target catalog",
            )
        })
    }

    /// A catalog request for this endpoint's pair.
    fn catalog_call(&self, op: CatalogOp) -> io::Result<FolderOp> {
        match &op {
            CatalogOp::Read { name, .. }
            | CatalogOp::Write { name, .. }
            | CatalogOp::Remove { name } => crate::catalog::validate_name(name)?,
            _ => {}
        }
        Ok(FolderOp::Catalog {
            pair: self.catalog_scope()?.pair.to_string(),
            op,
        })
    }

    fn ask_catalog(&self, op: CatalogOp) -> Result<Exchange, ScanError> {
        let op = self
            .catalog_call(op)
            .map_err(|error| ScanError::Peer(error.to_string()))?;
        self.ask(op)
    }

    /// A lease operation. On a catalog endpoint it rides on a catalog
    /// request, so the lease lands in the catalog's private directory;
    /// otherwise it is the shared folder's own lease, as before.
    fn lease_call(&self, file: LockFile, action: LeaseAction) -> Result<FolderOp, ScanError> {
        let file = match file {
            LockFile::Lock => LeaseFile::Lock,
            LockFile::Claim => LeaseFile::Claim,
        };
        match self.catalog {
            Some(scope) => Ok(FolderOp::Catalog {
                pair: scope.pair.to_string(),
                op: CatalogOp::Lease { file, action },
            }),
            None => Ok(lease_op(file, action)),
        }
    }

    fn ask(&self, op: FolderOp) -> Result<Exchange, ScanError> {
        let call = PeerCall::Folder {
            share: self.root.share().to_string(),
            op: Box::new(op),
        };
        self.link.ask(self.root.device(), call).map_err(unreachable)
    }

    fn ask_unless(
        &self,
        op: FolderOp,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<Exchange, ScanError> {
        let call = PeerCall::Folder {
            share: self.root.share().to_string(),
            op: Box::new(op),
        };
        self.link
            .ask_unless(self.root.device(), call, control)
            .map_err(unreachable)
    }

    /// The single reply to `op`, with failures as this Mac's errors.
    fn reply(&self, op: FolderOp) -> Result<PeerReply, ScanError> {
        match self.ask(op)?.reply().map_err(unreachable)? {
            PeerReply::Failed { error } => Err(wire::scan_error(error)),
            PeerReply::Refused { reason } => Err(ScanError::MacUnreachable(reason)),
            reply => Ok(reply),
        }
    }

    fn reply_unless(
        &self,
        op: FolderOp,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<PeerReply, ScanError> {
        match self
            .ask_unless(op, control)?
            .reply_unless(control)
            .map_err(unreachable)?
        {
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
            match exchange.next_unless(control).map_err(unreachable)? {
                Incoming::Data(chunk) => sink(&chunk).map_err(|source| ApplyError::Write {
                    path: path.clone(),
                    source: WriteError::Io(source),
                })?,
                Incoming::End => break,
                Incoming::Reply(_) => return Err(unexpected().into()),
            }
        }
        match exchange.reply_unless(control).map_err(unreachable)? {
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

fn lease_op(file: LeaseFile, action: LeaseAction) -> FolderOp {
    FolderOp::Lease { file, action }
}

fn scan_io(error: ScanError) -> io::Error {
    io::Error::other(error)
}

fn link_io(error: LinkError) -> io::Error {
    io::Error::other(error)
}

/// A catalog reply, with a refusal or a failure as this Mac's own error.
fn catalog_reply(reply: Result<PeerReply, LinkError>) -> io::Result<PeerReply> {
    match reply.map_err(link_io)? {
        PeerReply::Failed { error } => Err(catalog_failure(&error)),
        PeerReply::Refused { reason } => Err(io::Error::new(io::ErrorKind::Unsupported, reason)),
        reply => Ok(reply),
    }
}

/// A catalog failure on the answering Mac, as this Mac's own.
fn catalog_failure(error: &FolderError) -> io::Error {
    let kind = match error.kind {
        FolderErrorKind::Missing => io::ErrorKind::NotFound,
        FolderErrorKind::AccessDenied => io::ErrorKind::PermissionDenied,
        FolderErrorKind::Invalid => io::ErrorKind::InvalidData,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, error.message.clone())
}

fn pair_durability(durability: Durability) -> PairDurability {
    match durability {
        Durability::Standard => PairDurability::Standard,
        Durability::Full => PairDurability::Full,
    }
}

fn unreachable(error: LinkError) -> ScanError {
    match error {
        LinkError::Cancelled => ScanError::Cancelled,
        error => ScanError::MacUnreachable(error.to_string()),
    }
}

fn unexpected() -> ScanError {
    ScanError::Peer("The other Mac answered out of order".into())
}

/// Another Mac's folder always keeps its records, so a run that keeps no
/// archive there was refused when the pair was saved; nothing goes over
/// the wire that would archive anyway.
fn archived(run: &RunScope, path: &SyncPath) -> Result<(), ApplyError> {
    match run.archiving() {
        Archiving::Kept => Ok(()),
        Archiving::Off => Err(ApplyError::Unsupported(path.clone())),
    }
}

#[cfg(test)]
mod tests;
