//! The identity file each root receives on its first sync (PLAN.md §4.7).

use foldover_fs::WriteError;
use foldover_types::{Durability, EndpointId, SyncPath};

use super::{ApplyError, LocalEndpoint, ScanError, at};

const IDENTITY_FILE: &[u8] = b".foldover/id";
/// 32 hex digits and a newline, with room to reject anything longer.
const IDENTITY_FILE_LIMIT: u64 = 64;

fn identity_path() -> SyncPath {
    SyncPath::try_from(IDENTITY_FILE).expect("the identity path is valid")
}

impl LocalEndpoint {
    /// The identity this root carries, or `None` before its first sync.
    pub fn identity(&self) -> Result<Option<EndpointId>, ScanError> {
        let path = identity_path();
        let Some(content) = at(
            &Some(path.clone()),
            self.root.read_private_file(&path, IDENTITY_FILE_LIMIT),
        )?
        else {
            return Ok(None);
        };
        let text = content.strip_suffix(b"\n").unwrap_or(&content);
        EndpointId::from_hex(text)
            .map(Some)
            .ok_or(ScanError::InvalidIdentity)
    }

    /// The root's identity, writing a new one if it has none. Folders shared
    /// by several pairs keep one identity.
    pub fn ensure_identity(&self, durability: Durability) -> Result<EndpointId, ApplyError> {
        if let Some(identity) = self.identity()? {
            return Ok(identity);
        }
        let identity = EndpointId::from_bytes(*uuid::Uuid::new_v4().as_bytes());
        let path = identity_path();
        match self
            .root
            .create_private_file(&path, format!("{identity}\n").as_bytes(), durability)
        {
            Ok(()) => Ok(identity),
            // Another writer was first; its identity stands.
            Err(WriteError::Exists) => self.identity()?.ok_or(ApplyError::DestinationChanged(path)),
            Err(source) => Err(ApplyError::Write { path, source }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn identities_are_written_once_and_validated() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();
        assert_eq!(endpoint.identity().unwrap(), None);
        let identity = endpoint.ensure_identity(Durability::Standard).unwrap();
        assert_eq!(endpoint.identity().unwrap(), Some(identity));
        assert_eq!(
            endpoint.ensure_identity(Durability::Standard).unwrap(),
            identity
        );
        assert_eq!(
            fs::read(directory.path().join(".foldover/id")).unwrap(),
            format!("{identity}\n").as_bytes()
        );

        fs::write(directory.path().join(".foldover/id"), b"not an identity\n").unwrap();
        assert!(matches!(
            endpoint.identity(),
            Err(ScanError::InvalidIdentity)
        ));
        assert!(endpoint.ensure_identity(Durability::Standard).is_err());
    }
}
