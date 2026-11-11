//! Answering another Mac's operation on a folder this Mac shares with it.
//! The caller has checked that the share is granted to that Mac; this
//! opens the folder with the local endpoint and does what was asked.

use std::cell::{Cell, RefCell};
#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use foldover_fs::Resume;
use foldover_net::peer::MAX_MESSAGE;
use foldover_proto::local::PairDurability;
use foldover_proto::peer::{
    CatalogOp, FolderError, FolderErrorKind, FolderOp, Incoming, IncomingKind, LeaseAction,
    LeaseFile, NameCase, NameUnicode, PeerReply, RunSettings, ScanSettings, VolumeKind, WireItem,
    WireRun,
};
use foldover_types::{
    CaseMatching, Digest, Durability, EntryVersion, LocalRoot, MetadataPolicy, PairId, PairOptions,
    Propagation, ScanControl, Side, SyncPath, UnicodeMatching,
};

use super::CATALOG_CHUNK;
use crate::catalog::CATALOG_FILE_LIMIT;

use super::wire;
use crate::lease::{LEASE_LIMIT, LockFile};
use crate::link::Answer;
use crate::local::Carried;
use crate::{
    ArchivedItem, Expected, Filter, LocalEndpoint, LocalEntry, ObserveStep, ObservedEntry, Pruning,
    RootWatcher, RunScope,
};

/// How often a scan reports progress, which is also how soon it notices
/// that the asking Mac gave up.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
/// How often a quiet watch says it is still there.
const WATCH_HEARTBEAT: Duration = Duration::from_secs(60);
/// How often a watch looks whether this Mac is shutting down.
const WATCH_TICK: Duration = Duration::from_millis(250);
/// How long a burst of changes may last before it is reported.
const CHANGE_QUIET: Duration = Duration::from_millis(500);
/// Longest label a kept copy's name may carry.
const MAX_LABEL_BYTES: usize = 64;
/// The link prepends one discriminator byte to serialized JSON.
const REPLY_TAG_BYTES: usize = std::mem::size_of::<u8>();
const ITEM_SEPARATOR_BYTES: usize = b",".len();
/// PLAN.md §4: archives keep 30 days.
const MIN_SHARED_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Periodically proves a long local operation is still alive. Failure means
/// the asking Mac dropped or retired the request, so the operation stops at
/// its next checkpoint.
struct AnswerControl<'a> {
    answer: RefCell<&'a mut Answer>,
    heartbeat: Cell<Instant>,
    gone: Cell<bool>,
}

impl<'a> AnswerControl<'a> {
    fn new(answer: &'a mut Answer) -> Self {
        Self {
            answer: RefCell::new(answer),
            heartbeat: Cell::new(Instant::now()),
            gone: Cell::new(false),
        }
    }

    fn checkpoint(&self) -> ScanControl {
        if self.gone.get() {
            return ScanControl::Cancel;
        }
        if self.heartbeat.get().elapsed() < PROGRESS_INTERVAL {
            return ScanControl::Continue;
        }
        self.heartbeat.set(Instant::now());
        match self.answer.borrow_mut().heartbeat() {
            Ok(()) => ScanControl::Continue,
            Err(_) => {
                self.gone.set(true);
                ScanControl::Cancel
            }
        }
    }
}

/// The latest observation of each shared folder under each metadata
/// policy, so an unchanged file is not read again on the next scan.
/// Versions carry the policy's metadata digest, so a scan builds only on
/// one made under the same policy.
#[derive(Default)]
pub struct FolderCache(Mutex<HashMap<CacheKey, foldover_types::PathMap<ObservedEntry>>>);

type CacheKey = (Vec<u8>, MetadataPolicy);

impl FolderCache {
    /// The observation to build on for `root` scanned under `policy`.
    fn take(
        &self,
        root: &LocalRoot,
        policy: MetadataPolicy,
    ) -> foldover_types::PathMap<ObservedEntry> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(root.as_bytes().to_vec(), policy))
            .unwrap_or_else(foldover_types::PathMap::new)
    }

    fn keep(
        &self,
        root: &LocalRoot,
        policy: MetadataPolicy,
        entries: foldover_types::PathMap<ObservedEntry>,
    ) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((root.as_bytes().to_vec(), policy), entries);
    }
}

