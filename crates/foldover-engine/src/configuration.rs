use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use foldover_endpoint::{LocalEndpoint, LocalPair, PairRootError, RootRelation, ScanError};
use foldover_state::{PairStore, StateLocation};
use foldover_types::{PairConfig, PairDraft, PairId, Side};

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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Storage,
    NotFound,
    Changed,
    Unavailable,
    Overlap,
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
    state_root: LocalEndpoint,
}

impl Engine {
    pub fn open(location: &StateLocation) -> Result<Self, Error> {
        let pairs = PairStore::open(location)?;
        let root = pairs.root()?;
        let state_root = LocalEndpoint::open(Path::new(OsStr::from_bytes(root.as_bytes())))?;
        Ok(Self { pairs, state_root })
    }

    pub fn pairs_after(
        &self,
        after: Option<PairId>,
        limit: usize,
    ) -> Result<Vec<PairConfig>, Error> {
        Ok(self.pairs.pairs_after(after, limit)?)
    }

    pub fn pair(&self, id: PairId) -> Result<PairConfig, Error> {
        self.pairs.pair(id)?.ok_or(Error::PairNotFound)
    }

    pub fn create_pair(&mut self, draft: &PairDraft) -> Result<PairConfig, Error> {
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
        self.validate_roots(draft, Some(id))?;
        Ok(self.pairs.update(id, revision, draft)?)
    }

    fn validate_roots(&self, draft: &PairDraft, replacing: Option<PairId>) -> Result<(), Error> {
        let selected = LocalPair::open(draft.left(), draft.right())?;
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
                let existing = LocalPair::open(pair.draft().left(), pair.draft().right()).map_err(
                    |source| Error::CatalogUnavailable {
                        pair: pair.id(),
                        source,
                    },
                )?;
                for selected_side in [Side::Left, Side::Right] {
                    for existing_side in [Side::Left, Side::Right] {
                        if selected
                            .endpoint(selected_side)
                            .relation_to(existing.endpoint(existing_side))?
                            != RootRelation::Disjoint
                        {
                            return Err(Error::CatalogOverlap(pair.id()));
                        }
                    }
                }
            }
        }
    }
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
}
