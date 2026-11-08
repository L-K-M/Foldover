//! Timestamp-only refresh allocation and precision invariants.

use super::{Fingerprint, Observation, ObservedEntry};
use crate::observation_versions::ObservationOrigin;
use foldover_types::{Digest, EntryContent, EntryVersion, PathMap, Side, SyncPath};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

fn path(name: &[u8]) -> SyncPath {
    SyncPath::try_from(name).unwrap()
}

fn entry_version(seed: u8) -> EntryVersion {
    EntryVersion::new(
        EntryContent::File(Digest::from_bytes([seed; 32])),
        Digest::from_bytes([seed.wrapping_add(1); 32]),
    )
}

#[test]
fn time_only_candidate_reuses_the_original_fingerprint_key_allocation() {
    let path = path(b"a");
    let observed_at = UNIX_EPOCH + Duration::new(1_600_000_000, 123_456_789);
    let fresh_at = UNIX_EPOCH + Duration::new(1_700_000_000, 987_654_321);
    let original = ObservedEntry::new(
        entry_version(1),
        Fingerprint::from_key(b"stat-key".to_vec(), 4096, observed_at),
    );
    let key = original.fingerprint.key.as_ptr();
    let previous: PathMap<_> = [(path.clone(), original)].into_iter().collect();
    let (mut observed, cache) =
        Observation::recycling(previous, Arc::new(PathMap::new()), Side::Left);
    let candidate = ObservedEntry::new(
        entry_version(1),
        Fingerprint::from_key(b"stat-key".to_vec(), 4096, fresh_at),
    );
    observed.record(path.clone(), candidate);
    let known = cache
        .get(&path)
        .expect("a time-only refresh keeps the original readable");
    assert_eq!(known.fingerprint.observed_at, observed_at);
    assert_eq!(known.fingerprint.size, 4096);
    assert!(known.fingerprint.key.matches(b"stat-key"));
    observed.finish_entries();
    let landed = &observed.entries()[&path];
    assert_eq!(
        landed.fingerprint.key.as_ptr(),
        key,
        "a time-only candidate must reuse the original fingerprint key allocation"
    );
    assert_eq!(landed.fingerprint.observed_at, fresh_at);
    assert_eq!(landed.fingerprint.size, 4096);
    assert_eq!(landed.origin(), ObservationOrigin::PriorChanged);
    assert_eq!(observed.finish_snapshot(), Some(true));
}

#[test]
fn pre_epoch_and_full_precision_time_only_refresh_preserves_the_original() {
    let path = path(b"b");
    let pre_epoch = UNIX_EPOCH - Duration::new(5, 123_456_789);
    let fresh_at = UNIX_EPOCH + Duration::new(1_700_000_123, 987_654_321);
    let original = ObservedEntry::new(
        entry_version(2),
        Fingerprint::from_key(b"stat-key".to_vec(), 7, pre_epoch),
    );
    let key = original.fingerprint.key.as_ptr();
    let previous: PathMap<_> = [(path.clone(), original)].into_iter().collect();
    let (mut observed, cache) =
        Observation::recycling(previous, Arc::new(PathMap::new()), Side::Left);
    let candidate = ObservedEntry::new(
        entry_version(2),
        Fingerprint::from_key(b"stat-key".to_vec(), 7, fresh_at),
    );
    observed.record(path.clone(), candidate);
    let known = cache
        .get(&path)
        .expect("a pre-epoch original stays readable");
    assert_eq!(known.fingerprint.observed_at, pre_epoch);
    assert!(known.fingerprint.key.matches(b"stat-key"));
    observed.finish_entries();
    let landed = &observed.entries()[&path];
    assert_eq!(landed.fingerprint.key.as_ptr(), key);
    assert_eq!(landed.fingerprint.observed_at, fresh_at);
    assert_eq!(
        landed
            .fingerprint
            .observed_at
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos(),
        987_654_321
    );
    assert_eq!(observed.finish_snapshot(), Some(true));
}
