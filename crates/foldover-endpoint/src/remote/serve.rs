//! Answering another Mac's operation on a folder this Mac shares with it.
//! The caller has checked that the share is granted to that Mac; this
//! opens the folder with the local endpoint and does what was asked.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use foldover_fs::Resume;
use foldover_proto::local::PairDurability;
use foldover_proto::peer::{
    FolderError, FolderErrorKind, FolderOp, Incoming, IncomingKind, LeaseAction, LeaseFile,
    NameCase, NameUnicode, PeerReply, RunSettings, ScanSettings, VolumeKind, WireItem, WireRun,
};
use foldover_types::{
    CaseMatching, Digest, Durability, EntryVersion, LocalRoot, MetadataPolicy, PairOptions,
    Propagation, ScanControl, SyncPath, UnicodeMatching,
};

use super::wire;
use crate::lease::LockFile;
use crate::link::Answer;
use crate::local::Carried;
use crate::{
    Expected, Filter, LocalEndpoint, LocalEntry, ObserveStep, ObservedEntry, RootWatcher, RunScope,
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
/// PLAN.md §4: archives keep 30 days.
const MIN_SHARED_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// The latest observation of each shared folder under each metadata
/// policy, so an unchanged file is not read again on the next scan.
/// Versions carry the policy's metadata digest, so a scan builds only on
/// one made under the same policy.
#[derive(Default)]
pub struct FolderCache(Mutex<HashMap<CacheKey, BTreeMap<SyncPath, ObservedEntry>>>);

type CacheKey = (Vec<u8>, MetadataPolicy);

impl FolderCache {
    /// The observation to build on for `root` scanned under `policy`.
    fn take(&self, root: &LocalRoot, policy: MetadataPolicy) -> BTreeMap<SyncPath, ObservedEntry> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(root.as_bytes().to_vec(), policy))
            .unwrap_or_default()
    }

    fn keep(
        &self,
        root: &LocalRoot,
        policy: MetadataPolicy,
        entries: BTreeMap<SyncPath, ObservedEntry>,
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
    // `open_protecting` canonicalizes internally too; resolving here keeps
    // this consistent with the watch path and any future consumer.
    let state = state.and_then(|path| path.canonicalize().ok());
    let endpoint = match LocalEndpoint::open_protecting(
        Path::new(OsStr::from_bytes(root.as_bytes())),
        state.as_deref(),
    )
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
        FolderOp::Watch => unreachable!("answered before opening the folder"),
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
                let source = endpoint
                    .source_entry(&path, &entry, &run, &mut proceed)
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
            endpoint
                .receive_metadata(&path, &current, carried, &run, &mut proceed)
                .map(received)
                .map_err(|error| wire::apply_failure(&error))
        }),
        FolderOp::Remove { path, current, run } => {
            decoded(&path, Some(&current), run).and_then(|(path, current, run)| {
                let current = current.ok_or_else(malformed)?;
                endpoint
                    .remove(&path, &current, &run, &mut proceed)
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
        FolderOp::PruneArchive { retention_seconds } => endpoint
            .prune_archive(shared_retention(retention_seconds), SystemTime::now())
            .map(|count| PeerReply::Pruned { count })
            .map_err(|error| wire::apply_failure(&error)),
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
                    .map(|(items, more)| PeerReply::Items {
                        items: items
                            .iter()
                            .map(|item| WireItem {
                                path: item.path().as_bytes().to_vec(),
                                size: item.size(),
                            })
                            .collect(),
                        more,
                    })
                    .map_err(|error| wire::scan_failure(&error)),
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
                (Some(path), Some(version), Some(options)) if plain_label(&from) => endpoint
                    .keep_copy(&path, version, &from, &options, &mut proceed)
                    .map(placed)
                    .map_err(|error| wire::apply_failure(&error)),
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

    // The asking Mac gives up by dropping the request; the next progress
    // report then fails and the scan stops.
    let gone = Cell::new(false);
    let (mut listed, mut checked) = (0, 0);
    let mut reported = Instant::now();
    let observed = endpoint.observe_reporting(
        policy,
        &filter,
        &previous,
        &mut || {
            if gone.get() {
                ScanControl::Cancel
            } else {
                ScanControl::Continue
            }
        },
        &mut |step| {
            match step {
                ObserveStep::Listed => listed += 1,
                ObserveStep::Checked => checked += 1,
            }
            if reported.elapsed() >= PROGRESS_INTERVAL {
                reported = Instant::now();
                if answer
                    .reply(&PeerReply::Progress { listed, checked })
                    .is_err()
                {
                    gone.set(true);
                }
            }
        },
    );
    let observation = match observed {
        Ok(observation) => observation,
        Err(error) => return failed(answer, wire::scan_failure(&error)),
    };

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
        cache.keep(root, policy, observation.entries().clone());
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
    let source = decoded(path, Some(entry), run).and_then(|(path, entry, run)| {
        let entry = entry.ok_or_else(malformed)?;
        let source = endpoint
            .source_entry(&path, &entry, &run, &mut proceed)
            .map_err(|error| wire::apply_failure(&error))?;
        let reading = reading(endpoint, &source, &run)?;
        Ok((source, reading))
    });
    let (source, reading) = match source {
        Ok(source) => source,
        Err(error) => return failed(answer, error),
    };
    if answer.reply(&reading).is_err() {
        return;
    }
    let mut sent = Ok(());
    if source.metadata().kind() == foldover_fs::NodeKind::File {
        sent = endpoint.read_entry_from(&source, from, &mut proceed, &mut |chunk| {
            answer.send_data(chunk).map_err(std::io::Error::other)
        });
    }
    if answer.end_data().is_err() {
        return;
    }
    match sent {
        Ok(()) => {
            let _ = answer.reply(&PeerReply::Done);
        }
        Err(error) => failed(answer, wire::apply_failure(&error)),
    }
}

/// Place what the asking Mac sends, staged and verified like any copy.
fn receive(
    endpoint: &LocalEndpoint,
    path: &[u8],
    entry: Incoming,
    answer: &mut Answer,
) -> Result<PeerReply, FolderError> {
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
        .receiving(&path, current, &run, &mut proceed)
        .map_err(|error| wire::apply_failure(&error))?;
    let version = match entry.kind {
        IncomingKind::File { content, from } => {
            let content =
                <[u8; Digest::BYTE_LEN]>::try_from(content.as_slice()).map_err(|_| malformed())?;
            let resume = Resume {
                content: Digest::from_bytes(content),
                from,
            };
            endpoint.receive_file(&path, destination.as_ref(), carried, resume, &run, |sink| {
                while let Some(chunk) = answer.next_data().map_err(std::io::Error::other)? {
                    sink(&chunk)?;
                }
                Ok(())
            })
        }
        IncomingKind::Folder if destination.is_some() => {
            Err(crate::ApplyError::Unsupported(path.clone()))
        }
        IncomingKind::Folder => endpoint.receive_directory(&path, carried, &run, &mut proceed),
        IncomingKind::Link { target } => {
            endpoint.receive_link(&path, destination.as_ref(), &target, &run, &mut proceed)
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

/// The other Mac gives up by dropping its request, which ends any data in
/// flight; the operations themselves need no other stop.
fn proceed() -> ScanControl {
    ScanControl::Continue
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

        cache.keep(&root, with_times, entries.clone());
        assert!(cache.take(&root, without).is_empty());
        cache.keep(&root, with_times, entries.clone());
        assert_eq!(cache.take(&root, with_times), entries);
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
}
