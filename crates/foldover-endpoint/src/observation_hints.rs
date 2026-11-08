//! Cross-side observation version sharing.
//!
//! A completed observation's verified versions can seed another side's
//! version pool through [`ObservationHints`]. A hint shares the source's
//! exact path bytes and sealed version handle, so a version that is already
//! known costs no second copy.
//!
//! Hints are version identity only. They carry no fingerprint and can never
//! justify skipping an observation, hash or stat: the caller must still
//! produce a verified version and fingerprint before it asks whether a hint
//! matches. A matching hint is a sharing decision, not a short circuit
//! around reading or hashing.

use std::sync::Arc;

use foldover_types::{BaselineEntry, EntryVersion, PathMap, PathMapBuilder, Side, SyncPath};

use crate::local::Observation;
use crate::observation_versions::{ObservationOrigin, ObservationVisibility, ObservedVersion};

/// Verified versions from one completed observation, keyed by exact raw
/// path for another side's observation.
///
/// Cloning shares the backing map. The map stores only paths and sealed
/// version handles: no fingerprint bytes are copied or retained.
#[derive(Clone)]
pub struct ObservationHints {
    rows: Arc<PathMap<ObservedVersion>>,
}

impl Default for ObservationHints {
    fn default() -> Self {
        Self {
            rows: Arc::new(PathMap::new()),
        }
    }
}

impl ObservationHints {
    /// Collect a completed observation's verified versions for `side`.
    ///
    /// [`snapshot_entries`](Observation::snapshot_entries) includes rows
    /// excluded from planning, because a version stays verified even when
    /// its row is not planned. An entry whose version already equals the
    /// target baseline row's version at the same exact raw path is omitted:
    /// the next run resolves it from the baseline and needs no hint. A
    /// missing baseline row, or a differing version, is retained.
    ///
    /// The source observation must be completed, so every stored handle is
    /// already sealed and resolves after the source is dropped.
    pub fn from_observation(
        observation: &Observation,
        base: &PathMap<BaselineEntry>,
        side: Side,
    ) -> Self {
        let mut rows = PathMapBuilder::with_capacity(observation.snapshot_entries().count());
        for (path, entry) in observation.snapshot_entries() {
            if let Some(baseline) = base.get(path.as_bytes()) {
                let stored = match side {
                    Side::Left => baseline.left(),
                    Side::Right => baseline.right(),
                };
                if stored == entry.version() {
                    continue;
                }
            }
            rows.push(path.clone(), entry.observed_version());
        }
        Self {
            rows: Arc::new(rows.freeze_retaining_capacity()),
        }
    }

    /// The exact stored key for `path`, sharing its allocation.
    ///
    /// Returns `None` when the hint map has no row at that exact raw path.
    /// The cache wiring that shares path keys is the next step; keep the
    /// crate-private API available now.
    #[allow(dead_code)]
    pub(crate) fn key(&self, path: &SyncPath) -> Option<SyncPath> {
        self.rows
            .get_key_value(path.as_bytes())
            .map(|(key, _)| key.clone())
    }

