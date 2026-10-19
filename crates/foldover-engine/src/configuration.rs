use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use std::collections::BTreeMap;

use foldover_endpoint::{
    Endpoint, EndpointPair, Filter, InvalidFilter, LocalEndpoint, ObservedEntry, PairRootError,
    RootRelation, ScanError,
};
use foldover_state::{
    Device, HistoryEntry, Journal, JournalUpdate, MountSources, PairStore, Pause, SnapshotEntry,
    StateLocation,
};
use foldover_types::{
    DeviceId, Durability, PairConfig, PairDraft, PairId, PairRoot, Resolution, Side, SyncPath,
};

use crate::{Commit, RunInputs};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    State(foldover_state::Error),
    #[error(transparent)]
    Roots(#[from] PairRootError),
    #[error(transparent)]
    Endpoint(#[from] ScanError),
    #[error("Pair does not exist")]
    PairNotFound,
    #[error("Pair configuration changed; refresh before updating it")]
    StaleRevision,
    #[error("Selected root overlaps pair {0}")]
    CatalogOverlap(PairId),
    #[error("Reconnect the roots of pair {pair} before changing the catalog: {source}")]
    CatalogUnavailable { pair: PairId, source: PairRootError },
    #[error("Selected root overlaps the agent's state directory")]
    StateOverlap,
    #[error("SFTP folders cannot guarantee full durability; choose standard durability")]
    DurabilityUnsupported,
    #[error(transparent)]
    Filter(#[from] InvalidFilter),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Storage,
    NotFound,
    Changed,
    Unavailable,
    Overlap,
    Invalid,
}

impl Error {
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::State(_) => ErrorKind::Storage,
            Self::PairNotFound => ErrorKind::NotFound,
            Self::StaleRevision => ErrorKind::Changed,
            Self::Roots(PairRootError::Overlap(_))
            | Self::CatalogOverlap(_)
            | Self::StateOverlap => ErrorKind::Overlap,
            Self::Filter(_) | Self::DurabilityUnsupported => ErrorKind::Invalid,
            Self::Roots(_) | Self::Endpoint(_) | Self::CatalogUnavailable { .. } => {
                ErrorKind::Unavailable
            }
        }
    }
}

impl From<foldover_state::Error> for Error {
    fn from(error: foldover_state::Error) -> Self {
        match error {
            foldover_state::Error::PairNotFound => Self::PairNotFound,
            foldover_state::Error::StaleRevision => Self::StaleRevision,
            error => Self::State(error),
        }
    }
}

/// The agent's single configuration writer. Filesystem checks and a mutation
/// share one serialized command, so concurrent clients cannot reserve the same roots.
pub struct Engine {
    pairs: PairStore,
    state_root: Endpoint,
}

impl Engine {
    pub fn open(location: &StateLocation) -> Result<Self, Error> {
        let pairs = PairStore::open(location)?;
        let root = pairs.root()?;
        let state_root = Endpoint::Local(LocalEndpoint::open(Path::new(OsStr::from_bytes(
            root.as_bytes(),
        )))?);
        Ok(Self { pairs, state_root })
    }

    pub fn pairs_after(
        &self,
        after: Option<PairId>,
        limit: usize,
    ) -> Result<Vec<PairConfig>, Error> {
        Ok(self.pairs.pairs_after(after, limit)?)
    }

    /// The whole catalog, for agent-internal work such as scheduling.
    pub fn pairs(&self) -> Result<Vec<PairConfig>, Error> {
        Ok(self.pairs.pairs()?)
    }

    pub fn pair(&self, id: PairId) -> Result<PairConfig, Error> {
        self.pairs.pair(id)?.ok_or(Error::PairNotFound)
    }

    pub fn create_pair(&mut self, draft: &PairDraft) -> Result<PairConfig, Error> {
        validate_options(draft)?;
        self.validate_roots(draft, None)?;
        Ok(self.pairs.create(draft)?)
    }

