//! Canonical observation versions.
//!
//! A measured synthetic 1M-path run decoded a baseline plus two snapshots
//! into 840 MB before any scan, even though the first copy pass used
//! 472 MB. Keeping a separate `EntryVersion` in each snapshot duplicates
//! 65 bytes per side already represented by the shared baseline. This module
//! keeps one immutable baseline row per path and lets each observed
//! path-side resolve either that row or an appended owned version.
//!
//! An [`ObservedVersion`] is 16 bytes on 64-bit targets: an
//! `Arc<VersionPool>` plus a packed `usize` slot. The slot holds a row
//! index in its low bits, a location in the next two bits and a
//! transient comparison origin in the top two. One additional bit marks
//! cache-only rows excluded from planning. These per-handle flags never
//! change the resolved version or the other handles sharing its pool.
//!
//! [`VersionPoolBuilder`] stages appended versions and seals them with
//! [`VersionPoolBuilder::freeze`]. Handles are handed out before the
//! seal and stay valid because each holds its own pool `Arc`.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};

use foldover_types::{BaselineEntry, EntryVersion, PathMap, Side, SyncPath};

use crate::observation_hints::ObservationHints;

/// Slot bits reserved for the location, above the row index.
const LOCATION_SHIFT: u32 = usize::BITS - 4;
/// Slot bits reserved for the comparison origin, above the location.
const ORIGIN_SHIFT: u32 = usize::BITS - 2;
/// Mask for the row index stored in the low slot bits.
const INDEX_MASK: usize = usize::MAX >> 5;
/// One per-row visibility bit below location and origin.
const EXCLUDED_MASK: usize = 1 << (usize::BITS - 5);
/// Mask for the location stored in the second-highest slot bits.
const LOCATION_MASK: usize = 0b11 << LOCATION_SHIFT;
/// Mask for the origin stored in the highest slot bits.
const ORIGIN_MASK: usize = 0b11 << ORIGIN_SHIFT;

/// Where a handle's version lives.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VersionLocation {
    /// The pool's appended versions.
    Owned,
    /// The baseline row's left version.
    BaseLeft,
    /// The baseline row's right version.
    BaseRight,
}

/// Transient comparison bookkeeping stored in a handle's slot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ObservationOrigin {
    /// No prior observation has been compared yet.
    #[default]
    Uncompared,
    /// The prior observation matched.
    PriorEqual,
    /// The prior observation differed.
    PriorChanged,
    /// The path-side has no prior observation.
    New,
}

/// Whether a verified row participates in planning or only the cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ObservationVisibility {
    Included,
    Excluded,
}

/// One immutable version store: baseline rows plus versions appended
/// during observation.
pub(crate) struct VersionPool {
    base: Arc<PathMap<BaselineEntry>>,
    owned: OnceLock<Box<[EntryVersion]>>,
}

/// Stages appended versions and seals them into a [`VersionPool`].
pub(crate) struct VersionPoolBuilder {
    pool: Arc<VersionPool>,
    owned: Vec<EntryVersion>,
    hints: ObservationHints,
}

impl VersionPoolBuilder {
    /// A builder over `base` with no observation hints. A version matching
    /// a baseline row's side points at that row; anything else is appended.
    pub(crate) fn new(base: Arc<PathMap<BaselineEntry>>) -> Self {
        Self::with_hints(base, ObservationHints::default())
    }

    /// A builder over `base` that may reuse `hints`.
    ///
    /// A version matching a baseline row's side points at that row. A
    /// version that a hint has at the same exact raw path points at the
    /// hint's shared pool. Anything else is appended to this builder.
    pub(crate) fn with_hints(base: Arc<PathMap<BaselineEntry>>, hints: ObservationHints) -> Self {
        Self {
            pool: Arc::new(VersionPool {
                base,
                owned: OnceLock::new(),
            }),
            owned: Vec::new(),
            hints,
        }
    }

