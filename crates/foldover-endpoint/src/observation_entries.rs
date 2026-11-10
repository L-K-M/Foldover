//! A verified view and its snapshot cache share one row allocation.

use std::fmt;
use std::ops::Index;

use foldover_types::{PathMap, PathMapIter, SyncPath};

use crate::local::{ObservedEntry, PreviousObservation};
use crate::observation_versions::{ObservationOrigin, ObservationVisibility};

/// Readable, included entries in raw path order. Excluded verified entries
/// remain in the same allocation for the next snapshot, while unseen old
/// rows are hidden from both planning and persistence.
#[derive(Clone)]
pub struct ObservationEntries {
    rows: PathMap<ObservedEntry>,
    visible_len: usize,
}

impl Default for ObservationEntries {
    fn default() -> Self {
        Self::new(PathMap::new())
    }
}

fn verified(entry: &ObservedEntry) -> bool {
    entry.origin() != ObservationOrigin::Uncompared
}

fn visible(entry: &ObservedEntry) -> bool {
    verified(entry) && !entry.is_excluded()
}

impl ObservationEntries {
    pub(crate) fn new(rows: PathMap<ObservedEntry>) -> Self {
        let visible_len = rows.values().filter(|entry| visible(entry)).count();
        Self { rows, visible_len }
    }

    pub fn len(&self) -> usize {
        self.visible_len
    }

    pub fn is_empty(&self) -> bool {
        self.visible_len == 0
    }

    pub fn get(&self, path: &SyncPath) -> Option<&ObservedEntry> {
        self.rows.get(path).filter(|entry| visible(entry))
    }

    pub fn get_key_value(&self, path: &SyncPath) -> Option<(&SyncPath, &ObservedEntry)> {
        self.rows
            .get_key_value(path)
            .filter(|(_, entry)| visible(entry))
    }

    pub fn contains_key(&self, path: &SyncPath) -> bool {
        self.get(path).is_some()
    }

    pub fn iter(&self) -> ObservationIter<'_> {
        ObservationIter {
            inner: self.rows.iter(),
            remaining: self.visible_len,
        }
    }

    pub fn keys(&self) -> impl DoubleEndedIterator<Item = &SyncPath> {
        self.iter().map(|(path, _)| path)
    }

    pub fn values(&self) -> impl DoubleEndedIterator<Item = &ObservedEntry> {
        self.iter().map(|(_, entry)| entry)
    }

    pub(crate) fn snapshot(&self) -> impl Iterator<Item = (&SyncPath, &ObservedEntry)> {
        self.rows.iter().filter(|(_, entry)| verified(entry))
    }

    pub(crate) fn raw(&self) -> &PathMap<ObservedEntry> {
        &self.rows
    }

    pub(crate) fn set_origin(&mut self, path: &SyncPath, origin: ObservationOrigin) {
        if let Some(entry) = self.rows.get_mut(path) {
            self.visible_len -= usize::from(visible(entry));
            entry.set_origin(origin);
            self.visible_len += usize::from(visible(entry));
        }
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&SyncPath, &mut ObservedEntry) -> bool) {
        self.rows.retain(|path, entry| keep(path, entry));
        self.recount();
    }

    pub(crate) fn remove(&mut self, path: &SyncPath) -> Option<ObservedEntry> {
        let entry = self.rows.remove(path)?;
        self.visible_len -= usize::from(visible(&entry));
        Some(entry)
    }

    pub(crate) fn insert(&mut self, path: SyncPath, entry: ObservedEntry) {
        self.visible_len += usize::from(visible(&entry));
        if let Some(old) = self.rows.insert(path, entry) {
            self.visible_len -= usize::from(visible(&old));
        }
    }

    pub(crate) fn exclude(&mut self, paths: &[SyncPath]) {
        for path in paths {
            if let Some(entry) = self.rows.get_mut(path) {
                self.visible_len -= usize::from(visible(entry));
                entry.set_visibility(ObservationVisibility::Excluded);
            }
        }
    }

    pub(crate) fn into_rows(self) -> Vec<(SyncPath, ObservedEntry)> {
        self.rows.into_rows()
    }

    pub(crate) fn into_snapshot(mut self) -> PathMap<ObservedEntry> {
        self.rows.retain(|_, entry| verified(entry));
        self.rows
    }

    fn recount(&mut self) {
        self.visible_len = self.rows.values().filter(|entry| visible(entry)).count();
    }
}

impl PreviousObservation for ObservationEntries {
    fn get_key_value(&self, path: &SyncPath) -> Option<(&SyncPath, &ObservedEntry)> {
        self.get_key_value(path)
    }
}

impl Index<&SyncPath> for ObservationEntries {
    type Output = ObservedEntry;

    fn index(&self, path: &SyncPath) -> &Self::Output {
        self.get(path).expect("no visible observation at this path")
    }
}

impl fmt::Debug for ObservationEntries {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_map().entries(self.iter()).finish()
    }
}

impl PartialEq for ObservationEntries {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}

impl Eq for ObservationEntries {}

/// Included verified entries, with an exact remaining count.
pub struct ObservationIter<'a> {
    inner: PathMapIter<'a, ObservedEntry>,
    remaining: usize,
}

impl<'a> Iterator for ObservationIter<'a> {
    type Item = (&'a SyncPath, &'a ObservedEntry);

    fn next(&mut self) -> Option<Self::Item> {
        let next = self.inner.find(|(_, entry)| visible(entry));
        self.remaining -= usize::from(next.is_some());
        next
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl DoubleEndedIterator for ObservationIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let next = self.inner.rfind(|(_, entry)| visible(entry));
        self.remaining -= usize::from(next.is_some());
        next
    }
}

impl ExactSizeIterator for ObservationIter<'_> {}
impl std::iter::FusedIterator for ObservationIter<'_> {}

impl<'a> IntoIterator for &'a ObservationEntries {
    type Item = (&'a SyncPath, &'a ObservedEntry);
    type IntoIter = ObservationIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use foldover_types::{Digest, EntryContent, EntryVersion};

    fn path(name: &[u8]) -> SyncPath {
        SyncPath::try_from(name).unwrap()
    }

    fn entry(origin: ObservationOrigin) -> ObservedEntry {
        let mut entry = ObservedEntry::for_tests(
            EntryVersion::new(EntryContent::Directory, Digest::from_bytes([1; 32])),
            0,
        );
        entry.set_origin(origin);
        entry
    }

    #[test]
    fn snapshot_keeps_verified_exclusions_but_never_unseen_rows() {
        let mut rows = ObservationEntries::new(
            [
                (path(b"a"), entry(ObservationOrigin::PriorEqual)),
                (path(b"b"), entry(ObservationOrigin::New)),
                (path(b"c"), entry(ObservationOrigin::Uncompared)),
            ]
            .into_iter()
            .collect(),
        );
        assert_eq!(rows.len(), 2);
        rows.exclude(&[path(b"b")]);
        assert_eq!(rows.len(), 1);
        assert!(rows.get(&path(b"b")).is_none());
        assert!(rows.get(&path(b"c")).is_none());
        assert_eq!(
            rows.snapshot().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
            [path(b"a"), path(b"b")]
        );
        assert_eq!(rows.into_snapshot().len(), 2);
    }
}