    /// The hinted version at `path` when it equals `version`.
    ///
    /// The returned handle is independent of the stored one: it starts
    /// with origin [`Uncompared`] and visibility [`Included`], ready for a
    /// fresh comparison. The stored hint keeps the source's flags.
    pub(crate) fn matching_version(
        &self,
        path: &SyncPath,
        version: EntryVersion,
    ) -> Option<ObservedVersion> {
        let (_, hinted) = self.rows.get_key_value(path.as_bytes())?;
        if hinted.version() != version {
            return None;
        }
        let mut handle = hinted.clone();
        handle.set_origin(ObservationOrigin::Uncompared);
        handle.set_visibility(ObservationVisibility::Included);
        Some(handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::{Observation, ObservedEntry};
    use crate::observation_versions::{
        ObservationOrigin, ObservationVisibility, VersionPoolBuilder,
    };
    use foldover_types::{Digest, EntryContent};
    use std::sync::Arc;

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

    fn completed(entries: Vec<(SyncPath, EntryVersion)>) -> Observation {
        let mut observation = Observation::default();
        for (path, version) in entries {
            observation.record(path, ObservedEntry::for_tests(version, 0));
        }
        observation.finish_entries();
        observation
    }

    #[test]
    fn baseline_equality_omits_a_hint() {
        let left = file_version(1, 2);
        let right = file_version(1, 3);
        let base = BaselineEntry::new(left, right).unwrap();
        let base_map = PathMap::from_sorted(vec![
            (path(b"a"), base),
            (path(b"b"), baseline(file_version(9, 9))),
        ])
        .unwrap();
        let observation = completed(vec![
            (path(b"a"), left),
            (path(b"b"), file_version(8, 8)),
            (path(b"c"), file_version(7, 7)),
        ]);

        let hints = ObservationHints::from_observation(&observation, &base_map, Side::Left);
        assert!(hints.key(&path(b"a")).is_none());
        assert!(hints.matching_version(&path(b"a"), left).is_none());
        assert_eq!(hints.key(&path(b"b")).unwrap().as_bytes(), b"b".as_slice());
        assert_eq!(hints.key(&path(b"c")).unwrap().as_bytes(), b"c".as_slice());

        // The right side's baseline version is a different target.
        let right_hints = ObservationHints::from_observation(&observation, &base_map, Side::Right);
        assert!(right_hints.key(&path(b"a")).is_some());
        assert!(right_hints.matching_version(&path(b"a"), right).is_none());
        assert!(right_hints.matching_version(&path(b"a"), left).is_some());
    }

    #[test]
    fn exact_raw_spellings_are_separate_hints() {
        let spellings: [(&[u8], EntryVersion); 5] = [
            (&b"a"[..], file_version(1, 1)),
            (&b"a/b"[..], file_version(2, 2)),
            (&b"a.b"[..], file_version(3, 3)),
            (&b"A"[..], file_version(4, 4)),
            (&b"a\xff"[..], file_version(5, 5)),
        ];
        let observation = completed(
            spellings
                .iter()
                .map(|(bytes, version)| (path(bytes), *version))
                .collect(),
        );
        let hints = ObservationHints::from_observation(&observation, &PathMap::new(), Side::Left);

        for (bytes, version) in &spellings {
            assert_eq!(hints.key(&path(bytes)).unwrap().as_bytes(), *bytes);
            assert_eq!(
                hints
                    .matching_version(&path(bytes), *version)
                    .unwrap()
                    .version(),
                *version,
            );
        }
        for absent in [
            &b"a/b/c"[..],
            &b"a/bc"[..],
            &b"ab"[..],
            &b"a-b"[..],
            &b"a\xfe"[..],
            &b"\xff/a"[..],
        ] {
            assert!(hints.key(&path(absent)).is_none(), "{absent:?}");
            assert!(
                hints
                    .matching_version(&path(absent), file_version(1, 1))
                    .is_none()
            );
        }
        // A version from one spelling never answers for another.
        assert!(
            hints
                .matching_version(&path(b"a"), file_version(2, 2))
                .is_none()
        );
        assert!(
            hints
                .matching_version(&path(b"a/b"), file_version(1, 1))
                .is_none()
        );
    }

    #[test]
    fn changed_content_or_metadata_falls_back_to_an_owned_version() {
        let base_version = file_version(1, 2);
        let base_map = PathMap::from_sorted(vec![(path(b"a"), baseline(base_version))]).unwrap();
        let changed_content = file_version(9, 2);
        let changed_metadata = file_version(1, 8);
        let observation = completed(vec![
            (path(b"a"), changed_content),
            (path(b"b"), changed_metadata),
        ]);
        let hints = ObservationHints::from_observation(&observation, &base_map, Side::Left);

        assert!(hints.matching_version(&path(b"a"), base_version).is_none());
        assert!(
            hints
                .matching_version(&path(b"a"), changed_content)
                .is_some()
        );
        assert!(
            hints
                .matching_version(&path(b"b"), changed_metadata)
                .is_some()
        );
        assert!(hints.matching_version(&path(b"b"), base_version).is_none());

        let mut builder = VersionPoolBuilder::with_hints(Arc::new(base_map), hints);
        let reused = builder.intern(&path(b"a"), Side::Left, changed_content);
        assert_eq!(reused.version(), changed_content);
        assert_eq!(builder.owned_len(), 0, "a matching hint must not append");

        let fallback = builder.intern(&path(b"a"), Side::Left, file_version(7, 7));
        assert_eq!(builder.owned_len(), 1, "a non-matching version must append");

        let pool = builder.freeze();
        assert_eq!(reused.version(), changed_content);
        assert_eq!(fallback.version(), file_version(7, 7));
        drop(pool);
        assert_eq!(reused.version(), changed_content);
        assert_eq!(fallback.version(), file_version(7, 7));
    }

    #[test]
    fn empty_defaults_answer_nothing_and_append_everything() {
        let hints = ObservationHints::default();
        assert!(hints.key(&path(b"a")).is_none());
        assert!(
            hints
                .matching_version(&path(b"a"), file_version(1, 2))
                .is_none()
        );

        let mut builder = VersionPoolBuilder::new(Arc::new(PathMap::new()));
        let handle = builder.intern(&path(b"a"), Side::Left, file_version(1, 2));
        assert_eq!(builder.owned_len(), 1);
        let pool = builder.freeze();
        assert_eq!(handle.version(), file_version(1, 2));
        drop(pool);
        assert_eq!(handle.version(), file_version(1, 2));
    }

    #[test]
    fn hints_outlive_the_source_observation() {
        let version = file_version(1, 2);
        let hints = {
            let observation = completed(vec![(path(b"a"), version)]);
            ObservationHints::from_observation(&observation, &PathMap::new(), Side::Left)
        };

        let handle = hints.matching_version(&path(b"a"), version).unwrap();
        assert_eq!(handle.version(), version);

        let mut builder = VersionPoolBuilder::with_hints(Arc::new(PathMap::new()), hints);
        let interned = builder.intern(&path(b"a"), Side::Left, version);
        assert_eq!(builder.owned_len(), 0);
        let pool = builder.freeze();
        assert_eq!(interned.version(), version);
        drop(pool);
        assert_eq!(interned.version(), version);
    }

    #[test]
    fn returned_handles_reset_origin_and_visibility() {
        let version = file_version(1, 2);
        let mut observation = completed(vec![(path(b"a"), version)]);
        observation
            .entries
            .set_origin(&path(b"a"), ObservationOrigin::PriorEqual);
        observation.entries.exclude(&[path(b"a")]);

        let source = observation
            .snapshot_entries()
            .next()
            .unwrap()
            .1
            .observed_version();
        assert_eq!(source.origin(), ObservationOrigin::PriorEqual);
        assert_eq!(source.visibility(), ObservationVisibility::Excluded);

        let hints = ObservationHints::from_observation(&observation, &PathMap::new(), Side::Left);
        let returned = hints.matching_version(&path(b"a"), version).unwrap();
        assert_eq!(returned.origin(), ObservationOrigin::Uncompared);
        assert_eq!(returned.visibility(), ObservationVisibility::Included);
        assert_eq!(returned.version(), version);

        // The source and the stored hint keep their own flags.
        assert_eq!(
            observation
                .snapshot_entries()
                .next()
                .unwrap()
                .1
                .observed_version()
                .origin(),
            ObservationOrigin::PriorEqual,
        );
        let stored = hints.rows.get(&path(b"a")).unwrap();
        assert_eq!(stored.origin(), ObservationOrigin::PriorEqual);
        assert_eq!(stored.visibility(), ObservationVisibility::Excluded);
    }

    #[test]
    fn new_builder_does_not_consult_hints() {
        let version = file_version(1, 2);
        let observation = completed(vec![(path(b"a"), version)]);
        let hints = ObservationHints::from_observation(&observation, &PathMap::new(), Side::Left);
        let mut builder = VersionPoolBuilder::new(Arc::new(PathMap::new()));
        let handle = builder.intern(&path(b"a"), Side::Left, version);
        assert_eq!(builder.owned_len(), 1, "new(base) carries no hints");
        let pool = builder.freeze();
        assert_eq!(handle.version(), version);
        drop(pool);
        assert_eq!(handle.version(), version);
        assert!(hints.matching_version(&path(b"a"), version).is_some());
    }

    #[test]
    fn cloning_shares_the_hint_map() {
        let version = file_version(1, 2);
        let observation = completed(vec![(path(b"a"), version)]);
        let hints = ObservationHints::from_observation(&observation, &PathMap::new(), Side::Left);
        let clone = hints.clone();
        drop(hints);
        let handle = clone.matching_version(&path(b"a"), version).unwrap();
        assert_eq!(handle.version(), version);
    }

    #[test]
    fn builder_reuses_hints_and_freezes_shared_handles() {
        let version = file_version(1, 2);
        let observation = completed(vec![(path(b"a"), version), (path(b"b"), version)]);
        let hints = ObservationHints::from_observation(&observation, &PathMap::new(), Side::Left);
        let mut builder = VersionPoolBuilder::with_hints(Arc::new(PathMap::new()), hints);

        let first = builder.intern(&path(b"a"), Side::Left, version);
        let second = builder.intern(&path(b"b"), Side::Left, version);
        assert_eq!(
            builder.owned_len(),
            0,
            "hints must not append owned versions"
        );
        assert_eq!(first, second);
        assert_eq!(first.version(), version);

        let pool = builder.freeze();
        assert_eq!(first.version(), version);
        assert_eq!(second.version(), version);
        drop(pool);
        assert_eq!(first.version(), version);
        assert_eq!(second.version(), version);
    }
}
