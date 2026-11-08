//! Reuse a snapshot's row storage while observing its replacement.
//!
//! A reader owns only the old entry it needs. The caller lands verified
//! results in the original row allocation, with new paths staged separately.
//! Changed originals remain available until alignment, so duplicate listings
//! and a later spelling change still compare with the original snapshot.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use foldover_types::{BaselineEntry, PathMap, PathMapBuilder, Side, SyncPath};

use crate::local::{ObservedEntry, PreviousObservation, ROW_GROWTH_HEADROOM_DIVISOR};
use crate::observation_versions::{ObservationOrigin, ObservationVisibility, VersionPoolBuilder};

/// Borrowed compatibility input, or the storage being recycled by this run.
#[derive(Clone)]
pub(crate) enum Cache<'a> {
    Borrowed(&'a dyn PreviousObservation),
    Owned(Arc<Mutex<Recycled>>),
}

impl Cache<'_> {
    pub(crate) fn key(&self, path: &SyncPath) -> Option<SyncPath> {
        match self {
            Self::Borrowed(previous) => previous.get_key_value(path).map(|(key, _)| key.clone()),
            Self::Owned(rows) => rows
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .key(path),
        }
    }

    pub(crate) fn get(&self, path: &SyncPath) -> Option<Arc<ObservedEntry>> {
        match self {
            Self::Borrowed(previous) => previous.get(path).map(|entry| Arc::new(entry.clone())),
            Self::Owned(rows) => rows
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(path),
        }
    }
}

pub(crate) struct Recycled {
    // The original prefix stays sorted and keeps its keys until finish.
    rows: Vec<(SyncPath, ObservedEntry)>,
    additions: Vec<(SyncPath, ObservedEntry)>,
    originals: BTreeMap<usize, Arc<ObservedEntry>>,
    versions: Option<VersionPoolBuilder>,
    side: Side,
    original_len: usize,
}

/// Enough original state to finish comparison after spelling alignment.
#[derive(Default, Debug)]
pub(crate) struct Comparison {
    pub(crate) original_len: usize,
    pub(crate) changed: BTreeMap<SyncPath, Arc<ObservedEntry>>,
}

impl Recycled {
    pub(crate) fn new(
        previous: PathMap<ObservedEntry>,
        base: Arc<PathMap<BaselineEntry>>,
        side: Side,
    ) -> Self {
        let mut rows = previous.into_rows();
        // Reserve one percent of the decoded prefix as spare row capacity
        // before touching the rows so additions land in the existing
        // allocation when possible. A snapshot already built with headroom
        // needs no change; small maps keep their exact allocation.
        rows.reserve_exact(rows.len() / ROW_GROWTH_HEADROOM_DIVISOR);
        // Origins belong to this comparison, not to the persisted snapshot.
        for (_, entry) in &mut rows {
            entry.set_origin(ObservationOrigin::Uncompared);
            entry.set_visibility(ObservationVisibility::Included);
        }
        let original_len = rows.len();
        Self {
            rows,
            additions: Vec::new(),
            originals: BTreeMap::new(),
            versions: Some(VersionPoolBuilder::new(base)),
            side,
            original_len,
        }
    }

    fn index(&self, path: &SyncPath) -> Option<usize> {
        self.rows.binary_search_by(|(key, _)| key.cmp(path)).ok()
    }

    fn key(&self, path: &SyncPath) -> Option<SyncPath> {
        self.index(path).map(|index| self.rows[index].0.clone())
    }

    fn get(&mut self, path: &SyncPath) -> Option<Arc<ObservedEntry>> {
        let index = self.index(path)?;
        if let Some(original) = self.originals.get(&index) {
            return Some(Arc::clone(original));
        }
        // A prior-equal landed row is the original value. Taking it again
        // handles duplicates arriving after an earlier result has landed.
        let original = Arc::new(self.rows[index].1.take());
        self.originals.insert(index, Arc::clone(&original));
        Some(original)
    }