/// Answer one operation on the shared folder at `root`. `state` is this
/// agent's own state directory: where the shared folder holds it, that
/// subtree is never listed, read or changed for another Mac.
pub fn serve_folder(
    root: &LocalRoot,
    op: FolderOp,
    cache: &FolderCache,
    answer: &mut Answer,
    state: Option<&Path>,
) {
    if op == FolderOp::Watch {
        return watch(root, state, answer);
    }
    // A catalog operation opens the pair's private directory instead of
    // the shared folder itself, so its lease never meets the shared
    // folder's own. The share was already checked before this call.
    if let FolderOp::Catalog { pair, op } = &op {
        return serve_catalog(root, pair, op.clone(), answer, state);
    }
    // `open_protecting` canonicalizes the state path itself: pass it
    // through unchanged so resolution failures surface there instead of
    // silently disabling protection here. (A state path that is simply
    // gone is still "nothing to protect", matching every other caller.)
    let endpoint =
        match LocalEndpoint::open_protecting(Path::new(OsStr::from_bytes(root.as_bytes())), state)
            .and_then(|endpoint| endpoint.probe_access().map(|()| endpoint))
        {
            Ok(endpoint) => endpoint,
            Err(error) => return failed(answer, wire::scan_failure(&error)),
        };
    let reply = match op {
        FolderOp::Probe => Ok(PeerReply::Done),
        FolderOp::Identity => endpoint
            .identity()
            .map(|id| PeerReply::Identity {
                id: id.map(|id| id.to_string()),
            })
            .map_err(|error| wire::scan_failure(&error)),
        FolderOp::EnsureIdentity { durability } => endpoint
            .ensure_identity(durability_from(durability))
            .map(|id| PeerReply::Identity {
                id: Some(id.to_string()),
            })
            .map_err(|error| wire::apply_failure(&error)),
        FolderOp::IgnoreRules => endpoint
            .ignore_rules()
            .map(|rules| PeerReply::Rules { rules })
            .map_err(|error| wire::scan_failure(&error)),
        FolderOp::NameMatching => {
            let names = endpoint.name_matching();
            Ok(PeerReply::Names {
                case: match names.case() {
                    CaseMatching::Exact => NameCase::Exact,
                    CaseMatching::Insensitive => NameCase::Insensitive,
                },
                unicode: match names.unicode() {
                    UnicodeMatching::Exact => NameUnicode::Exact,
                    UnicodeMatching::Normalized => NameUnicode::Normalized,
                },
            })
        }
        FolderOp::Space => endpoint
            .available_space()
            .map(|bytes| PeerReply::Space { bytes })
            .map_err(|error| wire::scan_failure(&error)),
        FolderOp::Volume => endpoint
            .network_source()
            .map(|source| PeerReply::Volume {
                volume: match source {
                    Some(_) => VolumeKind::Network,
                    None => VolumeKind::Local,
                },
            })
            .map_err(|error| wire::scan_failure(&error)),
        FolderOp::Observe { scan } => return observe(&endpoint, root, scan, cache, answer),
        FolderOp::Watch | FolderOp::Catalog { .. } => {
            unreachable!("answered before opening the folder")
        }
        FolderOp::Lease { file, action } => lease(&endpoint, file, action),
        FolderOp::Read {
            path,
            entry,
            run,
            from,
        } => return read(&endpoint, &path, &entry, run, from, answer),
        FolderOp::Partial { content } => <[u8; Digest::BYTE_LEN]>::try_from(content.as_slice())
            .map_err(|_| malformed())
            .and_then(|content| {
                endpoint
                    .partial_length(&Digest::from_bytes(content))
                    .map_err(|error| wire::apply_failure(&error))
            })
            .map(|length| PeerReply::Partial { length }),
        FolderOp::Receive { path, entry } => receive(&endpoint, &path, entry, answer),
        FolderOp::Metadata { path, entry, run } => {
            decoded(&path, Some(&entry), run).and_then(|(path, entry, run)| {
                let entry = entry.ok_or_else(malformed)?;
                let controlled = AnswerControl::new(answer);
                let mut checkpoint = || controlled.checkpoint();
                let source = endpoint
                    .source_entry(&path, &entry, &run, &mut checkpoint)
                    .map_err(|error| wire::apply_failure(&error))?;
                reading(&endpoint, &source, &run)
            })
        }
        FolderOp::ReceiveMetadata {
            path,
            current,
            metadata,
            run,
        } => decoded(&path, Some(&current), run).and_then(|(path, current, run)| {
            let current = current.ok_or_else(malformed)?;
            let (metadata, native) = wire::metadata_from(metadata);
            let carried = Carried {
                metadata,
                native: native.as_ref(),
            };
            let controlled = AnswerControl::new(answer);
            let mut checkpoint = || controlled.checkpoint();
            endpoint
                .receive_metadata(&path, &current, carried, &run, &mut checkpoint)
                .map(received)
                .map_err(|error| wire::apply_failure(&error))
        }),
        FolderOp::Remove { path, current, run } => {
            decoded(&path, Some(&current), run).and_then(|(path, current, run)| {
                let current = current.ok_or_else(malformed)?;
                let controlled = AnswerControl::new(answer);
                let mut checkpoint = || controlled.checkpoint();
                endpoint
                    .remove(&path, &current, &run, &mut checkpoint)
                    .map(|()| PeerReply::Done)
                    .map_err(|error| wire::apply_failure(&error))
            })
        }
        FolderOp::RecoverLeftover { path, run } => {
            decoded(&path, None, run).and_then(|(path, _, run)| {
                endpoint
                    .recover_leftover(&path, &run)
                    .map(|()| PeerReply::Done)
                    .map_err(|error| wire::apply_failure(&error))
            })
        }
        // This Mac's clock decides what is old here.
        FolderOp::PruneArchive { retention_seconds } => {
            // A large archive prunes longer than a caller waits without
            // word: each deleted entry is a checkpoint that proves the
            // work and notices a Stop.
            let controlled = AnswerControl::new(answer);
            let mut checkpoint = || controlled.checkpoint();
            endpoint
                .prune_archive(
                    shared_retention(retention_seconds),
                    SystemTime::now(),
                    Pruning::Everyone,
                    &mut checkpoint,
                )
                .map(|count| PeerReply::Pruned { count })
                .map_err(|error| wire::apply_failure(&error))
        }
        FolderOp::ArchivedRuns => endpoint
            .archived_runs()
            .map(|runs| PeerReply::Runs {
                runs: runs
                    .iter()
                    .map(|run| WireRun {
                        name: run.name().to_owned(),
                        started_ms: wire::milliseconds(run.started()),
                    })
                    .collect(),
            })
            .map_err(|error| wire::scan_failure(&error)),
        FolderOp::ArchivedItems { run, after, limit } => {
            let after = after.map(|after| SyncPath::try_from(after.as_slice()));
            match after.transpose() {
                Err(_) => Err(malformed()),
                Ok(after) => endpoint
                    .archived_items(
                        &run,
                        after.as_ref(),
                        usize::try_from(limit).unwrap_or(usize::MAX),
                    )
                    .map_err(|error| wire::scan_failure(&error))
                    .and_then(archived_reply),
            }
        }
        FolderOp::PutBack {
            run,
            path,
            durability,
        } => match SyncPath::try_from(path.as_slice()) {
            Err(_) => Err(malformed()),
            Ok(path) => endpoint
                .put_back(&run, &path, durability_from(durability))
                .map(placed)
                .map_err(|error| wire::apply_failure(&error)),
        },
        FolderOp::KeepCopy {
            path,
            version,
            from,
            options,
        } => {
            let path = SyncPath::try_from(path.as_slice()).ok();
            let version = wire::version_from(&version);
            let options = PairOptions::try_from(options).ok();
            match (path, version, options) {
                // The label becomes part of a file name here.
                (Some(path), Some(version), Some(options)) if plain_label(&from) => {
                    let controlled = AnswerControl::new(answer);
                    let mut checkpoint = || controlled.checkpoint();
                    endpoint
                        .keep_copy(&path, version, &from, &options, &mut checkpoint)
                        .map(placed)
                        .map_err(|error| wire::apply_failure(&error))
                }
                _ => Err(malformed()),
            }
        }
    };
    match reply {
        Ok(reply) => {
            let _ = answer.reply(&reply);
        }
        Err(error) => failed(answer, error),
    }
}