    pub fn update_pair(
        &mut self,
        id: PairId,
        revision: u64,
        draft: &PairDraft,
    ) -> Result<PairConfig, Error> {
        if self.pair(id)?.revision() != revision {
            return Err(Error::StaleRevision);
        }
        validate_options(draft)?;
        self.validate_roots(draft, Some(id))?;
        Ok(self.pairs.update(id, revision, draft)?)
    }

    /// What a run reads from state, for work outside the engine lock.
    pub fn run_inputs(&self, id: PairId) -> Result<RunInputs, Error> {
        // Malformed cached fingerprints or versions only cost a reread.
        let snapshot = |side| -> Result<BTreeMap<_, _>, Error> {
            Ok(self
                .pairs
                .snapshot(id, side)?
                .into_iter()
                .filter_map(|entry| {
                    let observed = ObservedEntry::restore(entry.version, &entry.fingerprint)?;
                    Some((entry.path, observed))
                })
                .collect())
        };
        Ok(RunInputs::new(
            self.pair(id)?,
            self.pairs.base(id)?,
            self.pairs.identities(id)?,
            self.pairs.resolutions(id)?,
        )
        .with_snapshots(snapshot(Side::Left)?, snapshot(Side::Right)?))
    }

    /// Store a fresh observation of one side for the next run to reuse.
    pub fn store_snapshot(
        &mut self,
        id: PairId,
        revision: u64,
        side: Side,
        entries: &[SnapshotEntry],
    ) -> Result<(), Error> {
        Ok(self.pairs.replace_snapshot(id, revision, side, entries)?)
    }

    /// Store a person's conflict choice for the pair revision they previewed.
    pub fn record_resolution(
        &mut self,
        id: PairId,
        revision: u64,
        path: &SyncPath,
        resolution: Resolution,
    ) -> Result<(), Error> {
        Ok(self
            .pairs
            .record_resolution(id, revision, path, resolution)?)
    }

    pub fn release_automation(&mut self, id: PairId) -> Result<(), Error> {
        Ok(self.pairs.release_automation(id)?)
    }

    pub fn automation_held(&self, id: PairId) -> Result<bool, Error> {
        Ok(self.pairs.automation_held(id)?)
    }

    /// Paired Macs (ADR 0031), by name.
    pub fn devices(&self) -> Result<Vec<Device>, Error> {
        Ok(self.pairs.devices()?)
    }

    /// Record a device both people confirmed, as paired now.
    pub fn add_device(&mut self, id: DeviceId, name: String) -> Result<(), Error> {
        Ok(self.pairs.put_device(&Device {
            id,
            name,
            paired: std::time::SystemTime::now(),
        })?)
    }

    /// Forget a device; whether it was paired.
    pub fn remove_device(&mut self, id: DeviceId) -> Result<bool, Error> {
        Ok(self.pairs.remove_device(id)?)
    }

    /// The pair's newest writing run, while it can still be undone.
    pub fn journal(&self, id: PairId) -> Result<Option<Journal>, Error> {
        Ok(self.pairs.journal(id)?)
    }

    /// Whether the pair has completed a first sync, which records identities.
    pub fn first_synced(&self, id: PairId) -> Result<bool, Error> {
        let identities = self.pairs.identities(id)?;
        Ok(identities.side(Side::Left).is_some() && identities.side(Side::Right).is_some())
    }

    /// Store a run's progress, only while the pair keeps `revision`.
    pub fn commit_run(
        &mut self,
        id: PairId,
        revision: u64,
        commit: Commit<'_>,
    ) -> Result<(), Error> {
        match commit {
            Commit::Identities { left, right } => {
                Ok(self.pairs.record_identities(id, revision, left, right)?)
            }
            Commit::Base {
                run,
                updates,
                journal,
            } => Ok(self.pairs.update_base(
                id,
                revision,
                updates,
                JournalUpdate::Record {
                    run,
                    entries: journal,
                },
            )?),
            Commit::Undoing { updates } => {
                Ok(self
                    .pairs
                    .update_base(id, revision, updates, JournalUpdate::Hold)?)
            }
            Commit::Undid { run, seqs } => {
                Ok(self
                    .pairs
                    .update_base(id, revision, &[], JournalUpdate::Undid { run, seqs })?)
            }
            Commit::Undone { run, updates } => {
                Ok(self
                    .pairs
                    .update_base(id, revision, updates, JournalUpdate::Spend { run })?)
            }
        }
    }

