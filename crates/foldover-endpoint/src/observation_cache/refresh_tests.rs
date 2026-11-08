//! Timestamp-only refresh memory invariants.

use super::Recycled;
use crate::local::ObservedEntry;
use crate::observation_versions::ObservationOrigin;
use crate::Observation;
use foldover_types::{BaselineEntry, Digest, EntryContent, EntryVersion, PathMap, Side, SyncPath};
use std::sync::Arc;

const FINGERPRINT_HEADER_BYTES: usize = 8 + 4 + 8;

fn path(name: &[u8]) -> SyncPath {
    SyncPath::try_from(name).unwrap()
}

fn entry_version(seed: u8) -> EntryVersion {
    EntryVersion::new(
        EntryContent::File(Digest::from_bytes([seed; 32])),
        Digest::from_bytes([seed.wrapping_add(1); 32]),
    )
}

fn fingerprint_bytes(seconds: u64, nanoseconds: u32, size: u64, key: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(FINGERPRINT_HEADER_BYTES + key.len());
    bytes.extend_from_slice(&seconds.to_le_bytes());
    bytes.extend_from_slice(&nanoseconds.to_le_bytes());
    bytes.extend_from_slice(&size.to_le_bytes());
    bytes.extend_from_slice(key);
    bytes
}

fn entry(
    version: EntryVersion,
    seconds: u64,
    nanoseconds: u32,
    size: u64,
    key: &[u8],
) -> ObservedEntry {
    ObservedEntry::restore(version, &fingerprint_bytes(seconds, nanoseconds, size, key)).unwrap()
}

#[test]
fn time_only_refreshes_do_not_retain_full_originals() {
    const COUNT: usize = 1000;
    let old_at = (1_600_000_000u64, 123_456_789u32);
    let fresh_at = (1_700_000_000u64, 987_654_321u32);
    let mut previous = PathMap::new();
    let mut originals = Vec::with_capacity(COUNT);
    let mut candidates = Vec::with_capacity(COUNT);
    for index in 0..COUNT {
        let name = format!("k{index:04}");
        let key = format!("stat-{index:04}");
        let version = entry_version(index as u8);
        let original = entry(version, old_at.0, old_at.1, index as u64, key.as_bytes());
        let candidate = entry(version, fresh_at.0, fresh_at.1, index as u64, key.as_bytes());
        previous.insert(path(name.as_bytes()), original.clone());
        originals.push(original);
        candidates.push(candidate);
    }
    let paths: Vec<SyncPath> = previous.keys().cloned().collect();
    let mut recycled = Recycled::new(previous, Arc::new(PathMap::new()), Side::Left);
    for (index, path) in paths.iter().enumerate() {
        recycled.record(path.clone(), candidates[index].clone());
        assert!(
            recycled.originals.is_empty(),
            "time-only refresh {index} retained a full original Arc"
        );
        let original = recycled.get(path).expect("a refreshed row stays readable");
        assert_eq!(original.fingerprint_bytes(), originals[index].fingerprint_bytes());
        assert_eq!(original.version(), originals[index].version());
    }
    let (finished, comparison) = recycled.finish();
    assert_eq!(comparison.original_len, COUNT);
    assert_eq!(comparison.changed.len(), COUNT);
    assert_eq!(finished.len(), COUNT);
    for (index, path) in paths.iter().enumerate() {
        assert_eq!(
            comparison.changed[path].fingerprint_bytes(),
            originals[index].fingerprint_bytes()
        );
        assert_eq!(comparison.changed[path].version(), originals[index].version());
        assert_eq!(
            finished[path].fingerprint_bytes(),
            candidates[index].fingerprint_bytes()
        );
        assert_eq!(finished[path].version(), candidates[index].version());
    }
}

#[test]
fn time_only_then_exact_original_result_is_unchanged() {
    let version = entry_version(7);
    let original = entry(version, 100, 200, 4096, b"stat-key");
    let fresh = entry(version, 200, 300, 4096, b"stat-key");
    let path = path(b"a");
    let previous: PathMap<_> = [(path.clone(), original.clone())].into_iter().collect();
    let (mut observed, _cache) =
        Observation::recycling(previous.clone(), Arc::new(PathMap::new()), Side::Left);
    observed.record(path.clone(), fresh);
    observed.record(path.clone(), original.clone());
    observed.finish_entries();
    assert!(observed.snapshot_entries().eq(previous.iter()));
    assert_eq!(observed.finish_snapshot(), Some(false));
}

#[test]
fn time_only_then_version_or_stat_change_retains_the_exact_original() {
    let original = entry(entry_version(9), 100, 200, 4096, b"old-key");
    let fresh = entry(entry_version(9), 200, 300, 4096, b"old-key");
    let changed = entry(entry_version(10), 300, 400, 8192, b"new-key");
    let path = path(b"a");
    let previous: PathMap<_> = [(path.clone(), original.clone())].into_iter().collect();
    let mut recycled = Recycled::new(previous, Arc::new(PathMap::new()), Side::Left);
    recycled.record(path.clone(), fresh);
    recycled.record(path.clone(), changed.clone());
    let (finished, comparison) = recycled.finish();
    assert_eq!(comparison.original_len, 1);
    assert_eq!(
        comparison.changed[&path].fingerprint_bytes(),
        original.fingerprint_bytes()
    );
    assert_eq!(comparison.changed[&path].version(), original.version());
    assert_eq!(finished[&path], changed);
}