/// Keep the longest prefix that fits one framed reply. Paths have no
/// fixed length limit, so an item-count bound alone does not fit every page.
fn archived_reply(page: (Vec<ArchivedItem>, bool)) -> Result<PeerReply, FolderError> {
    let (items, mut more) = page;
    let too_large = || FolderError {
        kind: FolderErrorKind::Invalid,
        path: None,
        message: "An archived path is too large for a peer reply".into(),
    };
    // `false` is longer than `true`, leaving room when truncation sets more.
    let envelope = PeerReply::Items {
        items: Vec::new(),
        more: false,
    };
    let mut bytes = serde_json::to_vec(&envelope)
        .map_err(|_| too_large())?
        .len()
        + REPLY_TAG_BYTES;
    let mut kept = Vec::new();
    for item in items {
        let item = WireItem {
            path: item.path().as_bytes().to_vec(),
            size: item.size(),
        };
        let item_bytes = serde_json::to_vec(&item).map_err(|_| too_large())?.len();
        let separator = if kept.is_empty() {
            0
        } else {
            ITEM_SEPARATOR_BYTES
        };
        let needed = item_bytes.saturating_add(separator);
        if needed > MAX_MESSAGE.saturating_sub(bytes) {
            if kept.is_empty() {
                return Err(too_large());
            }
            more = true;
            break;
        }
        bytes += needed;
        kept.push(item);
    }
    Ok(PeerReply::Items { items: kept, more })
}