    /// Store a finished run; the pair keeps its newest runs.
    pub fn record_history(&mut self, id: PairId, entry: &HistoryEntry) -> Result<(), Error> {
        Ok(self.pairs.record_history(id, entry)?)
    }

    /// The pair's newest runs first, at most `limit`.
    pub fn history(&self, id: PairId, limit: usize) -> Result<Vec<HistoryEntry>, Error> {
        Ok(self.pairs.history(id, limit)?)
    }

    /// Where each side's network share was last mounted from.
    pub fn mounts(&self, id: PairId) -> Result<MountSources, Error> {
        Ok(self.pairs.mounts(id)?)
    }

    /// Record mount sources a run observed, only while the pair keeps
    /// `revision`.
    pub fn record_mounts(
        &mut self,
        id: PairId,
        revision: u64,
        sources: &MountSources,
    ) -> Result<(), Error> {
        Ok(self.pairs.record_mounts(id, revision, sources)?)
    }

    /// Whether automatic syncs are paused, for every pair.
    pub fn pause(&self) -> Result<Pause, Error> {
        Ok(self.pairs.pause()?)
    }

    pub fn set_pause(&mut self, pause: Pause) -> Result<(), Error> {
        Ok(self.pairs.set_pause(pause)?)
    }

    pub fn remove_pair(&mut self, id: PairId, revision: u64) -> Result<PairConfig, Error> {
        // The caller reserves this pair against active or starting jobs.
        Ok(self.pairs.remove(id, revision)?)
    }

    fn validate_roots(&self, draft: &PairDraft, replacing: Option<PairId>) -> Result<(), Error> {
        let selected = EndpointPair::open(draft.left(), draft.right())?;
        for side in [Side::Left, Side::Right] {
            if selected.endpoint(side).relation_to(&self.state_root)? != RootRelation::Disjoint {
                return Err(Error::StateOverlap);
            }
        }
        // Page the catalog to bound memory even when many pairs are configured.
        let mut after = None;
        const CATALOG_BATCH: usize = 32;
        loop {
            let page = self.pairs.pairs_after(after, CATALOG_BATCH)?;
            if page.is_empty() {
                return Ok(());
            }
            for pair in page {
                after = Some(pair.id());
                if Some(pair.id()) == replacing {
                    continue;
                }
                // Local roots are compared on disk; SFTP roots by locator, so
                // changing the catalog never waits on a server.
                for existing in [pair.draft().left(), pair.draft().right()] {
                    let overlapping = match existing {
                        PairRoot::Local(_) => {
                            let existing = Endpoint::open(existing).map_err(|source| {
                                Error::CatalogUnavailable {
                                    pair: pair.id(),
                                    source: PairRootError::Endpoint(source),
                                }
                            })?;
                            let mut overlapping = false;
                            for side in [Side::Left, Side::Right] {
                                overlapping |= selected.endpoint(side).relation_to(&existing)?
                                    != RootRelation::Disjoint;
                            }
                            overlapping
                        }
                        PairRoot::Sftp(existing) => {
                            [draft.left(), draft.right()]
                                .into_iter()
                                .any(|selected| match selected {
                                    PairRoot::Sftp(selected) => selected.overlaps(existing),
                                    PairRoot::Local(_) => false,
                                })
                        }
                    };
                    if overlapping {
                        return Err(Error::CatalogOverlap(pair.id()));
                    }
                }
            }
        }
    }
}