#[test]
fn full_change_then_time_only_candidate_keeps_the_previous_original() {
    let original = entry(entry_version(21), 100, 200, 10, b"old-key");
    let changed = entry(entry_version(22), 200, 300, 20, b"new-key");
    let refreshed = entry(entry_version(22), 300, 400, 20, b"new-key");
    let path = path(b"a");
    let previous: PathMap<_> = [(path.clone(), original.clone())].into_iter().collect();
    let mut recycled = Recycled::new(previous, Arc::new(PathMap::new()), Side::Left);
    recycled.record(path.clone(), changed);
    assert_eq!(recycled.originals.len(), 1);
    assert_eq!(
        recycled.get(&path).unwrap().fingerprint_bytes(),
        original.fingerprint_bytes()
    );
    recycled.record(path.clone(), refreshed.clone());
    assert_eq!(
        recycled.get(&path).unwrap().fingerprint_bytes(),
        original.fingerprint_bytes()
    );
    let (finished, comparison) = recycled.finish();
    assert_eq!(comparison.original_len, 1);
    assert_eq!(
        comparison.changed[&path].fingerprint_bytes(),
        original.fingerprint_bytes()
    );
    assert_eq!(finished[&path], refreshed);
}

#[test]
fn base_subset_with_equal_length_keys_indexes_time_only_refreshes() {
    let base_paths: [&[u8]; 4] = [b"aa", b"bb", b"cc", b"dd"];
    let base_versions: Vec<EntryVersion> = (0..4u8).map(entry_version).collect();
    let base: PathMap<BaselineEntry> = base_paths
        .iter()
        .zip(&base_versions)
        .map(|(path, version)| {
            (
                SyncPath::try_from(*path).unwrap(),
                BaselineEntry::new(*version, *version).unwrap(),
            )
        })
        .collect();
    let original_b = entry(base_versions[1], 100, 200, 10, b"stat-b");
    let original_d = entry(base_versions[3], 100, 200, 30, b"stat-d");
    let previous: PathMap<_> = [
        (path(b"bb"), original_b.clone()),
        (path(b"dd"), original_d.clone()),
    ]
    .into_iter()
    .collect();
    let mut recycled = Recycled::new(previous, Arc::new(base), Side::Left);
    let fresh_b = entry(base_versions[1], 200, 300, 10, b"stat-b");
    let fresh_d = entry(base_versions[3], 200, 300, 30, b"stat-d");
    let fresh_c = entry(base_versions[2], 200, 300, 20, b"stat-c");
    recycled.record(path(b"bb"), fresh_b.clone());
    recycled.record(path(b"dd"), fresh_d.clone());
    recycled.record(path(b"cc"), fresh_c.clone());
    let (finished, comparison) = recycled.finish();
    assert_eq!(comparison.original_len, 2);
    assert_eq!(comparison.changed.len(), 2);
    assert_eq!(
        comparison.changed[&path(b"bb")].fingerprint_bytes(),
        original_b.fingerprint_bytes()
    );
    assert_eq!(
        comparison.changed[&path(b"dd")].fingerprint_bytes(),
        original_d.fingerprint_bytes()
    );
    assert_eq!(finished[&path(b"bb")], fresh_b);
    assert_eq!(finished[&path(b"dd")], fresh_d);
    assert_eq!(finished[&path(b"cc")], fresh_c);
    assert_eq!(finished[&path(b"cc")].version(), base_versions[2]);
    assert_eq!(finished.len(), 3);
}

#[test]
fn respelling_a_refreshed_row_away_and_back_keeps_the_oracle() {
    let version = entry_version(11);
    let original = entry(version, 100, 200, 4096, b"stat-key");
    let fresh = entry(version, 200, 300, 4096, b"stat-key");
    let from = path(b"p");
    let to = path(b"q");
    let previous: PathMap<_> = [(from.clone(), original)].into_iter().collect();
    let (mut observed, _cache) =
        Observation::recycling(previous.clone(), Arc::new(PathMap::new()), Side::Left);
    observed.record(from.clone(), fresh.clone());
    observed.finish_entries();
    observed.respell(&from, to.clone());
    assert!(observed.entries().contains_key(&to));
    assert!(!observed.entries().contains_key(&from));
    observed.respell(&to, from.clone());
    assert!(observed.entries().contains_key(&from));
    assert!(!observed.entries().contains_key(&to));
    assert_eq!(
        observed.entries()[&from].origin(),
        ObservationOrigin::PriorChanged
    );
    let expected = !observed.snapshot_entries().eq(previous.iter());
    assert!(
        expected,
        "the refreshed timestamp still differs from the previous snapshot"
    );
    assert_eq!(observed.finish_snapshot(), Some(true));
    assert_eq!(observed.entries()[&from], fresh);
}