    pub(crate) fn reuse(&mut self, path: &SyncPath, known: Arc<ObservedEntry>) {
        let index = self.index(path).expect("a cache hit has an original row");
        let original = self
            .originals
            .remove(&index)
            .expect("lookup retains the original");
        // Release this read's reference before trying to move the payload.
        // Only another live duplicate can now require a bounded copy.
        drop(known);
        let mut entry = Arc::try_unwrap(original).unwrap_or_else(|shared| (*shared).clone());
        entry.set_origin(ObservationOrigin::PriorEqual);
        self.rows[index].1 = entry;
    }

    pub(crate) fn record(&mut self, path: SyncPath, mut entry: ObservedEntry) {
        let Some(index) = self.index(&path) else {
            let handle = self
                .versions
                .as_mut()
                .expect("record precedes finish")
                .intern(&path, self.side, entry.version());
            entry.set_version(handle);
            entry.set_origin(ObservationOrigin::New);
            self.additions.push((path, entry));
            return;
        };
        let original = self
            .originals
            .entry(index)
            .or_insert_with(|| Arc::new(self.rows[index].1.take()));
        if original.as_ref() == &entry {
            // Usually only this registry still owns it. A duplicate read
            // can retain another Arc; cloning then is bounded by live jobs.
            let original = self.originals.remove(&index).expect("original exists");
            let mut reused = Arc::try_unwrap(original).unwrap_or_else(|shared| (*shared).clone());
            reused.set_origin(ObservationOrigin::PriorEqual);
            self.rows[index].1 = reused;
        } else {
            let handle = self
                .versions
                .as_mut()
                .expect("record precedes finish")
                .intern(&path, self.side, entry.version());
            entry.set_version(handle);
            entry.set_origin(ObservationOrigin::PriorChanged);
            self.rows[index].1 = entry;
        }
    }

