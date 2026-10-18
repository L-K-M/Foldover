//! Browsing one side's archive and putting items back (ADR 0025). Each
//! side keeps what runs displaced or removed under
//! `.foldover/archive/<run>/<original path>` for 30 days.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use foldover_endpoint::{ApplyError, LocalEndpoint, ScanError};
use foldover_types::{Durability, PairConfig, Side, SyncPath};

pub use foldover_endpoint::{ArchivedItem, ArchivedRun};

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error(transparent)]
    Folder(#[from] ScanError),
    #[error(transparent)]
    PutBack(#[from] ApplyError),
}

/// What a client did wrong, or what the folder could not do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveErrorKind {
    /// The run or item is not in the archive.
    NotFound,
    /// The path names Foldover's own files.
    Invalid,
    /// The folder is missing or refused the operation.
    Unavailable,
}

impl ArchiveError {
    pub fn kind(&self) -> ArchiveErrorKind {
        match self {
            Self::Folder(ScanError::UnknownArchiveRun)
            | Self::PutBack(ApplyError::Scan(ScanError::UnknownArchiveRun))
            | Self::PutBack(ApplyError::NotArchived(_)) => ArchiveErrorKind::NotFound,
            Self::PutBack(ApplyError::Unsupported(_) | ApplyError::InvalidPath(_)) => {
                ArchiveErrorKind::Invalid
            }
            Self::Folder(_) | Self::PutBack(_) => ArchiveErrorKind::Unavailable,
        }
    }
}

/// One side's archive. Opening it needs only the pair's configuration, so
/// callers browse without holding the engine.
pub struct PairArchive {
    endpoint: LocalEndpoint,
    durability: Durability,
}

impl PairArchive {
    pub fn open(pair: &PairConfig, side: Side) -> Result<Self, ArchiveError> {
        let root = match side {
            Side::Left => pair.draft().left(),
            Side::Right => pair.draft().right(),
        };
        let endpoint = LocalEndpoint::open(Path::new(OsStr::from_bytes(root.as_bytes())))?;
        Ok(Self {
            endpoint,
            durability: pair.draft().options().durability(),
        })
    }

    /// Archived runs, newest first.
    pub fn runs(&self) -> Result<Vec<ArchivedRun>, ArchiveError> {
        Ok(self.endpoint.archived_runs()?)
    }

    /// A page of `run`'s items in path order after `after`, and whether
    /// more follow.
    pub fn items(
        &self,
        run: &str,
        after: Option<&SyncPath>,
        limit: usize,
    ) -> Result<(Vec<ArchivedItem>, bool), ArchiveError> {
        Ok(self.endpoint.archived_items(run, after, limit)?)
    }

    /// Move an item back to its original path, or beside it when that is
    /// taken, and return where it went. The next sync copies it across.
    pub fn put_back(&self, run: &str, path: &SyncPath) -> Result<SyncPath, ArchiveError> {
        Ok(self.endpoint.put_back(run, path, self.durability)?)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::ffi::OsStrExt;

    use foldover_endpoint::archive_name;
    use foldover_types::{LocalRoot, PairDraft, PairId, SyncMode};

    use super::*;

    #[test]
    fn items_go_back_on_the_side_that_archived_them() {
        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left");
        let right = directory.path().join("right");
        fs::create_dir_all(&right).unwrap();
        let run = archive_name(std::time::UNIX_EPOCH);
        let folder = left.join(".foldover/archive").join(&run);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("note"), b"kept").unwrap();
        let root =
            |path: &Path| LocalRoot::from_bytes(path.as_os_str().as_bytes().to_vec()).unwrap();
        let draft =
            PairDraft::new("Pair".into(), root(&left), root(&right), SyncMode::Sync).unwrap();
        let pair =
            PairConfig::from_stored(PairId::from_bytes([1; PairId::BYTE_LEN]).unwrap(), 1, draft)
                .unwrap();

        let archive = PairArchive::open(&pair, Side::Left).unwrap();
        assert_eq!(archive.runs().unwrap()[0].name(), run);
        let (items, more) = archive.items(&run, None, 10).unwrap();
        assert_eq!(items[0].path().as_bytes(), b"note");
        assert!(!more);
        let placed = archive.put_back(&run, items[0].path()).unwrap();
        assert_eq!(
            fs::read(left.join(OsStr::from_bytes(placed.as_bytes()))).unwrap(),
            b"kept"
        );
        assert!(
            PairArchive::open(&pair, Side::Right)
                .unwrap()
                .runs()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            archive
                .put_back("nope", items[0].path())
                .unwrap_err()
                .kind(),
            ArchiveErrorKind::NotFound
        );
    }
}
