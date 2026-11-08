//! Timestamp-only refresh memory invariants.

use super::Recycled;
use crate::Observation;
use crate::local::ObservedEntry;
use crate::observation_versions::ObservationOrigin;
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
fn planning_exclusions_do_not_change_snapshot_comparison() {
    let name = path(b"file");
    let original = entry(entry_version(7), 100, 123, 4096, b"stat");
    let refreshed = entry(entry_version(7), 200, 456, 4096, b"stat");
    let replaced = entry(entry_version(8), 200, 456, 8192, b"new-stat");
    for (candidate, changed) in [
        (Some(original.clone()), false),
        (Some(refreshed), true),
        (Some(replaced), true),
        (None, true),
    ] {
        let make = || {
            let previous = [(name.clone(), original.clone())].into_iter().collect();
            let (mut observed, _) =
                Observation::recycling(previous, Arc::new(PathMap::new()), Side::Left);
            if let Some(candidate) = &candidate {
                observed.record(name.clone(), candidate.clone());
            }
            observed.finish_entries();
            observed
        };
        let mut early = make();
        let mut late = make();
        assert_eq!(early.finish_snapshot(), Some(changed));
        early.exclude(std::slice::from_ref(&name));
        late.exclude(std::slice::from_ref(&name));
        assert_eq!(late.finish_snapshot(), Some(changed));
        assert!(early.entries().is_empty());
        assert!(late.entries().is_empty());
        assert!(early.snapshot_entries().eq(late.snapshot_entries()));
        assert_eq!(
            early.snapshot_entries().count(),
            usize::from(candidate.is_some())
        );
    }
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
        let candidate = entry(
            version,
            fresh_at.0,
            fresh_at.1,
            index as u64,
            key.as_bytes(),
        );
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
        assert_eq!(
            original.fingerprint_bytes(),
            originals[index].fingerprint_bytes()
        );
        assert_eq!(original.version(), originals[index].version());
        // Land the duplicate read too. A read without a result intentionally
        // retains its original until finish for failure handling.
        drop(original);
        recycled.record(path.clone(), candidates[index].clone());
        assert!(recycled.originals.is_empty());
    }
    let (finished, comparison) = recycled.finish();
    assert_eq!(comparison.original_len, COUNT);
    assert!(comparison.changed.is_empty());
    assert_eq!(finished.len(), COUNT);
    for (index, path) in paths.iter().enumerate() {
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
    for (name, original, fresh) in [
        (b"bb".as_slice(), &original_b, &fresh_b),
        (b"dd".as_slice(), &original_d, &fresh_d),
    ] {
        assert_eq!(*recycled.get(&path(name)).unwrap(), *original);
        recycled.record(path(name), fresh.clone());
    }
    let (finished, comparison) = recycled.finish();
    assert_eq!(comparison.original_len, 2);
    assert!(comparison.changed.is_empty());
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

#[test]
fn returning_from_a_full_change_to_an_original_stat_releases_the_full_original() {
    let original = entry(entry_version(1), 100, 123, 10, b"stat");
    let fresh = entry(entry_version(1), 200, 456, 10, b"stat");
    let previous = [(path(b"a"), original.clone())].into_iter().collect();
    let mut rows = Recycled::new(previous, Arc::new(PathMap::new()), Side::Left);
    rows.record(
        path(b"a"),
        entry(entry_version(2), 200, 456, 20, b"changed"),
    );
    rows.record(path(b"a"), fresh.clone());
    assert!(rows.originals.is_empty());
    assert_eq!(*rows.get(&path(b"a")).unwrap(), original);
    rows.record(path(b"a"), fresh.clone());
    let (finished, comparison) = rows.finish();
    assert!(comparison.changed.is_empty());
    assert_eq!(finished[&path(b"a")], fresh);
}

#[test]
fn equal_count_different_baseline_keys_do_not_supply_timestamp_positions() {
    for batch in [false, true] {
        let original = entry(entry_version(1), 100, 123, 10, b"stat");
        let base_row = BaselineEntry::new(original.version(), original.version()).unwrap();
        let base = [(path(b"a"), base_row), (path(b"c"), base_row)]
            .into_iter()
            .collect();
        let previous: PathMap<_> = [
            (path(b"a"), original.clone()),
            (path(b"d"), original.clone()),
        ]
        .into_iter()
        .collect();
        let (mut observed, _) =
            Observation::recycling(previous.clone(), Arc::new(base), Side::Left);
        observed.record(path(b"a"), original.clone());
        observed.record(path(b"d"), entry(entry_version(1), 200, 456, 10, b"stat"));
        observed.record(path(b"x"), original.clone());
        observed.finish_entries();
        if batch {
            observed.respell_all(&[(path(b"x"), path(b"d"))].into_iter().collect());
        } else {
            observed.respell(&path(b"x"), path(b"d"));
        }
        assert!(observed.snapshot_entries().eq(previous.iter()));
        assert_eq!(observed.finish_snapshot(), Some(false));
    }
}

#[test]
fn moving_a_refreshed_source_preserves_its_original_for_a_later_return() {
    for batch in [false, true] {
        let original = entry(entry_version(1), 100, 123, 10, b"stat");
        let previous: PathMap<_> = [(path(b"p"), original.clone())].into_iter().collect();
        let (mut observed, _) =
            Observation::recycling(previous.clone(), Arc::new(PathMap::new()), Side::Left);
        observed.record(path(b"p"), entry(entry_version(1), 200, 456, 10, b"stat"));
        observed.finish_entries();
        if batch {
            observed.respell_all(&[(path(b"p"), path(b"q"))].into_iter().collect());
        } else {
            observed.respell(&path(b"p"), path(b"q"));
        }
        let mut returned = original.clone();
        returned.set_origin(ObservationOrigin::New);
        observed.entries.insert(path(b"q"), returned);
        observed.respell(&path(b"q"), path(b"p"));
        assert!(observed.snapshot_entries().eq(previous.iter()));
        assert_eq!(observed.finish_snapshot(), Some(false));
    }
}

#[test]
fn overwriting_a_refreshed_destination_keeps_its_original_comparison_value() {
    for batch in [false, true] {
        for final_seed in [3, 2] {
            let a = entry(entry_version(1), 100, 123, 10, b"stat-a");
            let b = entry(entry_version(2), 100, 123, 20, b"stat-b");
            let other = entry(entry_version(3), 100, 123, 30, b"stat-other");
            let previous: PathMap<_> = [(path(b"a"), a.clone()), (path(b"b"), b.clone())]
                .into_iter()
                .collect();
            let (mut observed, _) =
                Observation::recycling(previous.clone(), Arc::new(PathMap::new()), Side::Left);
            observed.record(path(b"a"), a);
            observed.record(path(b"b"), entry(entry_version(2), 200, 456, 20, b"stat-b"));
            observed.record(path(b"x"), other.clone());
            observed.record(path(b"y"), if final_seed == 2 { b.clone() } else { other });
            observed.finish_entries();
            for from in [b"x", b"y"] {
                if batch {
                    observed.respell_all(&[(path(from), path(b"b"))].into_iter().collect());
                } else {
                    observed.respell(&path(from), path(b"b"));
                }
            }
            let changed = !observed.snapshot_entries().eq(previous.iter());
            assert_eq!(changed, final_seed != 2);
            assert_eq!(
                observed.finish_snapshot(),
                Some(changed),
                "batch={batch}, final version={final_seed}"
            );
        }
    }
}

#[test]
fn an_equal_original_can_be_respelled_away_and_back_without_a_cache_change() {
    let original = entry(entry_version(1), 100, 123, 10, b"stat");
    let previous: PathMap<_> = [(path(b"p"), original.clone())].into_iter().collect();
    let (mut observed, _) =
        Observation::recycling(previous.clone(), Arc::new(PathMap::new()), Side::Left);
    observed.record(path(b"p"), original);
    observed.finish_entries();
    observed.respell(&path(b"p"), path(b"q"));
    observed.respell(&path(b"q"), path(b"p"));
    assert!(observed.snapshot_entries().eq(previous.iter()));
    assert_eq!(observed.finish_snapshot(), Some(false));
}