    /// Interns `version` for `path` and `side`.
    ///
    /// When the exact raw path is a baseline row and that side's stored
    /// version equals `version`, the handle points at the row. Otherwise,
    /// when a hint holds that exact raw path at the same full version, the
    /// hint's shared handle is returned. Otherwise the version is appended
    /// to the builder's owned list and the handle points there.
    pub(crate) fn intern(
        &mut self,
        path: &SyncPath,
        side: Side,
        version: EntryVersion,
    ) -> ObservedVersion {
        if let Some(index) = self.pool.base.index_of(path)
            && let Some((_, baseline)) = self.pool.base.get_index(index)
        {
            let stored = match side {
                Side::Left => baseline.left(),
                Side::Right => baseline.right(),
            };
            if stored == version {
                return ObservedVersion::new(
                    Arc::clone(&self.pool),
                    slot(index, base_location(side)),
                );
            }
        }

        if let Some(hinted) = self.hints.matching_version(path, version) {
            return hinted;
        }

        let index = self.owned.len();
        self.owned.push(version);
        ObservedVersion::new(Arc::clone(&self.pool), slot(index, VersionLocation::Owned))
    }

    /// Seals the pool's appended versions and returns the shared pool.
    ///
    /// Every handle returned earlier stays usable: each holds its own
    /// `Arc<VersionPool>`. Reading an owned handle before this call is an
    /// internal programming error and panics.
    pub(crate) fn freeze(self) -> Arc<VersionPool> {
        self.pool
            .owned
            .set(self.owned.into_boxed_slice())
            .expect("freeze seals a version pool exactly once");
        self.pool
    }

    /// Number of appended versions, for tests that distinguish a baseline
    /// hit from an append.
    #[cfg(test)]
    pub(crate) fn owned_len(&self) -> usize {
        self.owned.len()
    }
}

impl Default for VersionPoolBuilder {
    fn default() -> Self {
        Self::new(Arc::new(PathMap::new()))
    }
}

/// A 16-byte (on 64-bit) reference to one canonical version.
///
/// The reference keeps its pool alive, so a held preview resolves its
/// versions after the run and its inputs are gone.
#[derive(Clone)]
pub struct ObservedVersion {
    pool: Arc<VersionPool>,
    slot: usize,
}

impl ObservedVersion {
    fn new(pool: Arc<VersionPool>, slot: usize) -> Self {
        Self { pool, slot }
    }

    /// A standalone handle for one version, sealed immediately.
    pub(crate) fn single(version: EntryVersion) -> Self {
        let pool = Arc::new(VersionPool {
            base: Arc::new(PathMap::new()),
            owned: OnceLock::from(vec![version].into_boxed_slice()),
        });
        Self::new(pool, slot(0, VersionLocation::Owned))
    }

    /// The resolved version.
    ///
    /// Panics when the handle points at an owned version whose pool has
    /// not been frozen yet.
    pub fn version(&self) -> EntryVersion {
        match self.location() {
            VersionLocation::Owned => self
                .pool
                .owned
                .get()
                .expect("owned version handle read before the pool was frozen")[self.index()],
            VersionLocation::BaseLeft => self
                .pool
                .base
                .get_index(self.index())
                .expect("baseline index from index_of is in bounds")
                .1
                .left(),
            VersionLocation::BaseRight => self
                .pool
                .base
                .get_index(self.index())
                .expect("baseline index from index_of is in bounds")
                .1
                .right(),
        }
    }

    /// Transient comparison origin.
    pub(crate) fn origin(&self) -> ObservationOrigin {
        match (self.slot & ORIGIN_MASK) >> ORIGIN_SHIFT {
            0 => ObservationOrigin::Uncompared,
            1 => ObservationOrigin::PriorEqual,
            2 => ObservationOrigin::PriorChanged,
            _ => ObservationOrigin::New,
        }
    }