/// Scan, reporting progress; then send the entries in batches and the
/// rest of the observation.
fn observe(
    endpoint: &LocalEndpoint,
    root: &LocalRoot,
    scan: ScanSettings,
    cache: &FolderCache,
    answer: &mut Answer,
) {
    let filter = match Filter::new(scan.rules.iter().map(String::as_str)) {
        Ok(filter) => filter.with_size_limit(scan.size_limit).with_cutoff(
            scan.cutoff_ms
                .map(|ms| UNIX_EPOCH + Duration::from_millis(ms)),
        ),
        Err(error) => {
            return failed(
                answer,
                FolderError {
                    kind: FolderErrorKind::Invalid,
                    path: None,
                    message: error.to_string(),
                },
            );
        }
    };
    let policy = MetadataPolicy::new(
        scan.modification_times.into(),
        scan.extended_attributes.into(),
        scan.permissions.into(),
    )
    .with_quarantine(scan.quarantine.map_or(Propagation::Sync, Into::into));
    let previous = cache.take(root, policy);

    // The asking Mac gives up by dropping the request. Report from the
    // control checkpoint rather than from completed entries, so hashing one
    // large file still proves the scan is alive and notices Stop.
    let answer = RefCell::new(answer);
    let gone = Cell::new(false);
    let listed = Cell::new(0);
    let checked = Cell::new(0);
    let reported = Cell::new(Instant::now());
    let observed = endpoint.observe_owned(
        policy,
        &filter,
        previous,
        Arc::new(foldover_types::PathMap::new()),
        Side::Left,
        crate::ObservationHints::default(),
        &mut || {
            if gone.get() {
                return ScanControl::Cancel;
            }
            if reported.get().elapsed() >= PROGRESS_INTERVAL {
                reported.set(Instant::now());
                if answer
                    .borrow_mut()
                    .reply(&PeerReply::Progress {
                        listed: listed.get(),
                        checked: checked.get(),
                    })
                    .is_err()
                {
                    gone.set(true);
                    return ScanControl::Cancel;
                }
            }
            ScanControl::Continue
        },
        &mut |step| match step {
            ObserveStep::Listed => listed.set(listed.get() + 1),
            ObserveStep::Checked => checked.set(checked.get() + 1),
        },
    );
    let answer = answer.into_inner();
    let observation = match observed {
        Ok(observation) => observation,
        Err(error) => return failed(answer, wire::scan_failure(&error)),
    };

    // Reports go out at most every PROGRESS_INTERVAL, so the last one can
    // be up to an interval behind, and a shorter scan sent none. The final
    // counts end the asking Mac's count on every entry. Like any report's,
    // they grew by at most an interval's steps, which the asking Mac's
    // per-report cap allows.
    if answer
        .reply(&PeerReply::Progress {
            listed: listed.get(),
            checked: checked.get(),
        })
        .is_err()
    {
        return;
    }

    for path in observation.unstable_directories() {
        if answer
            .reply(&PeerReply::UnstableDirectory {
                path: path.as_ref().map(|path| path.as_bytes().to_vec()),
            })
            .is_err()
        {
            return;
        }
    }
    for issue in observation.issues() {
        if answer
            .reply(&PeerReply::ScanIssue {
                path: issue.path().as_bytes().to_vec(),
                message: issue.message().to_owned(),
            })
            .is_err()
        {
            return;
        }
    }
    let mut batch = Vec::new();
    for (path, entry) in observation.entries() {
        wire::put_entry(&mut batch, path, entry);
        if batch.len() >= wire::BATCH_BYTES {
            if answer.send_data(&batch).is_err() {
                return;
            }
            batch.clear();
        }
    }
    let bytes = |paths: &[SyncPath]| paths.iter().map(|path| path.as_bytes().to_vec()).collect();
    let sent = (batch.is_empty() || answer.send_data(&batch).is_ok())
        && answer.end_data().is_ok()
        && answer
            .reply(&PeerReply::Observed {
                leftovers: bytes(observation.leftovers()),
                oversized: bytes(observation.oversized()),
                old: bytes(observation.old()),
                unavailable: bytes(observation.unavailable()),
            })
            .is_ok();
    if sent {
        cache.keep(root, policy, observation.into_snapshot());
    }
}

/// One step of the lease protocol on this Mac's folder.
fn lease(
    endpoint: &LocalEndpoint,
    file: LeaseFile,
    action: LeaseAction,
) -> Result<PeerReply, FolderError> {
    let file = match file {
        LeaseFile::Lock => LockFile::Lock,
        LeaseFile::Claim => LockFile::Claim,
    };
    let done = |result: Result<(), crate::ApplyError>| {
        result
            .map(|()| PeerReply::Done)
            .map_err(|error| wire::apply_failure(&error))
    };
    // Reads stop at `LEASE_LIMIT`: a longer record would leave this
    // folder's lease unreadable to every Mac, this one included.
    if let LeaseAction::Create { content } | LeaseAction::Replace { content } = &action
        && content.len() > LEASE_LIMIT
    {
        return Err(FolderError {
            kind: FolderErrorKind::Invalid,
            path: None,
            message: "A lease record is too large".into(),
        });
    }
    match action {
        LeaseAction::Read => endpoint
            .read_lock_file(file)
            .map(|content| PeerReply::LeaseRead { content })
            .map_err(|error| wire::scan_failure(&error)),
        LeaseAction::Create { content } => endpoint
            .create_lock_file(file, &content)
            .map(|created| PeerReply::LeaseCreated { created })
            .map_err(|error| wire::apply_failure(&error)),
        LeaseAction::Replace { content } => done(endpoint.replace_lock_file(file, &content)),
        LeaseAction::Remove => done(endpoint.remove_lock_file(file)),
        LeaseAction::Promote => done(endpoint.promote_claim()),
    }
}

/// Answer one operation on a pair's target catalog: the private directory
/// `.foldover/state/<pair>` inside the shared folder. The pair is checked
/// here, and the directory is opened from the shared folder's own
/// endpoint, so share access control holds and the catalog's lease never
/// meets the shared folder's own.
fn serve_catalog(
    root: &LocalRoot,
    pair: &str,
    op: CatalogOp,
    answer: &mut Answer,
    state: Option<&Path>,
) {
    let Ok(pair) = pair.parse::<PairId>() else {
        return failed(answer, malformed());
    };
    // `open_protecting` canonicalizes the state path itself: pass it
    // through unchanged, as every other caller does.
    let endpoint =
        match LocalEndpoint::open_protecting(Path::new(OsStr::from_bytes(root.as_bytes())), state)
            .and_then(|endpoint| endpoint.probe_access().map(|()| endpoint))
        {
            Ok(endpoint) => endpoint,
            Err(error) => return failed(answer, wire::scan_failure(&error)),
        };
    // `Open` creates and checks the directory's marker with the asking
    // Mac's durability; every other operation finds it there already, so
    // a marker write needs no stronger promise than a lease record's.
    let durability = match &op {
        CatalogOp::Open { durability } => durability_from(*durability),
        _ => Durability::Standard,
    };
    let catalog = match endpoint.catalog_endpoint(pair, durability) {
        Ok(catalog) => catalog,
        Err(error) => return failed(answer, catalog_failure(&error)),
    };
    match op {
        CatalogOp::Open { .. } => reply(answer, Ok(PeerReply::Done)),
        CatalogOp::Read { name, limit } => catalog_read(&catalog, &name, limit, answer),
        CatalogOp::Write { name, durability } => catalog_write(&catalog, &name, durability, answer),
        CatalogOp::List => catalog_list(&catalog, answer),
        CatalogOp::Remove { name } => reply(answer, catalog_remove(&catalog, &name)),
        CatalogOp::Lease { file, action } => reply(answer, lease(&catalog, file, action)),
    }
}