/// Options the chosen folders cannot keep are refused before any root
/// is opened, so an unreachable server cannot hide the rejection.
fn validate_options(draft: &PairDraft) -> Result<(), Error> {
    // SFTP has no flush for namespace changes, so full durability can
    // never be promised to or from it.
    let remote = |root: &PairRoot| matches!(root, PairRoot::Sftp(_));
    if draft.options().durability() == Durability::Full
        && (remote(draft.left()) || remote(draft.right()))
    {
        return Err(Error::DurabilityUnsupported);
    }
    validate_filters(draft)
}

/// Rules are checked when saved, so a run never meets an invalid one.
fn validate_filters(draft: &PairDraft) -> Result<(), Error> {
    Filter::new(draft.options().filters().rules().iter().map(String::as_str))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use foldover_types::{LocalRoot, SyncMode};
    use std::fs;
    use std::os::unix::fs::symlink;

    fn draft(left: &Path, right: &Path) -> PairDraft {
        PairDraft::new("Pair".into(), root(left), root(right), SyncMode::Sync).unwrap()
    }

    fn root(path: &Path) -> LocalRoot {
        LocalRoot::from_bytes(path.as_os_str().as_bytes().to_vec()).unwrap()
    }

    #[test]
    fn catalog_rejects_nested_aliased_and_state_roots_without_mutation() {
        let temporary = tempfile::tempdir().unwrap();
        let mut engine =
            Engine::open(&StateLocation::Directory(temporary.path().join("state"))).unwrap();
        let left = temporary.path().join("left");
        let right = temporary.path().join("right");
        let third = temporary.path().join("third");
        fs::create_dir_all(left.join("child")).unwrap();
        fs::create_dir(&right).unwrap();
        fs::create_dir(&third).unwrap();
        let pair = engine.create_pair(&draft(&left, &right)).unwrap();
        let alias = temporary.path().join("alias");
        symlink(&left, &alias).unwrap();
        for overlap in [&left, &alias, &left.join("child")] {
            assert!(
                matches!(engine.create_pair(&draft(overlap, &third)), Err(Error::CatalogOverlap(id)) if id == pair.id())
            );
        }
        assert!(matches!(
            engine.create_pair(&draft(temporary.path(), &third)),
            Err(Error::Roots(_))
        ));
        assert!(matches!(
            engine.create_pair(&draft(&temporary.path().join("state"), &third)),
            Err(Error::StateOverlap)
        ));
        assert_eq!(engine.pairs_after(None, 10).unwrap(), vec![pair]);
    }

    #[test]
    fn updates_are_revision_checked_and_offline_catalog_fails_closed() {
        let temporary = tempfile::tempdir().unwrap();
        let location = StateLocation::Directory(temporary.path().join("state"));
        let mut engine = Engine::open(&location).unwrap();
        let paths: Vec<_> = ["a", "b", "c", "d"]
            .map(|name| temporary.path().join(name))
            .into();
        for path in &paths {
            fs::create_dir(path).unwrap();
        }
        let saved = engine.create_pair(&draft(&paths[0], &paths[1])).unwrap();
        let updated = engine
            .update_pair(saved.id(), saved.revision(), saved.draft())
            .unwrap();
        assert_eq!(updated.revision(), saved.revision() + 1);
        fs::remove_dir(&paths[0]).unwrap();
        assert!(matches!(
            engine.update_pair(saved.id(), saved.revision(), saved.draft()),
            Err(Error::StaleRevision)
        ));
        assert!(matches!(
            engine.create_pair(&draft(&paths[2], &paths[3])),
            Err(Error::CatalogUnavailable { .. })
        ));
        drop(engine);
        let engine = Engine::open(&location).unwrap();
        assert_eq!(engine.pair(saved.id()).unwrap(), updated);
    }

    /// Full durability cannot be promised on SFTP: the check precedes
    /// root validation, so an unreachable server cannot hide it.
    #[test]
    fn sftp_pairs_reject_full_durability_before_any_root_check() {
        let temporary = tempfile::tempdir().unwrap();
        let location = StateLocation::Directory(temporary.path().join("state"));
        let mut engine = Engine::open(&location).unwrap();
        let right = temporary.path().join("right");
        fs::create_dir(&right).unwrap();
        let sftp = PairRoot::from_bytes(b"sftp://nas/srv".to_vec()).unwrap();
        let full = foldover_types::PairOptions::new(
            foldover_types::MetadataPolicy::default(),
            foldover_types::Durability::Full,
        );
        for (left, right_root) in [
            (sftp.clone(), PairRoot::from(root(&right))),
            (PairRoot::from(root(&right)), sftp.clone()),
        ] {
            let draft = PairDraft::new("Pair".into(), left, right_root, SyncMode::Sync)
                .unwrap()
                .with_options(full.clone());
            let error = engine.create_pair(&draft).unwrap_err();
            assert!(matches!(error, Error::DurabilityUnsupported), "{error:?}");
            assert_eq!(error.kind(), ErrorKind::Invalid);
        }
        assert!(engine.pairs().unwrap().is_empty());
        drop(engine);

        // A draft with Full over SFTP still decodes from stored bytes:
        // the rejection lives in the save path, not the data model.
        let draft = PairDraft::new("Pair".into(), sftp.clone(), root(&right), SyncMode::Sync)
            .unwrap()
            .with_options(full.clone());
        let stored =
            PairConfig::from_stored(PairId::from_bytes([7; PairId::BYTE_LEN]).unwrap(), 1, draft)
                .unwrap();
        assert_eq!(
            stored.draft().options().durability(),
            foldover_types::Durability::Full
        );

        // An existing full-durability pair stays editable as long as
        // the saved draft remains one the folders can keep.
        let left = temporary.path().join("left");
        fs::create_dir(&left).unwrap();
        let draft = PairDraft::new("Pair".into(), root(&left), root(&right), SyncMode::Sync)
            .unwrap()
            .with_options(full.clone());
        let mut engine = Engine::open(&location).unwrap();
        let pair = engine.create_pair(&draft).unwrap();
        let revision = engine
            .update_pair(pair.id(), pair.revision(), pair.draft())
            .unwrap()
            .revision();
        // The same pair gains an SFTP root: re-saving it is refused.
        let remote = PairDraft::new("Pair".into(), root(&left), sftp, SyncMode::Sync)
            .unwrap()
            .with_options(full);
        assert!(matches!(
            engine.update_pair(pair.id(), revision, &remote),
            Err(Error::DurabilityUnsupported)
        ));
    }

    #[test]
    fn offline_removal_checks_revision_and_releases_catalog_roots() {
        let temporary = tempfile::tempdir().unwrap();
        let mut engine =
            Engine::open(&StateLocation::Directory(temporary.path().join("state"))).unwrap();
        let paths = ["a", "b", "c", "d"].map(|name| temporary.path().join(name));
        for path in &paths {
            fs::create_dir(path).unwrap();
        }
        let original = engine.create_pair(&draft(&paths[0], &paths[1])).unwrap();
        let updated = engine
            .update_pair(original.id(), original.revision(), original.draft())
            .unwrap();
        fs::write(paths[0].join("kept"), b"keep this file").unwrap();
        let detached = temporary.path().join("detached");
        fs::rename(&paths[0], &detached).unwrap();
        assert!(matches!(
            engine.remove_pair(original.id(), original.revision()),
            Err(Error::StaleRevision)
        ));
        assert_eq!(engine.pair(updated.id()).unwrap(), updated);
        assert_eq!(
            engine
                .remove_pair(updated.id(), updated.revision())
                .unwrap(),
            updated
        );
        assert!(matches!(
            engine.pair(updated.id()),
            Err(Error::PairNotFound)
        ));
        assert_eq!(fs::read(detached.join("kept")).unwrap(), b"keep this file");
        assert!(paths[1].is_dir());
        assert!(engine.create_pair(&draft(&paths[2], &paths[3])).is_ok());
    }
}