    /// Sets the transient comparison origin without touching the
    /// location or index.
    pub(crate) fn set_origin(&mut self, origin: ObservationOrigin) {
        self.slot = (self.slot & !ORIGIN_MASK) | ((origin as usize) << ORIGIN_SHIFT);
    }

    pub(crate) fn visibility(&self) -> ObservationVisibility {
        if self.slot & EXCLUDED_MASK == 0 {
            ObservationVisibility::Included
        } else {
            ObservationVisibility::Excluded
        }
    }

    pub(crate) fn set_visibility(&mut self, visibility: ObservationVisibility) {
        self.slot = match visibility {
            ObservationVisibility::Included => self.slot & !EXCLUDED_MASK,
            ObservationVisibility::Excluded => self.slot | EXCLUDED_MASK,
        };
    }

    fn location(&self) -> VersionLocation {
        match (self.slot & LOCATION_MASK) >> LOCATION_SHIFT {
            0 => VersionLocation::Owned,
            1 => VersionLocation::BaseLeft,
            2 => VersionLocation::BaseRight,
            _ => unreachable!("version slots are constructed with a defined location"),
        }
    }

    fn index(&self) -> usize {
        self.slot & INDEX_MASK
    }
}

impl PartialEq for ObservedVersion {
    fn eq(&self, other: &Self) -> bool {
        self.version() == other.version()
    }
}

impl Eq for ObservedVersion {}

impl Hash for ObservedVersion {
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.version().hash(state);
    }
}

impl fmt::Debug for ObservedVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObservedVersion")
            .field("location", &self.location())
            .field("origin", &self.origin())
            .field("index", &self.index())
            .finish()
    }
}

fn slot(index: usize, location: VersionLocation) -> usize {
    // A Vec can never hold more than isize::MAX / size_of::<EntryVersion>()
    // entries, far below the index mask, so this is an internal invariant
    // rather than input validation.
    assert!(
        index <= INDEX_MASK,
        "version index {index} exceeds the packed slot capacity {}",
        INDEX_MASK,
    );
    (index & INDEX_MASK) | ((location as usize) << LOCATION_SHIFT)
}

