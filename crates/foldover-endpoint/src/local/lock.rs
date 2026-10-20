//! The lease files of a root (ADR 0032): small private files under
//! `.foldover`, written whole and replaced atomically.

use foldover_fs::WriteError;
use foldover_types::{Durability, SyncPath};

use super::{ApplyError, LocalEndpoint, ScanError, at};
use crate::lease::{LEASE_LIMIT, LockFile};

fn path(file: LockFile) -> SyncPath {
    SyncPath::try_from(file.path()).expect("lease paths are valid")
}

impl LocalEndpoint {
    pub(crate) fn read_lock_file(&self, file: LockFile) -> Result<Option<Vec<u8>>, ScanError> {
        let path = path(file);
        at(
            &Some(path.clone()),
            self.root.read_private_file(&path, LEASE_LIMIT as u64),
        )
    }

    /// Write `file` unless it exists; whether it was written.
    pub(crate) fn create_lock_file(
        &self,
        file: LockFile,
        content: &[u8],
    ) -> Result<bool, ApplyError> {
        let path = path(file);
        match self
            .root
            .create_private_file(&path, content, Durability::Standard)
        {
            Ok(()) => Ok(true),
            Err(WriteError::Exists) => Ok(false),
            Err(source) => Err(ApplyError::Write { path, source }),
        }
    }

    pub(crate) fn replace_lock_file(
        &self,
        file: LockFile,
        content: &[u8],
    ) -> Result<(), ApplyError> {
        let path = path(file);
        self.root
            .replace_private_file(&path, content, Durability::Standard)
            .map_err(|source| ApplyError::Write { path, source })
    }

    pub(crate) fn remove_lock_file(&self, file: LockFile) -> Result<(), ApplyError> {
        let path = path(file);
        self.root
            .remove_private_file(&path)
            .map_err(|source| ApplyError::Write { path, source })
    }

    /// Move the claim over the lock.
    pub(crate) fn promote_claim(&self) -> Result<(), ApplyError> {
        let (claim, lock) = (path(LockFile::Claim), path(LockFile::Lock));
        self.root
            .rename_private_file(&claim, &lock, Durability::Standard)
            .map_err(|source| ApplyError::Write { path: lock, source })
    }
}
