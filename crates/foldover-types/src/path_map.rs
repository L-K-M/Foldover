//! Dense ordered map from raw path bytes to values.
//!
//! A one-million-entry observation map built on `BTreeMap<SyncPath, _>`
//! spends more than 500 MB: every node is separately allocated and carries
//! tree pointers on top of the path bytes. [`PathMap`] stores one
//! `(SyncPath, T)` row per entry in a single `Vec`, sorted by raw path
//! bytes with no duplicate keys. Lookup is a binary search over
//! contiguous rows, iteration is a slice walk in raw byte order, and
//! building from unsorted observations is an append-only
//! [`PathMapBuilder`] plus one [`PathMapBuilder::freeze`].
//!
//! Recorded tradeoffs:
//!
//! - [`PathMap::insert`] and [`PathMap::remove`] are `O(n)` row shifts,
//!   right for the rare respelling or exclusion and wrong for bulk loads.
//!   The engine batches those edits through the builder.
//! - `freeze` sorts the builder's rows with an unstable sort keyed on
//!   `(raw key, insertion ordinal)`, then compacts duplicates in place.
//!   The unstable sort avoids `sort_by`'s full-payload merge scratch, but
//!   the standard library does not promise that it allocates nothing and
//!   this crate has not measured it.
//! - Compaction moves the surviving rows into the frozen `Vec`, so peak
//!   memory during `freeze` is the builder's capacity plus the frozen
//!   map's. The frozen map is then shrunk to its length.
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
    pub fn shrink_to_fit(&mut self) {
        self.rows.shrink_to_fit();
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
/// keeping the last insertion, exactly as `BTreeMap::insert` would.
#[derive(Clone, Debug, Default)]
pub struct PathMapBuilder<T> {
    rows: Vec<BuilderRow<T>>,
}

#[derive(Clone, Debug)]
struct BuilderRow<T> {
    key: SyncPath,
    value: T,
    insertion: usize,
}

impl<T> PathMapBuilder<T> {
    /// An empty builder.
    pub fn new() -> Self {
        Self { rows: Vec::new() }
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
        let insertion = self.rows.len();
        self.rows.push(BuilderRow {
            key,
            value,
            insertion,
        });
    }

    /// Sorts once, resolves duplicates by last insertion, and compacts.
    pub fn freeze(mut self) -> PathMap<T> {
        // Total order: raw bytes ascending, then insertion descending, so
        // the first row of each equal-key run is the last one inserted.
        // An unstable sort is deterministic for a total order and avoids
        // the stable sort's full-payload merge scratch.
        self.rows.sort_unstable_by(|left, right| {
            left.key
                .as_bytes()
                .cmp(right.key.as_bytes())
                .then_with(|| right.insertion.cmp(&left.insertion))
        });
        self.rows.dedup_by(|left, right| left.key == right.key);
        let mut rows = Vec::with_capacity(self.rows.len());
        rows.extend(self.rows.into_iter().map(|row| (row.key, row.value)));
        let mut map = PathMap { rows };
        map.shrink_to_fit();
        map
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
    }
}
