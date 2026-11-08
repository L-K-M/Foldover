//! Dense ordered map from raw path bytes to values.
//!
//! A measured 1M-entry observation run backed by `BTreeMap<SyncPath, _>`
//! exceeded 500 MB total. Every node is separately allocated and carries
//! tree pointers on top of the path bytes, so the container's own share is
//! estimated to be large, but only the whole-run figure is measured.
//! [`PathMap`] stores one `(SyncPath, T)` row per entry in a single `Vec`,
//! sorted by raw path bytes with no duplicate keys. Lookup is a binary
//! search over contiguous rows, iteration is a slice walk in raw byte
//! order, and building from unsorted observations is an append-only
//! [`PathMapBuilder`] plus one [`PathMapBuilder::freeze`].
//!
//! Recorded tradeoffs:
//!
//! - [`PathMap::insert`] and [`PathMap::remove`] are `O(n)` row shifts,
//!   right for the rare respelling or exclusion and wrong for bulk loads.
//!   The engine batches those edits through the builder.
//! - `freeze` checks for strictly increasing raw keys first. That case
//!   moves the builder's `Vec` into the map with no second full-payload
//!   allocation. Otherwise it allocates one `usize` (8 bytes on 64-bit
//!   targets) per row as a permutation of old indices, sorts those
//!   indices with an unstable sort keyed on `(raw key ascending, old
//!   insertion index descending)`, applies the permutation to the payload
//!   `Vec` in place with cycle swaps, then deduplicates in place. The same
//!   payload `Vec` is returned. Peak extra memory is the permutation plus
//!   any scratch the unstable sort allocates; the standard library does
//!   not promise that it allocates nothing.
//! - [`PathMap::shrink_to_fit`] may allocate transiently: `Vec`'s
//!   allocator shrink is not guaranteed to be in place, so a shrink can
//!   briefly hold the old and new buffers at once.
//!
//! The container is vocabulary only: no I/O, no interner, no global state
//! and no `unsafe`.
//!
//! ```
//! use foldover_types::{PathMapBuilder, SyncPath};
//!
//! let mut builder = PathMapBuilder::new();
//! for (name, size) in [("b", 2u64), ("a", 1), ("b", 3)] {
//!     builder.push(SyncPath::try_from(name.as_bytes()).unwrap(), size);
//! }
//! let map = builder.freeze();
//! // The last insertion for a duplicate key wins, as in `BTreeMap`.
//! assert_eq!(map.get(b"b".as_slice()), Some(&3));
//! assert_eq!(
//!     map.keys().map(|path| path.as_bytes()).collect::<Vec<_>>(),
//!     [b"a".as_slice(), b"b".as_slice()],
//! );
//! ```

use std::borrow::Borrow;
use std::cmp::Ordering;
use std::fmt;
use std::ops::Index;

use crate::SyncPath;

/// A dense map from exact path bytes to values, sorted by raw byte order.
///
/// Rows are unique and ordered, so [`iter`](Self::iter), [`keys`] and
/// [`values`] walk the `Vec` directly and [`get`](Self::get) binary-searches
/// it. Lookups accept borrowed raw bytes (`&[u8]`) as well as `&SyncPath`.
///
/// Build unsorted observations with [`PathMapBuilder`]; construct an
/// already-sorted table with [`from_sorted`](Self::from_sorted).
///
/// [`keys`]: Self::keys
/// [`values`]: Self::values
#[derive(Clone, Default, Eq, PartialEq)]
pub struct PathMap<T> {
    rows: Vec<(SyncPath, T)>,
}

impl<T> fmt::Debug for PathMap<T>
where
    T: fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_map().entries(self.iter()).finish()
    }
}

impl<T> PathMap<T> {
    /// An empty map.
    pub fn new() -> Self {
        Self { rows: Vec::new() }
    }

