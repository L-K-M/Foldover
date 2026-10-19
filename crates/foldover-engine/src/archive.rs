//! Browsing one side's archive and putting items back (ADR 0025). Each
//! side keeps what runs displaced or removed under
//! `.foldover/archive/<run>/<original path>` for 30 days.

use foldover_endpoint::{ApplyError, Endpoint, ScanError};
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
            Self::PutBack(
                ApplyError::Unsupported(_)
                | ApplyError::InvalidPath(_)
                | ApplyError::DurabilityUnsupported(_),
            ) => ArchiveErrorKind::Invalid,
            Self::Folder(_) | Self::PutBack(_) => ArchiveErrorKind::Unavailable,
        }
    }
}

/// What putting back a whole run did.
#[derive(Debug, Default)]
pub struct PutBackReport {
    restored: u64,
    beside: u64,
    failures: Vec<(SyncPath, String)>,
}

impl PutBackReport {
    /// Items back at their original path.
    pub fn restored(&self) -> u64 {
        self.restored
    }

    /// Items put beside a newer entry at their path, as `(restored)`.
    pub fn beside(&self) -> u64 {
        self.beside
    }

    pub fn failures(&self) -> &[(SyncPath, String)] {
        &self.failures
    }
}

/// One side's archive. Opening it needs only the pair's configuration, so
/// callers browse without holding the engine.
pub struct PairArchive {
    endpoint: Endpoint,
    durability: Durability,
}

impl PairArchive {
    pub fn open(pair: &PairConfig, side: Side) -> Result<Self, ArchiveError> {
        let root = match side {
            Side::Left => pair.draft().left(),
            Side::Right => pair.draft().right(),
        };
        let endpoint = Endpoint::open(root)?;
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
        // A rename has no flush over SFTP, so a full-durability pair is
        // refused rather than silently downgraded.
        if self.durability == Durability::Full && matches!(self.endpoint, Endpoint::Sftp(_)) {
            return Err(ApplyError::DurabilityUnsupported(path.clone()).into());
        }
        Ok(self.endpoint.put_back(run, path, self.durability)?)
    }

    /// Put back everything `run` archived. One item's failure does not stop
    /// the rest; the report lists it.
    pub fn put_back_all(&self, run: &str) -> Result<PutBackReport, ArchiveError> {
        // List first: putting items back changes the folder being listed.
        let (items, _) = self.items(run, None, usize::MAX)?;
        let mut report = PutBackReport::default();
        for item in items {
            match self.put_back(run, item.path()) {
                Ok(placed) if &placed == item.path() => report.restored += 1,
                Ok(_) => report.beside += 1,
                Err(error) => report
                    .failures
                    .push((item.path().clone(), error.to_string())),
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

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

        // The rest of a run comes back in one go, beside anything newer.
        fs::write(folder.join("a"), b"a").unwrap();
        fs::write(folder.join("b"), b"b").unwrap();
        fs::write(left.join("b"), b"newer").unwrap();
        let report = archive.put_back_all(&run).unwrap();
        assert_eq!((report.restored(), report.beside()), (1, 1));
        assert!(report.failures().is_empty());
        assert_eq!(fs::read(left.join("b")).unwrap(), b"newer");
        assert_eq!(fs::read(left.join("b (restored)")).unwrap(), b"b");

        assert_eq!(
            archive
                .put_back("nope", items[0].path())
                .unwrap_err()
                .kind(),
            ArchiveErrorKind::NotFound
        );
    }

    /// Putting an item back renames on the server; a pair asking for
    /// full durability is refused before the rename is attempted.
    #[test]
    fn full_durability_put_back_is_rejected_on_sftp() {
        let Some(server) = std::env::var_os("FOLDOVER_SFTP_SERVER")
            .map(std::path::PathBuf::from)
            .into_iter()
            .chain(
                [
                    "/usr/libexec/sftp-server",
                    "/usr/lib/openssh/sftp-server",
                    "/usr/lib/ssh/sftp-server",
                ]
                .map(std::path::PathBuf::from),
            )
            .find(|path| path.exists())
        else {
            eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let endpoint = foldover_endpoint::SftpEndpoint::spawn(
            &server,
            &[OsStr::new("-d"), OsStr::new("/")],
            directory.path().as_os_str().as_bytes(),
        )
        .unwrap();
        let run = archive_name(std::time::UNIX_EPOCH);
        let folder = directory.path().join(".foldover/archive").join(&run);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("note"), b"kept").unwrap();

        let archive = PairArchive {
            endpoint: Endpoint::Sftp(endpoint),
            durability: Durability::Full,
        };
        let path = SyncPath::try_from(b"note".as_slice()).unwrap();
        let error = archive.put_back(&run, &path).unwrap_err();
        assert!(
            matches!(
                error,
                ArchiveError::PutBack(ApplyError::DurabilityUnsupported(_))
            ),
            "{error:?}"
        );
        assert_eq!(error.kind(), ArchiveErrorKind::Invalid);
        // The item stayed in the archive; nothing appeared at its path.
        assert!(!directory.path().join("note").exists());
        assert_eq!(fs::read(folder.join("note")).unwrap(), b"kept");

        // Standard puts it back on the same server.
        let archive = PairArchive {
            endpoint: archive.endpoint,
            durability: Durability::Standard,
        };
        assert_eq!(archive.put_back(&run, &path).unwrap(), path);
        assert_eq!(fs::read(directory.path().join("note")).unwrap(), b"kept");
    }
}