fn base_location(side: Side) -> VersionLocation {
    match side {
        Side::Left => VersionLocation::BaseLeft,
        Side::Right => VersionLocation::BaseRight,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use foldover_types::{Digest, EntryContent};
    use std::collections::hash_map::DefaultHasher;

    fn path(bytes: &[u8]) -> SyncPath {
        SyncPath::try_from(bytes).unwrap()
    }

    fn digest(byte: u8) -> Digest {
        Digest::from_bytes([byte; Digest::BYTE_LEN])
    }

    fn file_version(content: u8, metadata: u8) -> EntryVersion {
        EntryVersion::new(EntryContent::File(digest(content)), digest(metadata))
    }

    fn baseline(version: EntryVersion) -> BaselineEntry {
        BaselineEntry::new(version, version).unwrap()
    }

    fn hash_of(value: &impl Hash) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn row_flags_do_not_change_versions_or_other_handles() {
        let version = file_version(1, 2);
        let mut entry = ObservedVersion::single(version);
        let other = entry.clone();
        for origin in [
            ObservationOrigin::Uncompared,
            ObservationOrigin::PriorEqual,
            ObservationOrigin::PriorChanged,
            ObservationOrigin::New,
        ] {
            for visibility in [
                ObservationVisibility::Included,
                ObservationVisibility::Excluded,
            ] {
                entry.set_origin(origin);
                entry.set_visibility(visibility);
                assert_eq!(entry.origin(), origin);
                assert_eq!(entry.visibility(), visibility);
                assert_eq!(entry.version(), version);
                assert_eq!(entry, other);
                assert_eq!(hash_of(&entry), hash_of(&other));
                assert_eq!(other.origin(), ObservationOrigin::Uncompared);
                assert_eq!(other.visibility(), ObservationVisibility::Included);
            }
        }
    }

    #[test]
    fn baseline_sides_resolve_without_owned_versions() {
        let left = file_version(1, 2);
        let right = file_version(1, 3);
        let base = BaselineEntry::new(left, right).unwrap();
        let map = PathMap::from_sorted(vec![(path(b"a"), base)]).unwrap();
        let mut builder = VersionPoolBuilder::new(Arc::new(map));

        let left_handle = builder.intern(&path(b"a"), Side::Left, left);
        let right_handle = builder.intern(&path(b"a"), Side::Right, right);

        assert_eq!(left_handle.location(), VersionLocation::BaseLeft);
        assert_eq!(right_handle.location(), VersionLocation::BaseRight);
        assert_eq!(left_handle.version(), left);
        assert_eq!(right_handle.version(), right);
        assert_ne!(left_handle, right_handle);
        assert_eq!(builder.owned.len(), 0);

        let pool = builder.freeze();
        assert!(pool.owned.get().unwrap().is_empty());
        drop(pool);

        assert_eq!(left_handle.version(), left);
        assert_eq!(right_handle.version(), right);
    }

    #[test]
    fn mismatched_content_or_metadata_uses_an_owned_version() {
        let left = file_version(1, 2);
        let right = file_version(1, 3);
        let base = BaselineEntry::new(left, right).unwrap();
        let map = PathMap::from_sorted(vec![(path(b"a"), base)]).unwrap();
        let mut builder = VersionPoolBuilder::new(Arc::new(map));

        let changed_content = file_version(9, 2);
        let changed_metadata = file_version(1, 8);
        let content_handle = builder.intern(&path(b"a"), Side::Left, changed_content);
        let metadata_handle = builder.intern(&path(b"a"), Side::Left, changed_metadata);

        assert_eq!(content_handle.location(), VersionLocation::Owned);
        assert_eq!(metadata_handle.location(), VersionLocation::Owned);
        assert_eq!(builder.owned.len(), 2);

        let pool = builder.freeze();
        assert_eq!(pool.owned.get().unwrap().len(), 2);
        assert_eq!(content_handle.version(), changed_content);
        assert_eq!(metadata_handle.version(), changed_metadata);
        assert_ne!(content_handle, metadata_handle);
    }

    #[test]
    fn exact_raw_paths_and_non_utf8_names_are_interned_per_row() {
        let raw_paths: [&[u8]; 7] = [b"a", b"a.b", b"a/b", b"a-b", b"a0", b"a\xff", b"\xff/a"];
        let mut rows: Vec<(SyncPath, BaselineEntry)> = raw_paths
            .iter()
            .enumerate()
            .map(|(index, bytes)| {
                let version = file_version(index as u8 + 1, index as u8 + 10);
                (path(bytes), baseline(version))
            })
            .collect();
        rows.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
        let map = PathMap::from_sorted(rows.clone()).unwrap();
        let mut builder = VersionPoolBuilder::new(Arc::new(map));

        let mut handles = Vec::new();
        for (path, base) in &rows {
            let left_handle = builder.intern(path, Side::Left, base.left());
            let right_handle = builder.intern(path, Side::Right, base.right());
            handles.push((path.clone(), *base, left_handle, right_handle));
        }

        assert_eq!(builder.owned.len(), 0);
        for (path, base, left_handle, right_handle) in &handles {
            let index = builder.pool.base.index_of(path).unwrap();
            assert_eq!(left_handle.index(), index);
            assert_eq!(right_handle.index(), index);
            assert_eq!(left_handle.location(), VersionLocation::BaseLeft);
            assert_eq!(right_handle.location(), VersionLocation::BaseRight);
            assert_eq!(left_handle.version(), base.left());
            assert_eq!(right_handle.version(), base.right());
        }

        // A path that is not an exact row is owned, and no row is a
        // prefix substitute for another.
        let absent = builder.intern(&path(b"a/b/c"), Side::Left, file_version(20, 20));
        assert_eq!(absent.location(), VersionLocation::Owned);
        let a_version = rows
            .iter()
            .find(|(path, _)| path.as_bytes() == b"a")
            .unwrap()
            .1
            .left();
        let cross = builder.intern(&path(b"a/b"), Side::Left, a_version);
        assert_eq!(cross.location(), VersionLocation::Owned);
        let non_utf8_version = rows
            .iter()
            .find(|(path, _)| path.as_bytes() == b"a\xff")
            .unwrap()
            .1
            .left();
        let non_utf8 = builder.intern(&path(b"a\xff"), Side::Left, non_utf8_version);
        assert_eq!(non_utf8.location(), VersionLocation::BaseLeft);
    }

    #[test]
    fn handles_clone_and_survive_freeze_and_pool_drop() {
        let mut builder = VersionPoolBuilder::default();
        let version = file_version(4, 5);
        let handle = builder.intern(&path(b"new"), Side::Left, version);
        let clone = handle.clone();

        let pool = builder.freeze();
        assert_eq!(pool.owned.get().unwrap().len(), 1);
        drop(pool);

        assert_eq!(handle.version(), version);
        assert_eq!(clone.version(), version);
        assert_eq!(handle, clone);

        let mut changed = handle.clone();
        changed.set_origin(ObservationOrigin::New);
        assert_eq!(changed.version(), version);
        assert_eq!(changed.origin(), ObservationOrigin::New);
        assert_eq!(handle.origin(), ObservationOrigin::Uncompared);
    }

    #[test]
    fn every_origin_preserves_every_location_and_version() {
        let left = file_version(1, 2);
        let right = file_version(1, 3);
        let base = BaselineEntry::new(left, right).unwrap();
        let map = PathMap::from_sorted(vec![(path(b"a"), base)]).unwrap();
        let mut builder = VersionPoolBuilder::new(Arc::new(map));
        let owned = builder.intern(&path(b"absent"), Side::Left, file_version(4, 5));
        let base_left = builder.intern(&path(b"a"), Side::Left, left);
        let base_right = builder.intern(&path(b"a"), Side::Right, right);
        let pool = builder.freeze();
        drop(pool);

        let origins = [
            ObservationOrigin::Uncompared,
            ObservationOrigin::PriorEqual,
            ObservationOrigin::PriorChanged,
            ObservationOrigin::New,
        ];
        let cases = [
            (owned, VersionLocation::Owned),
            (base_left, VersionLocation::BaseLeft),
            (base_right, VersionLocation::BaseRight),
        ];
        for (handle, location) in &cases {
            let mut handle = handle.clone();
            assert_eq!(handle.origin(), ObservationOrigin::Uncompared);
            let index = handle.index();
            let version = handle.version();
            for origin in origins {
                handle.set_origin(origin);
                assert_eq!(handle.origin(), origin);
                assert_eq!(handle.location(), *location);
                assert_eq!(handle.index(), index);
                assert_eq!(handle.version(), version);
            }
        }
    }

    #[test]
    fn single_handle_is_sealed_and_ready() {
        let version = file_version(7, 8);
        let handle = ObservedVersion::single(version);
        assert_eq!(handle.version(), version);
        assert_eq!(handle.location(), VersionLocation::Owned);
        assert_eq!(handle.index(), 0);
        assert_eq!(handle.origin(), ObservationOrigin::Uncompared);
    }

    #[test]
    #[should_panic(expected = "before the pool was frozen")]
    fn owned_read_before_freeze_panics() {
        let mut builder = VersionPoolBuilder::default();
        let handle = builder.intern(&path(b"new"), Side::Left, file_version(1, 2));
        let _ = handle.version();
    }

    #[test]
    fn equal_versions_compare_equal_across_unrelated_pools() {
        let version = file_version(3, 4);
        let map = PathMap::from_sorted(vec![(path(b"x"), baseline(version))]).unwrap();
        let mut base_builder = VersionPoolBuilder::new(Arc::new(map));
        let base_handle = base_builder.intern(&path(b"x"), Side::Left, version);
        let base_pool = base_builder.freeze();

        let mut owned_builder = VersionPoolBuilder::default();
        let owned_handle = owned_builder.intern(&path(b"x"), Side::Left, version);
        let owned_pool = owned_builder.freeze();

        assert_eq!(base_handle, owned_handle);
        assert_eq!(base_handle.version(), owned_handle.version());
        assert_eq!(hash_of(&base_handle), hash_of(&owned_handle));

        // Origin is transient bookkeeping and never part of equality.
        let mut marked = owned_handle.clone();
        marked.set_origin(ObservationOrigin::PriorChanged);
        assert_eq!(base_handle, marked);
        assert_eq!(base_handle.version(), marked.version());

        drop(base_pool);
        drop(owned_pool);
        assert_eq!(base_handle, owned_handle);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn handle_is_sixteen_bytes_on_64_bit() {
        assert_eq!(size_of::<ObservedVersion>(), 16);
        assert_eq!(size_of::<Arc<VersionPool>>(), size_of::<usize>());
        assert_eq!(size_of::<usize>(), 8);
    }

    #[test]
    fn base_hits_append_no_owned_versions() {
        let rows: Vec<(SyncPath, BaselineEntry)> = (0..256u32)
            .map(|index| {
                let version = file_version(index as u8, index.wrapping_add(1) as u8);
                (
                    path(format!("dir/file{index:04}").as_bytes()),
                    baseline(version),
                )
            })
            .collect();
        let map = PathMap::from_sorted(rows.clone()).unwrap();
        let mut builder = VersionPoolBuilder::new(Arc::new(map));

        let mut handles = Vec::new();
        for (path, base) in &rows {
            handles.push(builder.intern(path, Side::Left, base.left()));
            handles.push(builder.intern(path, Side::Right, base.right()));
        }
        assert_eq!(
            builder.owned.len(),
            0,
            "baseline hits must not append owned versions",
        );

        let pool = builder.freeze();
        assert_eq!(pool.owned.get().unwrap().len(), 0);
        drop(pool);

        for (index, handle) in handles.iter().enumerate() {
            let row = &rows[index / 2];
            let expected = if index % 2 == 0 {
                row.1.left()
            } else {
                row.1.right()
            };
            assert_eq!(handle.version(), expected);
            assert_eq!(handle.index(), index / 2);
            assert_eq!(
                handle.location(),
                if index % 2 == 0 {
                    VersionLocation::BaseLeft
                } else {
                    VersionLocation::BaseRight
                },
            );
        }
    }

    #[test]
    fn freeze_counts_only_owned_versions() {
        let left = file_version(1, 2);
        let right = file_version(1, 3);
        let base = BaselineEntry::new(left, right).unwrap();
        let map = PathMap::from_sorted(vec![(path(b"base"), base)]).unwrap();
        let mut builder = VersionPoolBuilder::new(Arc::new(map));

        let _left = builder.intern(&path(b"base"), Side::Left, left);
        let _right = builder.intern(&path(b"base"), Side::Right, right);
        let _changed = builder.intern(&path(b"base"), Side::Left, file_version(9, 9));
        let _other = builder.intern(&path(b"other"), Side::Right, file_version(8, 8));

        assert_eq!(builder.owned.len(), 2);
        let pool = builder.freeze();
        assert_eq!(pool.owned.get().unwrap().len(), 2);
    }

    #[test]
    fn debug_reports_the_slot_without_resolving() {
        let handle = ObservedVersion::single(file_version(1, 2));
        let rendered = format!("{handle:?}");
        assert!(rendered.contains("ObservedVersion"), "{rendered}");
        assert!(rendered.contains("Owned"), "{rendered}");
        assert!(rendered.contains("Uncompared"), "{rendered}");
    }
}