    /// An empty map with room for `capacity` entries.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            rows: Vec::with_capacity(capacity),
        }
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the map has no entries.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Allocated row capacity. Capacity can exceed [`len`](Self::len)
    /// after rare edits; call [`shrink_to_fit`](Self::shrink_to_fit) to
    /// reclaim it.
    pub fn capacity(&self) -> usize {
        self.rows.capacity()
    }

    /// Reclaim unused capacity after rare edits.
    ///
    /// The allocator shrink is not guaranteed to be in place and may
    /// allocate transiently.
    pub fn shrink_to_fit(&mut self) {
        self.rows.shrink_to_fit();
    }

    /// Consumes the map and returns its rows in ascending raw byte order.
    ///
    /// The returned `Vec` keeps the map's allocation, so a batch of edits
    /// can go through [`PathMapBuilder`] without copying the payload.
    pub fn into_rows(self) -> Vec<(SyncPath, T)> {
        self.rows
    }

    /// Looks a path up by borrowed raw bytes or `&SyncPath`.
    pub fn get<Q>(&self, key: &Q) -> Option<&T>
    where
        Q: ?Sized + Ord,
        SyncPath: Borrow<Q>,
    {
        self.rows
            .binary_search_by(|row| row.0.borrow().cmp(key))
            .ok()
            .map(|index| &self.rows[index].1)
    }

    /// Mutable access by borrowed raw bytes or `&SyncPath`.
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut T>
    where
        Q: ?Sized + Ord,
        SyncPath: Borrow<Q>,
    {
        let index = self
            .rows
            .binary_search_by(|row| row.0.borrow().cmp(key))
            .ok()?;
        Some(&mut self.rows[index].1)
    }

    /// The stored key and value for a borrowed lookup.
    pub fn get_key_value<Q>(&self, key: &Q) -> Option<(&SyncPath, &T)>
    where
        Q: ?Sized + Ord,
        SyncPath: Borrow<Q>,
    {
        let index = self
            .rows
            .binary_search_by(|row| row.0.borrow().cmp(key))
            .ok()?;
        self.rows.get(index).map(|(key, value)| (key, value))
    }

    /// Position of a path in raw byte order, by borrowed lookup.
    ///
    /// This is the row index [`get_index`](Self::get_index) accepts, so a
    /// caller can hold a stable position instead of searching again.
    pub fn index_of<Q>(&self, key: &Q) -> Option<usize>
    where
        Q: ?Sized + Ord,
        SyncPath: Borrow<Q>,
    {
        self.rows
            .binary_search_by(|row| row.0.borrow().cmp(key))
            .ok()
    }

    /// The stored key and value at a row position, in `O(1)`.
    ///
    /// `index` comes from [`index_of`](Self::index_of) or a walk of
    /// [`keys`](Self::keys); a position at or past [`len`](Self::len)
    /// is `None`.
    pub fn get_index(&self, index: usize) -> Option<(&SyncPath, &T)> {
        self.rows.get(index).map(|(key, value)| (key, value))
    }

    /// Whether a path is present, by borrowed raw bytes or `&SyncPath`.
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        Q: ?Sized + Ord,
        SyncPath: Borrow<Q>,
    {
        self.rows
            .binary_search_by(|row| row.0.borrow().cmp(key))
            .is_ok()
    }

    /// Inserts one entry, replacing any existing value and returning it.
    ///
    /// `O(log n)` search plus an `O(n)` row shift when the key is new;
    /// replacing an existing value is `O(log n)`. Rare edits only — bulk
    /// loads belong in [`PathMapBuilder`].
    pub fn insert(&mut self, key: SyncPath, value: T) -> Option<T> {
        match self
            .rows
            .binary_search_by(|row| row.0.as_bytes().cmp(key.as_bytes()))
        {
            Ok(index) => Some(std::mem::replace(&mut self.rows[index].1, value)),
            Err(index) => {
                self.rows.insert(index, (key, value));
                None
            }
        }
    }

    /// Removes one entry, returning its value if present.
    ///
    /// `O(log n)` search plus an `O(n)` row shift. Rare edits only.
    pub fn remove<Q>(&mut self, key: &Q) -> Option<T>
    where
        Q: ?Sized + Ord,
        SyncPath: Borrow<Q>,
    {
        let index = self
            .rows
            .binary_search_by(|row| row.0.borrow().cmp(key))
            .ok()?;
        Some(self.rows.remove(index).1)
    }

    /// Drops entries for which `keep` returns false, preserving raw byte
    /// order. `O(n)`.
    pub fn retain<F>(&mut self, mut keep: F)
    where
        F: FnMut(&SyncPath, &mut T) -> bool,
    {
        self.rows.retain_mut(|row| keep(&row.0, &mut row.1));
    }

    /// Entries in ascending raw byte order.
    pub fn iter(&self) -> PathMapIter<'_, T> {
        PathMapIter {
            inner: self.rows.iter(),
        }
    }

    /// Keys in ascending raw byte order.
    pub fn keys(&self) -> impl DoubleEndedIterator<Item = &SyncPath> + ExactSizeIterator + '_ {
        self.rows.iter().map(|(key, _)| key)
    }

    /// Values in ascending raw byte order of their keys.
    pub fn values(&self) -> impl DoubleEndedIterator<Item = &T> + ExactSizeIterator + '_ {
        self.rows.iter().map(|(_, value)| value)
    }

    /// Builds a map from rows already sorted by raw byte order.
    ///
    /// Rejects a duplicate key or a key that sorts before its predecessor
    /// with [`InvalidSortedPathMap`], rather than silently reordering or
    /// dropping rows.
    pub fn from_sorted(entries: Vec<(SyncPath, T)>) -> Result<Self, InvalidSortedPathMap> {
        for pair in entries.windows(2) {
            match pair[0].0.as_bytes().cmp(pair[1].0.as_bytes()) {
                Ordering::Equal => {
                    return Err(InvalidSortedPathMap::DuplicateKey(pair[1].0.clone()));
                }
                Ordering::Greater => {
                    return Err(InvalidSortedPathMap::UnorderedKey(pair[1].0.clone()));
                }
                Ordering::Less => {}
            }
        }
        Ok(Self { rows: entries })
    }
}

impl<T> FromIterator<(SyncPath, T)> for PathMap<T> {
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = (SyncPath, T)>,
    {
        iter.into_iter().collect::<PathMapBuilder<T>>().freeze()
    }
}

impl<T> IntoIterator for PathMap<T> {
    type Item = (SyncPath, T);
    type IntoIter = std::vec::IntoIter<(SyncPath, T)>;

    fn into_iter(self) -> Self::IntoIter {
        self.rows.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a PathMap<T> {
    type Item = (&'a SyncPath, &'a T);
    type IntoIter = PathMapIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<T, Q> Index<&Q> for PathMap<T>
where
    Q: ?Sized + Ord,
    SyncPath: Borrow<Q>,
{
    type Output = T;

    fn index(&self, key: &Q) -> &T {
        self.get(key).expect("no entry found for key")
    }
}

/// Borrowed iteration over `(SyncPath, T)` pairs in raw byte order.
pub struct PathMapIter<'a, T> {
    inner: std::slice::Iter<'a, (SyncPath, T)>,
}

impl<'a, T> Iterator for PathMapIter<'a, T> {
    type Item = (&'a SyncPath, &'a T);

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|entry| (&entry.0, &entry.1))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<T> DoubleEndedIterator for PathMapIter<'_, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|entry| (&entry.0, &entry.1))
    }
}

