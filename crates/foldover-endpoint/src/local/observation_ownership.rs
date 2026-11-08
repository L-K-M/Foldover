//! Private moves and comparison flags for consumed observation snapshots.

use std::time::UNIX_EPOCH;

use super::{Fingerprint, ObservedEntry};
use crate::observation_versions::{ObservationOrigin, ObservationVisibility, ObservedVersion};

impl ObservedEntry {
    /// Leave a private placeholder while a bounded read owns the original.
    /// No placeholder crosses the endpoint's completed-observation boundary.
    pub(crate) fn take(&mut self) -> Self {
        assert!(self.version.is_some(), "an original is taken only once");
        Self {
            version: self.version.take(),
            fingerprint: std::mem::replace(
                &mut self.fingerprint,
                Fingerprint {
                    key: Box::from([]),
                    size: 0,
                    observed_at: UNIX_EPOCH,
                },
            ),
        }
    }

    pub(crate) fn is_taken(&self) -> bool {
        self.version.is_none()
    }

    pub(crate) fn set_version(&mut self, version: ObservedVersion) {
        self.version = Some(version);
    }

    pub(crate) fn origin(&self) -> ObservationOrigin {
        self.version
            .as_ref()
            .expect("completed entries hold a version")
            .origin()
    }

    pub(crate) fn set_origin(&mut self, origin: ObservationOrigin) {
        self.version
            .as_mut()
            .expect("completed entries hold a version")
            .set_origin(origin);
    }

    pub(crate) fn set_visibility(&mut self, visibility: ObservationVisibility) {
        self.version
            .as_mut()
            .expect("completed entries hold a version")
            .set_visibility(visibility);
    }

    pub(crate) fn is_excluded(&self) -> bool {
        self.version
            .as_ref()
            .expect("completed entries hold a version")
            .visibility()
            == ObservationVisibility::Excluded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Observation;
    use foldover_types::{Digest, EntryContent, EntryVersion, PathMap, Side, SyncPath};
    use std::sync::Arc;

    #[test]
    fn cancelling_a_directory_only_walk_stops_after_the_first_listing() {
        use crate::{Filter, LocalEndpoint, ObserveStep, ScanControl, ScanError};
        use std::cell::Cell;
        use std::num::NonZeroUsize;
        let root = tempfile::tempdir().unwrap();
        for index in 0..32 {
            std::fs::create_dir(root.path().join(format!("d-{index:03}"))).unwrap();
        }
        let endpoint = LocalEndpoint::open(root.path()).unwrap();
        let listed = Cell::new(0);
        let result = endpoint.observe_reporting_workers(
            foldover_types::MetadataPolicy::default(),
            &Filter::none(),
            &std::collections::BTreeMap::new(),
            &mut || {
                if listed.get() == 0 {
                    ScanControl::Continue
                } else {
                    ScanControl::Cancel
                }
            },
            &mut |step| {
                if step == ObserveStep::Listed {
                    listed.set(listed.get() + 1);
                }
            },
            NonZeroUsize::new(4).unwrap(),
        );
        assert!(matches!(result, Err(ScanError::Cancelled)));
        assert_eq!(
            listed.get(),
            1,
            "directory inventory must honor the caller's stop"
        );
    }

    #[test]
    fn an_equal_result_keeps_the_original_row_and_fingerprint_allocations() {
        let path = SyncPath::try_from(b"a".as_slice()).unwrap();
        let entry = ObservedEntry::for_tests(
            EntryVersion::new(
                EntryContent::File(Digest::from_bytes([1; 32])),
                Digest::from_bytes([2; 32]),
            ),
            12,
        );
        let candidate = entry.clone();
        let previous: PathMap<_> = [(path.clone(), entry)].into_iter().collect();
        let row = previous.get(&path).unwrap() as *const ObservedEntry;
        let payload = previous[&path].fingerprint.key.as_ptr();
        let (mut observed, cache) =
            Observation::recycling(previous, Arc::new(PathMap::new()), Side::Left);
        // This mirrors a local settled hit or an SFTP read confirming the
        // same version. Drop the reader's Arc before the caller lands it.
        drop(cache.get(&path));
        observed.record(path.clone(), candidate);
        observed.finish_entries();
        assert!(std::ptr::eq(row, &observed.entries()[&path]));
        assert_eq!(payload, observed.entries()[&path].fingerprint.key.as_ptr());
        assert_eq!(observed.finish_snapshot(), Some(false));
    }

    #[test]
    fn a_settled_cache_hit_moves_the_original_payload_without_cloning_it() {
        let path = SyncPath::try_from(b"a".as_slice()).unwrap();
        let entry = ObservedEntry::for_tests(
            EntryVersion::new(
                EntryContent::File(Digest::from_bytes([1; 32])),
                Digest::from_bytes([2; 32]),
            ),
            12,
        );
        let previous: PathMap<_> = [(path.clone(), entry)].into_iter().collect();
        let payload = previous[&path].fingerprint.key.as_ptr();
        let (mut observed, cache) =
            Observation::recycling(previous, Arc::new(PathMap::new()), Side::Left);
        let known = cache.get(&path).unwrap();
        observed.reuse(path.clone(), known);
        observed.finish_entries();
        assert_eq!(payload, observed.entries()[&path].fingerprint.key.as_ptr());
        assert_eq!(observed.finish_snapshot(), Some(false));
    }
}