/// Send one reply, or the failure that replaced it.
fn reply(answer: &mut Answer, reply: Result<PeerReply, FolderError>) {
    match reply {
        Ok(reply) => {
            let _ = answer.reply(&reply);
        }
        Err(error) => failed(answer, error),
    }
}

/// Send one catalog file: `catalogRead`, its data frames and their end,
/// then `done`. The endpoint refuses a name that is not a regular file
/// and a read beyond its limit.
fn catalog_read(endpoint: &LocalEndpoint, name: &str, limit: u64, answer: &mut Answer) {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    if limit > CATALOG_FILE_LIMIT {
        return failed(answer, catalog_too_large());
    }
    match endpoint.catalog_read(name, limit) {
        Ok(None) => {
            let _ = answer.reply(&PeerReply::CatalogRead { found: false });
        }
        Ok(Some(bytes)) => {
            if answer
                .reply(&PeerReply::CatalogRead { found: true })
                .is_err()
            {
                return;
            }
            // A file of at most 16 MiB leaves in 1 MiB frames.
            for chunk in bytes.chunks(CATALOG_CHUNK) {
                if answer.send_data(chunk).is_err() {
                    return;
                }
            }
            if answer.end_data().is_err() {
                return;
            }
            let _ = answer.reply(&PeerReply::Done);
        }
        Err(error) => failed(answer, catalog_failure(&error)),
    }
}