impl<T> ExactSizeIterator for PathMapIter<'_, T> {}

impl<T> std::iter::FusedIterator for PathMapIter<'_, T> {}

/// Append-only staging for a [`PathMap`], for observations that arrive
/// unsorted.
///
/// [`push`](Self::push) is `O(1)` amortized and keeps every observation;
/// [`freeze`](Self::freeze) sorts once and resolves duplicate keys by
/// keeping the last insertion, exactly as `BTreeMap::insert` would. The
/// builder backs directly onto the map's `Vec<(SyncPath, T)>` row store,
/// so freezing an already-sorted unique batch moves that allocation
/// instead of copying the payload.
#[derive(Clone, Debug, Default)]
pub struct PathMapBuilder<T> {
    rows: Vec<(SyncPath, T)>,
}

impl<T> PathMapBuilder<T> {
    /// An empty builder.
    pub fn new() -> Self {
        Self { rows: Vec::new() }
    }

    /// Takes an existing row allocation, including unordered or duplicate
    /// keys. Freezing applies the usual last-insertion rule in place.
    pub fn from_rows(rows: Vec<(SyncPath, T)>) -> Self {
        Self { rows }
    }

    /// An empty builder with room for `capacity` observations.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            rows: Vec::with_capacity(capacity),
        }
    }

    /// Number of appended observations, including duplicates.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether nothing has been appended.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Allocated observation capacity.
    pub fn capacity(&self) -> usize {
        self.rows.capacity()
    }

    /// Appends one observation. `O(1)` amortized.
    pub fn push(&mut self, key: SyncPath, value: T) {
        self.rows.push((key, value));
    }

    /// Sorts once, resolves duplicates by last insertion, and compacts.
    ///
    /// Strictly increasing raw keys are already unique and sorted, so the
    /// builder's `Vec` is moved into the map unchanged. Otherwise one
    /// `usize` permutation per row is allocated, the payload `Vec` is
    /// permuted in place with cycle swaps, and duplicate rows are dropped
    /// in place. The same payload `Vec` is returned; `shrink_to_fit` may
    /// allocate transiently.
    pub fn freeze(mut self) -> PathMap<T> {
        if self
            .rows
            .windows(2)
            .all(|pair| pair[0].0.as_bytes() < pair[1].0.as_bytes())
        {
            let mut map = PathMap { rows: self.rows };
            map.shrink_to_fit();
            return map;
        }

        // Permutation p[new] = old. Sorting indices by raw key ascending,
        // then old insertion index descending, puts the last insertion
        // first in each equal-key run. An unstable sort is deterministic
        // for this total order and swaps only `usize` indices.
        let mut permutation: Vec<usize> = (0..self.rows.len()).collect();
        permutation.sort_unstable_by(|&left, &right| {
            self.rows[left]
                .0
                .as_bytes()
                .cmp(self.rows[right].0.as_bytes())
                .then_with(|| right.cmp(&left))
        });
        apply_permutation(&mut self.rows, &mut permutation);
        self.rows.dedup_by(|left, right| left.0 == right.0);
        let mut map = PathMap { rows: self.rows };
        map.shrink_to_fit();
        map
    }
}

/// Applies `permutation` to `rows` in place, where `permutation[new]` is
/// the old index of the row that belongs at `new`. Each cycle is followed
/// with swaps; consumed entries are marked `usize::MAX` so the next cycle
/// starts fresh.
fn apply_permutation<T>(rows: &mut [T], permutation: &mut [usize]) {
    for start in 0..permutation.len() {
        if permutation[start] == usize::MAX {
            continue;
        }
        let mut current = start;
        while permutation[current] != start {
            let next = permutation[current];
            rows.swap(current, next);
            permutation[current] = usize::MAX;
            current = next;
        }
        permutation[current] = usize::MAX;
    }
}

impl<T> Extend<(SyncPath, T)> for PathMapBuilder<T> {
    fn extend<I>(&mut self, iter: I)
    where
        I: IntoIterator<Item = (SyncPath, T)>,
    {
        for (key, value) in iter {
            self.push(key, value);
        }
    }
}

impl<T> FromIterator<(SyncPath, T)> for PathMapBuilder<T> {
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = (SyncPath, T)>,
    {
        let iter = iter.into_iter();
        let mut builder = Self::with_capacity(iter.size_hint().0);
        builder.extend(iter);
        builder
    }
}

/// Why [`PathMap::from_sorted`] rejected a row sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvalidSortedPathMap {
    /// The same raw path bytes appear twice.
    DuplicateKey(SyncPath),
    /// A path sorts before its predecessor.
    UnorderedKey(SyncPath),
}

impl fmt::Display for InvalidSortedPathMap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateKey(key) => {
                write!(formatter, "duplicate path {key:?} in sorted rows")
            }
            Self::UnorderedKey(key) => {
                write!(formatter, "path {key:?} is out of raw byte order")
            }
        }
    }
}

