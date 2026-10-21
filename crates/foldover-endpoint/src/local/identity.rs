//! The identity file each root receives on its first sync (PLAN.md §4.7).

use foldover_fs::WriteError;
use foldover_types::{Durability, EndpointId, SyncPath};

use super::{ApplyError, INTERNAL_DIRECTORY, LocalEndpoint, ScanError, at};

const IDENTITY_FILE: &[u8] = b".foldover/id";
/// 32 hex digits and a newline, with room to reject anything longer.
const IDENTITY_FILE_LIMIT: u64 = 64;

fn identity_path() -> SyncPath {
    SyncPath::try_from(IDENTITY_FILE).expect("the identity path is valid")
}

impl LocalEndpoint {
    /// Whether this folder can hold Foldover's records, as a Backup
    /// target without them need not (PLAN.md §3.6). A folder with an
    /// identity holds them; otherwise a probe file is written in
    /// `.foldover` and removed, with `.foldover` itself if it was new.
    pub fn probe_records(&self) -> Result<(), ApplyError> {
        if self.identity()?.is_some() {
            return Ok(());
        }
        let internal = SyncPath::try_from(INTERNAL_DIRECTORY).expect("the internal path is valid");
        let existed = self.root.metadata(&internal).is_ok();
        let mut probe = INTERNAL_DIRECTORY.to_vec();
        probe.extend_from_slice(format!("/probe-{}", uuid::Uuid::new_v4().simple()).as_bytes());
        let probe = SyncPath::try_from(probe).expect("probe names are valid");

        // Clean up however far the probe got; only its result counts.
        let written = self
            .root
            .create_private_file(&probe, b"", Durability::Standard);
        if written.is_ok()
            && let Ok(metadata) = self.root.metadata(&probe)
        {
            let _ = self.root.delete(&probe, &metadata, Durability::Standard);
        }
        if !existed && let Ok(metadata) = self.root.metadata(&internal) {
            let _ = self
                .root
                .remove_directory(&internal, &metadata, Durability::Standard);
        }
        written.map_err(|source| ApplyError::Write {
            path: probe,
            source,
        })
    }

    /// The identity this root carries, or `None` before its first sync.
    /// An empty file is a placeholder an interrupted first write left on a
    /// volume without exclusive renames (ADR 0019): no identity yet.
    pub fn identity(&self) -> Result<Option<EndpointId>, ScanError> {
        let path = identity_path();
        let Some(content) = at(
            &Some(path.clone()),
            self.root.read_private_file(&path, IDENTITY_FILE_LIMIT),
        )?
        else {
            return Ok(None);
        };
        if content.is_empty() {
            return Ok(None);
        }
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
        let content = format!("{identity}\n");
        match self
            .root
            .create_private_file(&path, content.as_bytes(), durability)
        {
            Ok(()) => Ok(identity),
            // Another writer was first; its identity stands. An empty
            // placeholder is no identity: replace it.
            Err(WriteError::Exists) => match self.identity()? {
                Some(existing) => Ok(existing),
                None => {
                    self.root
                        .replace_private_file(&path, content.as_bytes(), durability)
                        .map_err(|source| ApplyError::Write {
                            path: path.clone(),
                            source,
                        })?;
                    Ok(identity)
                }
            },
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

    /// Review of #105: on volumes without exclusive renames, an agent that
    /// dies while creating the identity leaves it empty. It counts as
    /// missing and is written again, rather than wedging every run.
    #[test]
    fn an_empty_identity_file_counts_as_missing() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();
        fs::create_dir(directory.path().join(".foldover")).unwrap();
        fs::write(directory.path().join(".foldover/id"), b"").unwrap();
        assert_eq!(endpoint.identity().unwrap(), None);
        let identity = endpoint.ensure_identity(Durability::Standard).unwrap();
        assert_eq!(endpoint.identity().unwrap(), Some(identity));
    }

    #[test]
    fn probing_for_records_leaves_the_folder_as_it_was() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();
        let names = || {
            let mut names: Vec<_> = fs::read_dir(directory.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            names
        };
        endpoint.probe_records().unwrap();
        assert!(names().is_empty());

        // A `.foldover` already there keeps what it holds.
        fs::create_dir(directory.path().join(".foldover")).unwrap();
        fs::write(directory.path().join(".foldover/lock"), b"held").unwrap();
        endpoint.probe_records().unwrap();
        assert_eq!(
            fs::read_dir(directory.path().join(".foldover"))
                .unwrap()
                .count(),
            1
        );

        // A name `.foldover` cannot become a folder refuses records.
        fs::remove_dir_all(directory.path().join(".foldover")).unwrap();
        fs::write(directory.path().join(".foldover"), b"").unwrap();
        assert!(endpoint.probe_records().is_err());
        assert_eq!(names(), [".foldover"]);
    }
}