    /// Seal appended versions before exposing any replacement entry.
    /// Unseen originals remain as hidden comparison rows until alignment.
    pub(crate) fn finish(&mut self) -> (PathMap<ObservedEntry>, Comparison) {
        self.versions.take().expect("finish once").freeze();
        let mut changed = BTreeMap::new();
        for (index, original) in std::mem::take(&mut self.originals) {
            if self.rows[index].1.is_taken() {
                // A read failed. Restore the old row only as an unseen
                // comparison value; it must not become a verified result.
                self.rows[index].1 =
                    Arc::try_unwrap(original).unwrap_or_else(|shared| (*shared).clone());
                self.rows[index].1.set_origin(ObservationOrigin::Uncompared);
            } else {
                changed.insert(self.rows[index].0.clone(), original);
            }
        }
        let mut rows = std::mem::take(&mut self.rows);
        if rows.is_empty() {
            // The original prefix is gone; move the additions allocation
            // instead of copying it into the unused empty row buffer.
            rows = std::mem::take(&mut self.additions);
        } else {
            // Reserve exactly the additions so a larger batch does not
            // trigger the default doubling growth.
            rows.reserve_exact(self.additions.len());
            rows.append(&mut self.additions);
        }
        let rows = PathMapBuilder::from_rows(rows).freeze_retaining_capacity();
        (
            rows,
            Comparison {
                original_len: self.original_len,
                changed,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use foldover_types::{Digest, EntryContent, EntryVersion};

    fn path(name: &[u8]) -> SyncPath {
        SyncPath::try_from(name).unwrap()
    }

    fn entry(seed: u8) -> ObservedEntry {
        ObservedEntry::for_tests(
            EntryVersion::new(
                EntryContent::File(Digest::from_bytes([seed; 32])),
                Digest::from_bytes([1; 32]),
            ),
            12,
        )
    }

    fn recycled(old: ObservedEntry) -> Recycled {
        Recycled::new(
            [(path(b"a"), old)].into_iter().collect(),
            Arc::new(PathMap::new()),
            Side::Left,
        )
    }

    #[test]
    fn duplicate_reads_always_compare_with_the_original() {
        let original = entry(1);
        let mut rows = recycled(original.clone());
        let first = rows.get(&path(b"a")).unwrap();
        let second = rows.get(&path(b"a")).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        rows.record(path(b"a"), entry(2));
        assert_eq!(*rows.get(&path(b"a")).unwrap(), original);
        rows.record(path(b"a"), (*second).clone());
        drop(first);
        drop(second);
        let (finished, comparison) = rows.finish();
        assert_eq!(finished[&path(b"a")], original);
        assert_eq!(
            finished[&path(b"a")].origin(),
            ObservationOrigin::PriorEqual
        );
        assert_eq!(comparison.original_len, 1);
    }

    #[test]
    fn duplicate_after_equal_landing_still_uses_the_original() {
        let original = entry(1);
        let mut rows = recycled(original.clone());
        rows.record(path(b"a"), original.clone());
        assert_eq!(*rows.get(&path(b"a")).unwrap(), original);
        rows.record(path(b"a"), entry(2));
        let (finished, comparison) = rows.finish();
        assert_eq!(finished[&path(b"a")].version(), entry(2).version());
        assert_eq!(*comparison.changed[&path(b"a")], original);
    }

    #[test]
    fn unseen_and_failed_rows_never_become_verified() {
        let original = entry(1);
        let mut rows = recycled(original.clone());
        drop(rows.get(&path(b"a")));
        let (finished, _) = rows.finish();
        assert_eq!(finished[&path(b"a")], original);
        assert_eq!(
            finished[&path(b"a")].origin(),
            ObservationOrigin::Uncompared
        );
    }

    #[test]
    fn last_new_duplicate_wins_without_a_prior_entry() {
        let mut rows = Recycled::new(PathMap::new(), Arc::new(PathMap::new()), Side::Left);
        rows.record(path(b"new"), entry(1));
        assert!(rows.get(&path(b"new")).is_none());
        rows.record(path(b"new"), entry(2));
        let (finished, comparison) = rows.finish();
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[&path(b"new")].version(), entry(2).version());
        assert_eq!(comparison.original_len, 0);
    }

    #[test]
    fn empty_original_finish_moves_the_additions_allocation() {
        let mut rows = Recycled::new(PathMap::new(), Arc::new(PathMap::new()), Side::Left);
        for (key, seed) in [(b"a", 1u8), (b"b", 2), (b"c", 3), (b"d", 4)] {
            rows.record(path(key), entry(seed));
        }
        let additions = rows.additions.as_ptr();
        let (finished, comparison) = rows.finish();
        assert_eq!(comparison.original_len, 0);
        let finished_rows = finished.into_rows();
        assert_eq!(finished_rows.len(), 4);
        assert!(
            std::ptr::eq(additions, finished_rows.as_ptr()),
            "an empty original prefix must reuse the additions allocation"
        );
        assert_eq!(finished_rows[3].1.version(), entry(4).version());
    }

    #[test]
    fn finish_keeps_the_original_row_allocation_and_spare_capacity() {
        let mut previous = PathMap::with_capacity(102);
        for index in 0..100u8 {
            let name = format!("k{index:03}");
            previous.insert(path(name.as_bytes()), entry(index));
        }
        assert_eq!(previous.capacity(), 102);
        let original = previous.get_index(0).unwrap().1 as *const ObservedEntry;

        let mut recycled = Recycled::new(previous, Arc::new(PathMap::new()), Side::Left);
        recycled.record(path(b"zz"), entry(200));

        let (finished, comparison) = recycled.finish();
        assert_eq!(comparison.original_len, 100);
        assert_eq!(finished.len(), 101);
        assert_eq!(finished.capacity(), 102);
        assert!(std::ptr::eq(original, &finished[&path(b"k000")]));
    }

    fn observation(previous: PathMap<ObservedEntry>) -> crate::Observation {
        crate::Observation::recycling(previous, Arc::new(PathMap::new()), Side::Left).0
    }

    fn assert_comparison(mut observation: crate::Observation, previous: &PathMap<ObservedEntry>) {
        let expected = !observation.snapshot_entries().eq(previous.iter());
        assert_eq!(observation.finish_snapshot(), Some(expected));
        assert!(
            observation
                .snapshot_entries()
                .eq(observation.entries().snapshot())
        );
    }

    #[test]
    fn final_comparison_keeps_verified_excluded_rows() {
        let previous: PathMap<_> = [(path(b"a"), entry(1))].into_iter().collect();
        let mut observed = observation(previous.clone());
        observed.record(path(b"a"), entry(1));
        observed.finish_entries();
        observed.exclude(&[path(b"a")]);
        assert!(observed.entries().is_empty());
        assert_eq!(observed.snapshot_entries().count(), 1);
        assert_eq!(observed.finish_snapshot(), Some(false));
    }

    #[test]
    fn final_comparison_counts_missing_originals() {
        let previous: PathMap<_> = [(path(b"a"), entry(1))].into_iter().collect();
        let mut observed = observation(previous.clone());
        observed.finish_entries();
        assert!(observed.entries().is_empty());
        assert_eq!(observed.snapshot_entries().count(), 0);
        assert_comparison(observed, &previous);
    }

    #[test]
    fn a_new_row_suppressed_by_a_late_failure_does_not_change_an_empty_cache() {
        let previous = PathMap::new();
        let mut observed = observation(PathMap::new());
        observed.record(path(b"a"), entry(1));
        observed.finish_entries();
        observed.entries.retain(|_, _| false);
        assert_comparison(observed, &previous);
    }

    #[test]
    fn a_prior_row_suppressed_by_a_late_failure_changes_the_cache() {
        let previous: PathMap<_> = [(path(b"a"), entry(1))].into_iter().collect();
        let mut observed = observation(previous.clone());
        observed.record(path(b"a"), entry(1));
        observed.finish_entries();
        observed.entries.retain(|_, _| false);
        assert_comparison(observed, &previous);
    }

    #[test]
    fn respelling_compares_with_the_original_at_the_final_path() {
        for batch in [false, true] {
            let previous: PathMap<_> = [(path(b"q"), entry(1))].into_iter().collect();
            let mut observed = observation(previous.clone());
            observed.record(path(b"p"), entry(1));
            observed.finish_entries();
            assert!(!observed.entries().contains_key(&path(b"q")));
            if batch {
                observed.respell_all(&[(path(b"p"), path(b"q"))].into_iter().collect());
            } else {
                observed.respell(&path(b"p"), path(b"q"));
            }
            assert!(observed.snapshot_entries().eq(previous.iter()));
            assert_eq!(observed.entries().len(), 1);
            assert_comparison(observed, &previous);
        }
    }

    #[test]
    fn respelling_a_prior_entry_to_a_new_key_changes_the_cache() {
        let previous: PathMap<_> = [(path(b"p"), entry(1))].into_iter().collect();
        let mut observed = observation(previous.clone());
        observed.record(path(b"p"), entry(1));
        observed.finish_entries();
        observed.respell(&path(b"p"), path(b"q"));
        assert_comparison(observed, &previous);
    }

    #[test]
    fn batch_respelling_reads_all_original_flags_before_changing_any() {
        let previous: PathMap<_> = [(path(b"b"), entry(1)), (path(b"c"), entry(1))]
            .into_iter()
            .collect();
        let mut observed = observation(previous.clone());
        observed.record(path(b"a"), entry(1));
        observed.record(path(b"b"), entry(1));
        observed.finish_entries();
        observed.respell_all(
            &[(path(b"a"), path(b"b")), (path(b"b"), path(b"a"))]
                .into_iter()
                .collect(),
        );
        assert_comparison(observed, &previous);
    }
}