impl std::error::Error for InvalidSortedPathMap {}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn path(bytes: &[u8]) -> SyncPath {
        SyncPath::try_from(bytes).unwrap()
    }

    fn component() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(
            any::<u8>().prop_filter("name byte", |byte| !matches!(*byte, 0 | b'/')),
            1..10,
        )
        .prop_filter("not dot traversal", |name| name != b"." && name != b"..")
    }

    fn valid_path() -> impl Strategy<Value = SyncPath> {
        prop::collection::vec(component(), 1..5).prop_map(|parts| path(&parts.join(&b'/')))
    }

    /// Paths that share byte prefixes across component boundaries: `a`,
    /// `a/b`, `a.b`, `a-b`, `a0` and a non-UTF8 name.
    fn boundary_path() -> impl Strategy<Value = SyncPath> {
        prop_oneof![
            Just(path(b"a")),
            Just(path(b"a/b")),
            Just(path(b"a/b/c")),
            Just(path(b"a.b")),
            Just(path(b"a-b")),
            Just(path(b"a0")),
            Just(path(b"a\xff")),
            Just(path(b"\xff/a")),
            valid_path(),
        ]
    }

    #[derive(Clone, Debug)]
    enum Op {
        Insert(SyncPath, u32),
        Remove(SyncPath),
    }

    fn op_strategy() -> impl Strategy<Value = Op> {
        prop_oneof![
            (valid_path(), any::<u32>()).prop_map(|(key, value)| Op::Insert(key, value)),
            valid_path().prop_map(Op::Remove),
        ]
    }

    #[test]
    fn empty_map_is_empty_and_default() {
        let map = PathMap::<u32>::default();
        assert!(map.is_empty());
        assert_eq!(map.len(), 0);
        assert_eq!(map.iter().len(), 0);
        assert_eq!(map.keys().count(), 0);
        assert_eq!(map.values().count(), 0);
        assert_eq!(map.get(b"a".as_slice()), None);
        assert_eq!(map.get(&path(b"a")), None);
        assert!(!map.contains_key(b"a".as_slice()));
        assert_eq!(map.into_iter().next(), None);
    }

    #[test]
    fn borrowed_byte_lookups_find_stored_paths() {
        let mut map = PathMap::new();
        assert_eq!(map.insert(path(b"one/\xff/two"), 1u32), None);
        assert_eq!(map.insert(path(b"one"), 2), None);
        assert_eq!(map.get(b"one".as_slice()), Some(&2));
        assert_eq!(map.get(&path(b"one")), Some(&2));
        assert_eq!(map.get(b"one/\xff/two".as_slice()), Some(&1));
        assert_eq!(map.get(b"one/\xff".as_slice()), None);
        assert!(map.contains_key(b"one/\xff/two".as_slice()));
        assert!(!map.contains_key(b"one/\xff".as_slice()));

        let (key, value) = map.get_key_value(b"one/\xff/two".as_slice()).unwrap();
        assert_eq!(key, &path(b"one/\xff/two"));
        assert_eq!(*value, 1);

        *map.get_mut(b"one".as_slice()).unwrap() = 20;
        assert_eq!(map.get(b"one".as_slice()), Some(&20));
        assert_eq!(map.get_mut(b"absent".as_slice()), None);
    }

    #[test]
    fn index_of_matches_btree_position_and_get_index_returns_the_row() {
        let entries = vec![
            (path(b"a"), 1u32),
            (path(b"a.b"), 2),
            (path(b"a/b"), 3),
            (path(b"a\xff"), 4),
            (path(b"b"), 5),
        ];
        let map: PathMap<u32> = entries.iter().cloned().collect();
        let oracle: BTreeMap<SyncPath, u32> = entries.into_iter().collect();

        for (index, key) in oracle.keys().enumerate() {
            assert_eq!(map.index_of(key.as_bytes()), Some(index), "{key:?}");
            assert_eq!(map.index_of(key), Some(index), "{key:?}");
            let (stored_key, value) = map.get_index(index).unwrap();
            assert_eq!(stored_key, key);
            assert_eq!(*value, oracle[key]);
        }

        assert_eq!(map.index_of(b"absent".as_slice()), None);
        assert_eq!(map.index_of(b"a/bb".as_slice()), None);
        assert_eq!(map.index_of(b"a/".as_slice()), None);
        assert_eq!(map.get_index(oracle.len()), None);
        assert_eq!(map.get_index(usize::MAX), None);
    }

    #[test]
    fn index_accessors_work_for_non_clone_values() {
        #[derive(Debug, PartialEq)]
        struct Row {
            value: u32,
        }

        let mut map = PathMap::new();
        assert_eq!(map.insert(path(b"one"), Row { value: 1 }), None);
        assert_eq!(map.insert(path(b"two"), Row { value: 2 }), None);
        assert_eq!(map.index_of(b"one".as_slice()), Some(0));
        assert_eq!(map.index_of(b"two".as_slice()), Some(1));
        assert_eq!(map.index_of(b"three".as_slice()), None);

        let (key, row) = map.get_index(0).unwrap();
        assert_eq!(key, &path(b"one"));
        assert_eq!(row.value, 1);
        let (key, row) = map.get_index(1).unwrap();
        assert_eq!(key, &path(b"two"));
        assert_eq!(row.value, 2);
        assert_eq!(map.get_index(2), None);
    }

    #[test]
    fn insert_replaces_and_remove_returns_the_old_value() {
        let mut map = PathMap::new();
        assert_eq!(map.insert(path(b"a"), 1), None);
        assert_eq!(map.insert(path(b"a"), 2), Some(1));
        assert_eq!(map.len(), 1);
        assert_eq!(map.remove(b"a".as_slice()), Some(2));
        assert_eq!(map.remove(b"a".as_slice()), None);
        assert!(map.is_empty());

        // Insertion lands in raw byte order.
        assert_eq!(map.insert(path(b"c"), 3), None);
        assert_eq!(map.insert(path(b"a"), 1), None);
        assert_eq!(map.insert(path(b"b"), 2), None);
        assert_eq!(
            map.keys().map(|key| key.as_bytes()).collect::<Vec<_>>(),
            [b"a".as_slice(), b"b".as_slice(), b"c".as_slice()],
        );
    }

    #[test]
    fn from_iter_keeps_the_last_insertion_for_duplicates() {
        let entries = vec![
            (path(b"b"), 1u32),
            (path(b"a"), 2),
            (path(b"b"), 3),
            (path(b"a"), 4),
            (path(b"b"), 5),
        ];
        let map: PathMap<u32> = entries.iter().cloned().collect();
        let oracle: BTreeMap<SyncPath, u32> = entries.into_iter().collect();
        assert_eq!(map.get(b"a".as_slice()), Some(&4));
        assert_eq!(map.get(b"b".as_slice()), Some(&5));
        assert_eq!(
            map.iter().collect::<Vec<_>>(),
            oracle.iter().collect::<Vec<_>>(),
        );
    }

    #[test]
    fn builder_freeze_keeps_the_last_insertion_for_duplicates() {
        let mut builder = PathMapBuilder::new();
        let mut oracle = BTreeMap::new();
        for (key, value) in [
            (path(b"b"), 1u32),
            (path(b"a"), 2),
            (path(b"b"), 3),
            (path(b"a"), 4),
            (path(b"b"), 5),
        ] {
            builder.push(key.clone(), value);
            oracle.insert(key, value);
        }
        let map = builder.freeze();
        assert_eq!(
            map.iter().collect::<Vec<_>>(),
            oracle.iter().collect::<Vec<_>>(),
        );
    }

    #[test]
    fn builder_extend_collects_unsorted_observations() {
        let mut builder = PathMapBuilder::with_capacity(3);
        builder.extend(vec![(path(b"c"), 3u32), (path(b"a"), 1)]);
        builder.push(path(b"b"), 2);
        assert_eq!(builder.len(), 3);
        let map = builder.freeze();
        assert_eq!(
            map.keys().map(|key| key.as_bytes()).collect::<Vec<_>>(),
            [b"a".as_slice(), b"b".as_slice(), b"c".as_slice()],
        );
    }

    #[test]
    fn builder_from_iter_collects_unsorted_rows() {
        let builder: PathMapBuilder<u32> = vec![(path(b"b"), 2u32), (path(b"a"), 1)]
            .into_iter()
            .collect();
        let map = builder.freeze();
        assert_eq!(
            map.keys().map(|key| key.as_bytes()).collect::<Vec<_>>(),
            [b"a".as_slice(), b"b".as_slice()],
        );
    }

    #[test]
    fn freeze_compacts_duplicate_rows() {
        let mut builder = PathMapBuilder::new();
        for index in 0..10u32 {
            builder.push(path(b"same"), index);
        }
        builder.push(path(b"other"), 99);
        let map = builder.freeze();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get(b"same".as_slice()), Some(&9));
        assert_eq!(map.get(b"other".as_slice()), Some(&99));
    }

    struct DropCounter<'a> {
        tag: u32,
        drops: &'a AtomicUsize,
    }

    impl<'a> DropCounter<'a> {
        fn new(tag: u32, drops: &'a AtomicUsize) -> Self {
            Self { tag, drops }
        }
    }

    impl Drop for DropCounter<'_> {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn duplicate_prone_path() -> impl Strategy<Value = SyncPath> {
        prop_oneof![
            Just(path(b"a")),
            Just(path(b"b")),
            Just(path(b"c")),
            Just(path(b"a/b")),
            Just(path(b"a/b/c")),
            Just(path(b"a.b")),
        ]
    }

    #[test]
    fn builder_freeze_sorted_unique_moves_the_same_buffer() {
        let entries = vec![
            (path(b"a"), 1u32),
            (path(b"a-b"), 2),
            (path(b"a.b"), 3),
            (path(b"a/b"), 4),
            (path(b"a/b/c"), 5),
            (path(b"a0"), 6),
            (path(b"a\xff"), 7),
        ];
        let mut builder = PathMapBuilder::with_capacity(entries.len());
        for (key, value) in entries {
            builder.push(key, value);
        }
        assert_eq!(builder.capacity(), builder.len());
        let before = builder.rows.as_ptr();
        let map = builder.freeze();
        assert!(std::ptr::eq(before, map.rows.as_ptr()));
        assert_eq!(map.len(), 7);
        assert_eq!(map.capacity(), map.len());
    }

    #[test]
    fn builder_freeze_descending_input_sorts_and_keeps_last_insertion() {
        let entries = vec![
            (path(b"c"), 1u32),
            (path(b"b"), 2),
            (path(b"b"), 3),
            (path(b"a"), 4),
            (path(b"a"), 5),
            (path(b"a"), 6),
        ];
        let mut builder = PathMapBuilder::new();
        for (key, value) in entries {
            builder.push(key, value);
        }
        let map = builder.freeze();
        assert_eq!(map.len(), 3);
        assert_eq!(map.get(b"a".as_slice()), Some(&6));
        assert_eq!(map.get(b"b".as_slice()), Some(&3));
        assert_eq!(map.get(b"c".as_slice()), Some(&1));
        assert_eq!(
            map.keys().map(|key| key.as_bytes()).collect::<Vec<_>>(),
            [b"a".as_slice(), b"b".as_slice(), b"c".as_slice()],
        );
    }

    #[test]
    fn builder_freeze_unsorted_unique_moves_the_same_payload_buffer() {
        let entries = vec![(path(b"c"), 3u32), (path(b"a"), 1), (path(b"b"), 2)];
        let mut builder = PathMapBuilder::with_capacity(entries.len());
        for (key, value) in entries {
            builder.push(key, value);
        }
        assert_eq!(builder.capacity(), builder.len());
        let before = builder.rows.as_ptr();
        let map = builder.freeze();
        assert!(std::ptr::eq(before, map.rows.as_ptr()));
        assert_eq!(map.len(), 3);
        assert_eq!(map.capacity(), map.len());
    }

    #[test]
    fn builder_accepts_an_existing_row_allocation() {
        let rows = vec![(path(b"b"), 2), (path(b"a"), 1), (path(b"b"), 3)];
        let before = rows.as_ptr();
        let builder = PathMapBuilder::from_rows(rows);
        assert!(std::ptr::eq(before, builder.rows.as_ptr()));
        let map = builder.freeze();
        assert_eq!(map.len(), 2);
        assert_eq!(map[&path(b"a")], 1);
        assert_eq!(map[&path(b"b")], 3);
    }

    #[test]
    fn apply_permutation_follows_cycles() {
        let cases: [(Vec<u32>, Vec<usize>, Vec<u32>); 10] = [
            (vec![0, 1, 2], vec![0, 1, 2], vec![0, 1, 2]),
            (vec![0, 1, 2], vec![1, 0, 2], vec![1, 0, 2]),
            (vec![0, 1, 2], vec![0, 2, 1], vec![0, 2, 1]),
            (vec![0, 1, 2], vec![1, 2, 0], vec![1, 2, 0]),
            (vec![0, 1, 2], vec![2, 0, 1], vec![2, 0, 1]),
            (vec![0, 1, 2, 3], vec![1, 0, 3, 2], vec![1, 0, 3, 2]),
            (vec![0, 1, 2, 3], vec![2, 1, 0, 3], vec![2, 1, 0, 3]),
            (vec![0, 1, 2, 3], vec![3, 2, 1, 0], vec![3, 2, 1, 0]),
            (vec![0, 1, 2, 3], vec![1, 2, 3, 0], vec![1, 2, 3, 0]),
            (
                vec![0, 1, 2, 3, 4],
                vec![4, 0, 1, 2, 3],
                vec![4, 0, 1, 2, 3],
            ),
        ];
        for (mut rows, mut permutation, expected) in cases {
            apply_permutation(&mut rows, &mut permutation);
            assert_eq!(rows, expected);
            assert!(permutation.iter().all(|&entry| entry == usize::MAX));
        }
    }

    #[test]
    fn into_rows_returns_owned_rows_for_rebuilding() {
        let entries = vec![(path(b"b"), 2u32), (path(b"a"), 1)];
        let map: PathMap<u32> = entries.into_iter().collect();
        let rows = map.into_rows();
        assert_eq!(rows.len(), 2);
        let mut builder = PathMapBuilder::new();
        builder.extend(rows);
        let rebuilt = builder.freeze();
        assert_eq!(rebuilt.get(b"a".as_slice()), Some(&1));
        assert_eq!(rebuilt.get(b"b".as_slice()), Some(&2));
    }

    #[test]
    fn from_sorted_accepts_strictly_ascending_unique_keys() {
        let map = PathMap::from_sorted(vec![
            (path(b"a"), 1u32),
            (path(b"a-b"), 2),
            (path(b"a.b"), 3),
            (path(b"a/b"), 4),
            (path(b"a/b/c"), 5),
            (path(b"a0"), 6),
            (path(b"a\xff"), 7),
        ])
        .unwrap();
        assert_eq!(map.len(), 7);
        assert_eq!(map.get(b"a/b".as_slice()), Some(&4));
        let keys: Vec<&[u8]> = map.keys().map(|key| key.as_bytes()).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn from_sorted_rejects_duplicates_and_unordered_rows() {
        let duplicate = PathMap::from_sorted(vec![(path(b"a"), 1u32), (path(b"a"), 2)]);
        assert!(matches!(
            duplicate,
            Err(InvalidSortedPathMap::DuplicateKey(_))
        ));
        let unordered = PathMap::from_sorted(vec![(path(b"b"), 1u32), (path(b"a"), 2)]);
        assert!(matches!(
            unordered,
            Err(InvalidSortedPathMap::UnorderedKey(_))
        ));
        // A key that repeats after a larger key is out of order.
        let gap = PathMap::from_sorted(vec![(path(b"a"), 1u32), (path(b"b"), 2), (path(b"a"), 3)]);
        assert!(matches!(gap, Err(InvalidSortedPathMap::UnorderedKey(_))));
        assert!(PathMap::<u32>::from_sorted(vec![]).unwrap().is_empty());
        assert!(PathMap::from_sorted(vec![(path(b"a"), 1u32)]).is_ok());
    }

    #[test]
    fn iteration_is_ascending_double_ended_and_exact_size() {
        let entries = vec![
            (path(b"b"), 2u32),
            (path(b"a/c"), 3),
            (path(b"a"), 1),
            (path(b"a.b"), 4),
            (path(b"a/b"), 5),
        ];
        let map: PathMap<u32> = entries.into_iter().collect();
        let keys: Vec<&[u8]> = map.keys().map(|key| key.as_bytes()).collect();
        assert_eq!(
            keys,
            [
                b"a".as_slice(),
                b"a.b".as_slice(),
                b"a/b".as_slice(),
                b"a/c".as_slice(),
                b"b".as_slice(),
            ],
        );
        let reversed: Vec<&[u8]> = map.keys().rev().map(|key| key.as_bytes()).collect();
        let mut expected_reversed = keys.clone();
        expected_reversed.reverse();
        assert_eq!(reversed, expected_reversed);

        let mut iter = map.iter();
        assert_eq!(iter.len(), 5);
        let first = iter.next().unwrap();
        assert_eq!(first.0, &path(b"a"));
        assert_eq!(*first.1, 1);
        assert_eq!(iter.len(), 4);
        let last = iter.next_back().unwrap();
        assert_eq!(last.0, &path(b"b"));
        assert_eq!(iter.len(), 3);

        assert_eq!(map.values().len(), 5);
        assert_eq!(map.values().next_back(), Some(&2));
        assert_eq!(map.iter().rev().count(), 5);
        assert_eq!(map.into_iter().count(), 5);
    }

    #[test]
    fn into_iter_yields_ascending_pairs() {
        let entries = vec![(path(b"b"), 2u32), (path(b"a"), 1), (path(b"c"), 3)];
        let map: PathMap<u32> = entries.into_iter().collect();
        let pairs: Vec<(Vec<u8>, u32)> = map
            .into_iter()
            .map(|(key, value)| (key.as_bytes().to_vec(), value))
            .collect();
        assert_eq!(
            pairs,
            vec![(b"a".to_vec(), 1), (b"b".to_vec(), 2), (b"c".to_vec(), 3)],
        );
    }

    #[test]
    fn index_uses_borrowed_bytes_and_panics_when_absent() {
        let mut map = PathMap::new();
        assert_eq!(map.insert(path(b"a/b"), 9u32), None);
        assert_eq!(map[&path(b"a/b")], 9);
        assert_eq!(map[b"a/b".as_slice()], 9);
        assert_eq!(map.insert(path(b"a/b"), 10), Some(9));
        assert_eq!(map[&path(b"a/b")], 10);
    }

    #[test]
    #[should_panic(expected = "no entry found for key")]
    fn index_panics_for_a_missing_key() {
        let map = PathMap::<u32>::new();
        let _ = map[&path(b"absent")];
    }

    #[test]
    fn clone_is_equal_and_independent() {
        let mut original = PathMap::new();
        assert_eq!(original.insert(path(b"a"), vec![1u32]), None);
        assert_eq!(original.insert(path(b"b"), vec![2]), None);
        let mut clone = original.clone();
        assert_eq!(original, clone);

        assert_eq!(clone.insert(path(b"c"), vec![3]), None);
        assert_eq!(clone.remove(b"a".as_slice()), Some(vec![1]));
        assert_eq!(original.len(), 2);
        assert!(original.contains_key(b"a".as_slice()));
        assert_eq!(clone.len(), 2);
        assert!(!clone.contains_key(b"a".as_slice()));
        assert!(clone.contains_key(b"c".as_slice()));

        assert_eq!(original.insert(path(b"d"), vec![4]), None);
        assert!(!clone.contains_key(b"d".as_slice()));
    }

    #[test]
    fn debug_prints_entries_as_a_map() {
        let mut map = PathMap::new();
        assert_eq!(map.insert(path(b"a"), 1u32), None);
        assert_eq!(map.insert(path(b"b"), 2), None);
        let rendered = format!("{map:?}");
        assert!(rendered.contains("\"a\": 1"), "{rendered}");
        assert!(rendered.contains("\"b\": 2"), "{rendered}");
    }

    #[test]
    fn utf8_and_invalid_byte_paths_round_trip_in_order() {
        let entries = vec![
            (path("caf\u{e9}/notes".as_bytes()), 1u32),
            (path(b"cafe\xcc\x81/notes"), 2),
            (path(b"\xff/\xfe"), 3),
            (path(b"plain"), 4),
            (path(b"caf\xc3\xa9/autre"), 5),
        ];
        let map: PathMap<u32> = entries.iter().cloned().collect();
        let oracle: BTreeMap<SyncPath, u32> = entries.into_iter().collect();
        assert_eq!(
            map.iter().collect::<Vec<_>>(),
            oracle.iter().collect::<Vec<_>>(),
        );
        // The two spellings of "café" are distinct raw byte paths.
        assert_ne!(path("caf\u{e9}".as_bytes()), path(b"cafe\xcc\x81"));
        assert_eq!(map.len(), 5);
    }

    proptest! {
        #[test]
        fn builder_freeze_matches_btree_map(
            entries in prop::collection::vec((valid_path(), any::<u32>()), 0..200),
        ) {
            let mut builder = PathMapBuilder::new();
            let mut oracle = BTreeMap::new();
            for (key, value) in entries {
                builder.push(key.clone(), value);
                oracle.insert(key, value);
            }
            let map = builder.freeze();
            prop_assert_eq!(map.len(), oracle.len());
            prop_assert_eq!(
                map.iter().collect::<Vec<_>>(),
                oracle.iter().collect::<Vec<_>>(),
            );
            for (key, value) in &oracle {
                prop_assert_eq!(map.get(key.as_bytes()), Some(value));
                prop_assert!(map.contains_key(key.as_bytes()));
            }
        }

        #[test]
        fn from_iter_matches_btree_map(
            entries in prop::collection::vec((valid_path(), any::<u32>()), 0..200),
        ) {
            let map: PathMap<u32> = entries.iter().cloned().collect();
            let oracle: BTreeMap<SyncPath, u32> = entries.into_iter().collect();
            prop_assert_eq!(
                map.iter().collect::<Vec<_>>(),
                oracle.iter().collect::<Vec<_>>(),
            );
        }

        #[test]
        fn interleaved_component_paths_match_btree_map(
            entries in prop::collection::vec((boundary_path(), any::<u32>()), 0..100),
        ) {
            let map: PathMap<u32> = entries.iter().cloned().collect();
            let oracle: BTreeMap<SyncPath, u32> = entries.into_iter().collect();
            prop_assert_eq!(
                map.iter().collect::<Vec<_>>(),
                oracle.iter().collect::<Vec<_>>(),
            );
            let keys: Vec<&[u8]> = map.keys().map(|key| key.as_bytes()).collect();
            let mut sorted = keys.clone();
            sorted.sort();
            prop_assert_eq!(keys, sorted);
        }

        #[test]
        fn mutation_matches_btree_map(ops in prop::collection::vec(op_strategy(), 0..200)) {
            let mut map = PathMap::new();
            let mut oracle = BTreeMap::new();
            for op in ops {
                match op {
                    Op::Insert(key, value) => {
                        prop_assert_eq!(map.insert(key.clone(), value), oracle.insert(key, value));
                    }
                    Op::Remove(key) => {
                        prop_assert_eq!(map.remove(&key), oracle.remove(&key));
                    }
                }
                prop_assert_eq!(map.len(), oracle.len());
                prop_assert_eq!(
                    map.iter().collect::<Vec<_>>(),
                    oracle.iter().collect::<Vec<_>>(),
                );
            }
        }

        #[test]
        fn retain_matches_btree_map(
            entries in prop::collection::vec((valid_path(), any::<u32>()), 0..100),
            threshold in any::<u32>(),
        ) {
            let mut map = PathMap::new();
            let mut oracle = BTreeMap::new();
            for (key, value) in entries {
                map.insert(key.clone(), value);
                oracle.insert(key, value);
            }
            map.retain(|_, value| *value > threshold);
            oracle.retain(|_, value| *value > threshold);
            prop_assert_eq!(
                map.iter().collect::<Vec<_>>(),
                oracle.iter().collect::<Vec<_>>(),
            );
        }

        #[test]
        fn from_sorted_accepts_sorted_unique_rows(
            mut entries in prop::collection::vec((valid_path(), any::<u32>()), 0..100),
        ) {
            entries.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
            entries.dedup_by(|left, right| left.0 == right.0);
            let map = PathMap::from_sorted(entries.clone()).unwrap();
            let oracle: BTreeMap<SyncPath, u32> = entries.into_iter().collect();
            prop_assert_eq!(
                map.iter().collect::<Vec<_>>(),
                oracle.iter().collect::<Vec<_>>(),
            );
        }

        #[test]
        fn freeze_arbitrary_permutation_matches_btree_map_with_drop_counts(
            entries in prop::collection::vec((valid_path(), any::<u32>()), 0..200).prop_shuffle(),
        ) {
            let mut builder = PathMapBuilder::new();
            let mut oracle = BTreeMap::new();
            let drops = AtomicUsize::new(0);
            for (key, tag) in &entries {
                builder.push(key.clone(), DropCounter::new(*tag, &drops));
                oracle.insert(key.clone(), *tag);
            }
            let map = builder.freeze();
            prop_assert_eq!(map.len(), oracle.len());
            for (key, tag) in &oracle {
                prop_assert_eq!(map.get(key.as_bytes()).unwrap().tag, *tag);
            }
            let keys: Vec<&[u8]> = map.keys().map(|key| key.as_bytes()).collect();
            let mut sorted = keys.clone();
            sorted.sort();
            prop_assert_eq!(keys, sorted);
            drop(map);
            prop_assert_eq!(drops.load(Ordering::SeqCst), entries.len());
        }

        #[test]
        fn freeze_repeated_duplicate_keys_drop_each_value_once(
            entries in prop::collection::vec((duplicate_prone_path(), any::<u32>()), 0..200)
                .prop_shuffle(),
        ) {
            let mut builder = PathMapBuilder::new();
            let mut oracle = BTreeMap::new();
            let drops = AtomicUsize::new(0);
            for (key, tag) in &entries {
                builder.push(key.clone(), DropCounter::new(*tag, &drops));
                oracle.insert(key.clone(), *tag);
            }
            let map = builder.freeze();
            prop_assert_eq!(map.len(), oracle.len());
            for (key, tag) in &oracle {
                prop_assert_eq!(map.get(key.as_bytes()).unwrap().tag, *tag);
            }
            drop(map);
            prop_assert_eq!(drops.load(Ordering::SeqCst), entries.len());
        }

        #[test]
        fn freeze_sorted_unique_bypass_moves_the_same_buffer(
            mut entries in prop::collection::vec((valid_path(), any::<u32>()), 0..100),
        ) {
            entries.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
            entries.dedup_by(|left, right| left.0 == right.0);
            let len = entries.len();
            let mut builder = PathMapBuilder::with_capacity(len);
            for (key, value) in entries {
                builder.push(key, value);
            }
            prop_assert_eq!(builder.capacity(), builder.len());
            let before = builder.rows.as_ptr();
            let map = builder.freeze();
            prop_assert!(std::ptr::eq(before, map.rows.as_ptr()));
            prop_assert_eq!(map.len(), len);
            prop_assert_eq!(map.capacity(), map.len());
            let keys: Vec<&[u8]> = map.keys().map(|key| key.as_bytes()).collect();
            let mut sorted = keys.clone();
            sorted.sort();
            prop_assert_eq!(keys, sorted);
        }

        #[test]
        fn index_of_matches_btree_oracle(
            entries in prop::collection::vec((valid_path(), any::<u32>()), 0..200),
        ) {
            let map: PathMap<u32> = entries.iter().cloned().collect();
            let oracle: BTreeMap<SyncPath, u32> = entries.into_iter().collect();

            for (index, key) in oracle.keys().enumerate() {
                prop_assert_eq!(map.index_of(key.as_bytes()), Some(index));
                prop_assert_eq!(map.index_of(key), Some(index));
                let (stored_key, value) = map.get_index(index).unwrap();
                prop_assert_eq!(stored_key, key);
                prop_assert_eq!(*value, oracle[key]);
            }
            prop_assert_eq!(map.get_index(oracle.len()), None);
        }
    }
}