/// Write one catalog file from the asking Mac's data frames, creating it
/// only. Bytes beyond the catalog's limit are refused before any file is
/// written, and an existing file is reported rather than replaced.
fn catalog_write(
    endpoint: &LocalEndpoint,
    name: &str,
    durability: PairDurability,
    answer: &mut Answer,
) {
    let mut bytes = Vec::new();
    loop {
        match answer.next_data() {
            Ok(Some(chunk)) => {
                if bytes.len().saturating_add(chunk.len()) > CATALOG_FILE_LIMIT {
                    return failed(answer, catalog_too_large());
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(error) => {
                return failed(answer, catalog_failure(&io::Error::other(error)));
            }
        }
    }
    match endpoint.catalog_write(name, &bytes, durability_from(durability)) {
        Ok(()) => {
            let _ = answer.reply(&PeerReply::CatalogCreated { created: true });
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let _ = answer.reply(&PeerReply::CatalogCreated { created: false });
        }
        Err(error) => failed(answer, catalog_failure(&error)),
    }
}

/// Send the catalog's file names a frame-sized page at a time; `more`
/// says whether another follows, so the 250,000-file limit never
/// overflows one reply.
fn catalog_list(endpoint: &LocalEndpoint, answer: &mut Answer) {
    let names = match endpoint.catalog_list() {
        Ok(names) => names,
        Err(error) => return failed(answer, catalog_failure(&error)),
    };
    let mut rest = names.as_slice();
    while !rest.is_empty() {
        let (page, more) = catalog_page(rest);
        if page.is_empty() {
            return failed(
                answer,
                catalog_failure(&io::Error::new(
                    io::ErrorKind::InvalidData,
                    "A catalog name is too large for a peer reply",
                )),
            );
        }
        if answer
            .reply(&PeerReply::CatalogListed {
                names: page.to_vec(),
                more,
            })
            .is_err()
        {
            return;
        }
        if !more {
            return;
        }
        rest = &rest[page.len()..];
    }
    let _ = answer.reply(&PeerReply::CatalogListed {
        names: Vec::new(),
        more: false,
    });
}

/// The longest prefix of `names` whose page fits one framed reply, and
/// whether more names follow.
fn catalog_page(names: &[String]) -> (&[String], bool) {
    let mut bytes = serde_json::to_vec(&PeerReply::CatalogListed {
        names: Vec::new(),
        more: true,
    })
    .map_or(usize::MAX, |json| json.len())
    .saturating_add(REPLY_TAG_BYTES);
    let mut kept = 0;
    for (index, name) in names.iter().enumerate() {
        let item = serde_json::to_string(name)
            .map_or(usize::MAX, |text| text.len())
            .saturating_add(",".len());
        if bytes.saturating_add(item) > MAX_MESSAGE {
            break;
        }
        bytes = bytes.saturating_add(item);
        kept = index + 1;
    }
    (&names[..kept], kept < names.len())
}

/// Remove one catalog file; nothing to do if it is absent. A directory
/// or another non-regular entry is refused, never unlinked.
fn catalog_remove(endpoint: &LocalEndpoint, name: &str) -> Result<PeerReply, FolderError> {
    endpoint
        .catalog_remove(name)
        .map(|()| PeerReply::Done)
        .map_err(|error| catalog_failure(&error))
}

/// A catalog failure on this Mac, for the asking one.
fn catalog_failure(error: &io::Error) -> FolderError {
    let kind = match error.kind() {
        io::ErrorKind::NotFound => FolderErrorKind::Missing,
        io::ErrorKind::PermissionDenied => FolderErrorKind::AccessDenied,
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput => FolderErrorKind::Invalid,
        _ => FolderErrorKind::Other,
    };
    FolderError {
        kind,
        path: None,
        message: error.to_string(),
    }
}

fn catalog_too_large() -> FolderError {
    FolderError {
        kind: FolderErrorKind::Invalid,
        path: None,
        message: "A catalog file exceeds the 16 MiB limit".into(),
    }
}

/// Report changes in the folder until the asking Mac gives up: a burst of
/// notifications is one `changed`, and `watching` every minute without
/// changes proves the request is still wanted.
fn watch(root: &LocalRoot, state: Option<&Path>, answer: &mut Answer) {
    let (sender, changes) = mpsc::channel();
    let watcher = RootWatcher::new(move |()| {
        let _ = sender.send(());
    });
    let mut watcher = match watcher {
        Ok(watcher) => watcher,
        Err(error) => return failed(answer, watch_failure(&error)),
    };
    if let Some((_, error)) = watcher.set_roots(&[(root.clone(), ())], state).first() {
        return failed(answer, watch_failure(error));
    }
    let mut reply = PeerReply::Watching;
    loop {
        if answer.closing() || answer.reply(&reply).is_err() {
            return;
        }
        let sent = Instant::now();
        // Short waits, so this Mac shutting down never waits for the
        // next heartbeat.
        reply = loop {
            if answer.closing() {
                return;
            }
            match changes.recv_timeout(WATCH_TICK) {
                Ok(()) => {
                    // One report per burst, e.g. a folder of files copied in.
                    std::thread::sleep(CHANGE_QUIET);
                    while changes.try_recv().is_ok() {}
                    break PeerReply::Changed;
                }
                Err(RecvTimeoutError::Timeout) if sent.elapsed() >= WATCH_HEARTBEAT => {
                    break PeerReply::Watching;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        };
    }
}

fn watch_failure(error: &std::io::Error) -> FolderError {
    FolderError {
        kind: FolderErrorKind::Other,
        path: None,
        message: format!("That Mac cannot watch the folder: {error}"),
    }
}

/// Send a verified entry: `reading`, a file's data and its end, then
/// `done`. A failure after `reading` comes after the end of the data.
fn read(
    endpoint: &LocalEndpoint,
    path: &[u8],
    entry: &[u8],
    run: RunSettings,
    from: u64,
    answer: &mut Answer,
) {
    let controlled = AnswerControl::new(answer);
    let mut checkpoint = || controlled.checkpoint();
    let source = decoded(path, Some(entry), run).and_then(|(path, entry, run)| {
        let entry = entry.ok_or_else(malformed)?;
        let source = endpoint
            .source_entry(&path, &entry, &run, &mut checkpoint)
            .map_err(|error| wire::apply_failure(&error))?;
        let reading = reading(endpoint, &source, &run)?;
        Ok((source, reading))
    });
    let (source, reading) = match source {
        Ok(source) => source,
        Err(error) => {
            return failed(&mut controlled.answer.borrow_mut(), error);
        }
    };
    if controlled.answer.borrow_mut().reply(&reading).is_err() {
        return;
    }
    let mut sent = Ok(());
    if source.metadata().kind() == foldover_fs::NodeKind::File {
        sent = endpoint.read_entry_from(&source, from, &mut checkpoint, &mut |chunk| {
            controlled
                .answer
                .borrow_mut()
                .send_data(chunk)
                .map_err(std::io::Error::other)
        });
    }
    if controlled.answer.borrow_mut().end_data().is_err() {
        return;
    }
    match sent {
        Ok(()) => {
            let _ = controlled.answer.borrow_mut().reply(&PeerReply::Done);
        }
        Err(error) => failed(
            &mut controlled.answer.borrow_mut(),
            wire::apply_failure(&error),
        ),
    }
}

/// Place what the asking Mac sends, staged and verified like any copy.
fn receive(
    endpoint: &LocalEndpoint,
    path: &[u8],
    entry: Incoming,
    answer: &mut Answer,
) -> Result<PeerReply, FolderError> {
    let controlled = AnswerControl::new(answer);
    let mut checkpoint = || controlled.checkpoint();
    let (path, current, run) = decoded(path, entry.current.as_deref(), entry.run)?;
    let (metadata, native) = wire::metadata_from(entry.metadata);
    let carried = Carried {
        metadata,
        native: native.as_ref(),
    };
    let current = match &current {
        Some(current) => Expected::Present(current),
        None => Expected::Absent,
    };
    let destination = endpoint
        .receiving(&path, current, &run, &mut checkpoint)
        .map_err(|error| wire::apply_failure(&error))?;
    let version = match entry.kind {
        IncomingKind::File { content, from } => {
            let content =
                <[u8; Digest::BYTE_LEN]>::try_from(content.as_slice()).map_err(|_| malformed())?;
            let resume = Resume {
                content: Digest::from_bytes(content),
                from,
            };
            endpoint.receive_file(
                &path,
                destination.as_ref(),
                carried,
                resume,
                &run,
                &mut checkpoint,
                |sink| {
                    while let Some(chunk) = controlled
                        .answer
                        .borrow_mut()
                        .next_data()
                        .map_err(std::io::Error::other)?
                    {
                        sink(&chunk)?;
                    }
                    Ok(())
                },
            )
        }
        IncomingKind::Folder if destination.is_some() => {
            Err(crate::ApplyError::Unsupported(path.clone()))
        }
        IncomingKind::Folder => endpoint.receive_directory(&path, carried, &run, &mut checkpoint),
        IncomingKind::Link { target } => {
            endpoint.receive_link(&path, destination.as_ref(), &target, &run, &mut checkpoint)
        }
    };
    version
        .map(received)
        .map_err(|error| wire::apply_failure(&error))
}

/// A request's path, expected entry and run, or why they are unusable.
fn decoded(
    path: &[u8],
    entry: Option<&[u8]>,
    run: RunSettings,
) -> Result<(SyncPath, Option<ObservedEntry>, RunScope), FolderError> {
    let path = SyncPath::try_from(path).map_err(|_| malformed())?;
    let entry = entry
        .map(|entry| wire::entry_from(entry).ok_or_else(malformed))
        .transpose()?;
    let run = wire::run_from(run).ok_or_else(malformed)?;
    Ok((path, entry, run))
}

/// A verified entry's metadata for the asking Mac: mode and time, and
/// the attributes and flags the pair syncs.
fn reading(
    endpoint: &LocalEndpoint,
    source: &LocalEntry,
    run: &RunScope,
) -> Result<PeerReply, FolderError> {
    let native = match source.link_target() {
        Some(_) => None,
        None => Some(
            endpoint
                .native_metadata(source, run.options().metadata())
                .map_err(|error| wire::apply_failure(&error))?,
        ),
    };
    Ok(PeerReply::Reading {
        metadata: wire::metadata_wire(LocalEndpoint::foreign_metadata(source), native.as_ref()),
        target: source.link_target().map(<[u8]>::to_vec),
    })
}

fn received(version: EntryVersion) -> PeerReply {
    PeerReply::Received {
        version: wire::version_bytes(version),
    }
}

fn placed(path: SyncPath) -> PeerReply {
    PeerReply::Placed {
        path: path.as_bytes().to_vec(),
    }
}

/// How long this Mac keeps archived runs when a paired Mac prunes them:
/// never less than its own archives keep.
fn shared_retention(requested_seconds: u64) -> Duration {
    Duration::from_secs(requested_seconds).max(MIN_SHARED_RETENTION)
}

/// A short label without path separators or control characters, such as
/// "Folder B".
fn plain_label(label: &str) -> bool {
    !label.trim().is_empty()
        && label.len() <= MAX_LABEL_BYTES
        && !label.chars().any(|c| c == '/' || c.is_control())
}

fn durability_from(durability: PairDurability) -> Durability {
    match durability {
        PairDurability::Standard => Durability::Standard,
        PairDurability::Full => Durability::Full,
    }
}

fn malformed() -> FolderError {
    FolderError {
        kind: FolderErrorKind::Invalid,
        path: None,
        message: "That Mac sent a request this Mac cannot read".into(),
    }
}

fn failed(answer: &mut Answer, error: FolderError) {
    let _ = answer.reply(&PeerReply::Failed { error });
}

#[cfg(test)]
mod tests {
    use foldover_types::{Digest, EntryContent, EntryVersion, Propagation};

    use super::*;

    /// Review: two pairs scanning one share under different metadata
    /// policies shared one cache, so one reused versions the other's
    /// policy had computed.
    #[test]
    fn a_scan_builds_only_on_one_made_under_the_same_policy() {
        let root = LocalRoot::from_bytes(b"/Users/b/Photos".to_vec()).unwrap();
        let with_times =
            MetadataPolicy::new(Propagation::Sync, Propagation::Ignore, Propagation::Ignore);
        let without = MetadataPolicy::new(
            Propagation::Ignore,
            Propagation::Ignore,
            Propagation::Ignore,
        );
        let entry = ObservedEntry::for_tests(
            EntryVersion::new(EntryContent::Directory, Digest::from_bytes([1; 32])),
            0,
        );
        let entries = BTreeMap::from([(SyncPath::try_from(&b"album"[..]).unwrap(), entry)]);
        let cache = FolderCache::default();

        cache.keep(&root, with_times, entries.clone().into_iter().collect());
        assert!(cache.take(&root, without).is_empty());
        cache.keep(&root, with_times, entries.clone().into_iter().collect());
        assert!(cache.take(&root, with_times).iter().eq(entries.iter()));
    }

    /// Review: the asking Mac's clock and retention decided what this
    /// Mac's archive kept, so a wrong clock could empty it.
    #[test]
    fn a_paired_mac_cannot_shorten_this_macs_archive() {
        assert_eq!(shared_retention(0), MIN_SHARED_RETENTION);
        assert_eq!(
            shared_retention(MIN_SHARED_RETENTION.as_secs() * 2),
            MIN_SHARED_RETENTION * 2
        );
    }

    /// A paired Mac writes lease records here. One longer than a read
    /// accepts is refused before it reaches the disk.
    #[test]
    fn a_lease_record_longer_than_a_read_accepts_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();
        let long = vec![b'x'; LEASE_LIMIT + 1];
        let fits = vec![b'x'; LEASE_LIMIT];

        for action in [
            LeaseAction::Create {
                content: long.clone(),
            },
            LeaseAction::Replace { content: long },
        ] {
            let error = lease(&endpoint, LeaseFile::Lock, action).unwrap_err();
            assert_eq!(error.kind, FolderErrorKind::Invalid);
        }
        assert_eq!(
            lease(&endpoint, LeaseFile::Lock, LeaseAction::Read).unwrap(),
            PeerReply::LeaseRead { content: None }
        );

        // The longest record a read accepts still goes through.
        assert_eq!(
            lease(
                &endpoint,
                LeaseFile::Lock,
                LeaseAction::Create {
                    content: fits.clone()
                }
            )
            .unwrap(),
            PeerReply::LeaseCreated { created: true }
        );
        assert_eq!(
            lease(&endpoint, LeaseFile::Lock, LeaseAction::Read).unwrap(),
            PeerReply::LeaseRead {
                content: Some(fits)
            }
        );
    }

    #[test]
    fn kept_copy_labels_stay_one_plain_name() {
        assert!(plain_label("Folder B"));
        for label in [
            "",
            "  ",
            "a/b",
            "line\nbreak",
            &"x".repeat(MAX_LABEL_BYTES + 1),
        ] {
            assert!(!plain_label(label), "{label:?}");
        }
    }

    #[test]
    fn runs_named_by_another_mac_are_one_folder() {
        let options = PairOptions::default();
        assert!(RunScope::named("2026-10-20T10-00-00.000Z", options.clone()).is_some());
        for name in ["", ".", "..", "a/b", "../x"] {
            assert!(RunScope::named(name, options.clone()).is_none(), "{name}");
        }
    }

    /// The listing pages at a frame's worth of names, so a catalog of the
    /// 250,000-file limit never overflows one reply; a name that alone
    /// would overflow a frame is left out, not sent forever.
    #[test]
    fn catalog_listings_page_within_one_frame() {
        let names: Vec<String> = (0..500)
            .map(|index| format!("c-0000-{index:016x}-{}", "a".repeat(100)))
            .collect();
        let mut rest = names.as_slice();
        let mut pages = 0;
        let mut seen = 0;
        while !rest.is_empty() {
            let (page, more) = catalog_page(rest);
            assert!(!page.is_empty(), "an empty page would never finish");
            let reply = PeerReply::CatalogListed {
                names: page.to_vec(),
                more,
            };
            assert!(
                serde_json::to_vec(&reply).unwrap().len() + REPLY_TAG_BYTES <= MAX_MESSAGE,
                "page {pages} overflows one frame"
            );
            seen += page.len();
            pages += 1;
            assert_eq!(more, seen < names.len());
            rest = &rest[page.len()..];
        }
        assert_eq!(seen, names.len());
        assert_eq!(pages, 1, "500 short names fit one page");

        // A name that alone would overflow a frame is left out entirely;
        // the listing refuses it rather than looping on an empty page.
        let huge = vec!["x".repeat(MAX_MESSAGE)];
        let (page, _) = catalog_page(&huge);
        assert!(page.is_empty());
    }
}

#[cfg(test)]
mod archive_reply_tests {
    use super::*;

    fn item(index: usize) -> ArchivedItem {
        let path = format!("{}{:04}", "folder/".repeat(650), index);
        ArchivedItem::new(SyncPath::try_from(path.as_bytes()).unwrap(), index as u64)
    }

    #[test]
    fn long_archive_paths_page_within_the_framed_reply_limit() {
        let all: Vec<_> = (0..1_000).map(item).collect();
        let full = PeerReply::Items {
            items: all
                .iter()
                .map(|item| WireItem {
                    path: item.path().as_bytes().to_vec(),
                    size: item.size(),
                })
                .collect(),
            more: false,
        };
        assert!(serde_json::to_vec(&full).unwrap().len() > MAX_MESSAGE);

        let reply = archived_reply((all.clone(), false)).unwrap();
        assert!(serde_json::to_vec(&reply).unwrap().len() + REPLY_TAG_BYTES <= MAX_MESSAGE);
        let PeerReply::Items { items, more } = reply else {
            panic!("not an item page")
        };
        assert!(more);
        assert!(!items.is_empty());
        assert!(items.len() < all.len());
        let end = items.len();
        for (actual, expected) in items.iter().zip(&all) {
            assert_eq!(actual.path, expected.path().as_bytes());
        }

        let reply = archived_reply((all[end..].to_vec(), false)).unwrap();
        assert!(serde_json::to_vec(&reply).unwrap().len() + REPLY_TAG_BYTES <= MAX_MESSAGE);
        let PeerReply::Items { items, more } = reply else {
            panic!("not an item page")
        };
        assert!(!more);
        assert_eq!(items.len(), all.len() - end);
        assert_eq!(items[0].path, all[end].path().as_bytes());
    }

    #[test]
    fn a_single_unrepresentable_archived_path_returns_an_error() {
        let path = SyncPath::try_from(vec![b'x'; MAX_MESSAGE / 4]).unwrap();
        let Err(error) = archived_reply((vec![ArchivedItem::new(path, 1)], false)) else {
            panic!("an unrepresentable path must return an explicit error");
        };
        assert_eq!(error.kind, FolderErrorKind::Invalid);
        assert!(error.path.is_none());
    }

    #[test]
    fn archive_reply_preserves_more_and_empty_pages() {
        for more in [false, true] {
            let reply = archived_reply((vec![], more)).unwrap();
            assert_eq!(
                reply,
                PeerReply::Items {
                    items: vec![],
                    more
                }
            );
        }
    }
}
