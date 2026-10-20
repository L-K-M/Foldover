//! Answering another Mac's operation on a folder this Mac shares with it.
//! The caller has checked that the share is granted to that Mac; this
//! opens the folder with the local endpoint and does what was asked.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, UNIX_EPOCH};

use foldover_proto::local::PairDurability;
use foldover_proto::peer::{
    FolderError, FolderErrorKind, FolderOp, NameCase, NameUnicode, PeerReply, ScanSettings,
};
use foldover_types::{
    CaseMatching, Durability, LocalRoot, MetadataPolicy, ScanControl, SyncPath, UnicodeMatching,
};

use super::wire;
use crate::link::Answer;
use crate::{Filter, LocalEndpoint, ObserveStep, ObservedEntry};

/// How often a scan reports progress, which is also how soon it notices
/// that the asking Mac gave up.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

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

/// Answer one operation on the shared folder at `root`.
pub fn serve_folder(root: &LocalRoot, op: FolderOp, cache: &FolderCache, answer: &mut Answer) {
    let endpoint = match LocalEndpoint::open(Path::new(OsStr::from_bytes(root.as_bytes())))
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
        FolderOp::EnsureIdentity { durability } => {
            let durability = match durability {
                PairDurability::Standard => Durability::Standard,
                PairDurability::Full => Durability::Full,
            };
            endpoint
                .ensure_identity(durability)
                .map(|id| PeerReply::Identity {
                    id: Some(id.to_string()),
                })
                .map_err(|error| wire::apply_failure(&error))
        }
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
        FolderOp::Observe { scan } => return observe(&endpoint, root, scan, cache, answer),
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
    );
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
            })
            .is_ok();
    if sent {
        cache.keep(root, policy, observation.entries().clone());
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
}
