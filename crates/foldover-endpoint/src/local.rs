use std::io;
use std::path::Path;

use foldover_fs::{DirectoryEntry, DirectoryScope, EntryMetadata, FileIdentity, NodeKind, Root};
use foldover_types::{InvalidSyncPath, SyncPath};

const INTERNAL_DIRECTORY: &[u8] = b".foldover";
const TEMPORARY_PREFIX: &[u8] = b".foldover-tmp-";
const FINDER_STATE: &[u8] = b".DS_Store";

/// A held local root. Metadata inventory does not establish volume UUIDs,
/// content equality, metadata fidelity or readiness to execute a sync.
pub struct LocalEndpoint {
    root: Root,
    identity: FileIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanControl {
    Continue,
    Cancel,
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("Metadata scan failed at {path:?}: {source}")]
    Io {
        path: Option<SyncPath>,
        #[source]
        source: io::Error,
    },
    #[error("Invalid directory entry: {0}")]
    InvalidPath(#[from] InvalidSyncPath),
    #[error("Metadata scan cancelled; partial rows are not a complete inventory")]
    Cancelled,
    #[error("Entry changed during metadata scan: {0:?}")]
    Changed(Option<SyncPath>),
    #[error("Nested filesystem requires its own endpoint: {0:?}")]
    NestedFilesystem(SyncPath),
    #[error("Directory cycle during metadata scan: {0:?}")]
    DirectoryCycle(SyncPath),
    #[error("Unsupported entry type: {0:?}")]
    UnsupportedEntry(SyncPath),
    #[error("AppleDouble sidecar needs metadata classification before scanning: {0:?}")]
    UnclassifiedSidecar(SyncPath),
}

/// Exact path and observed attributes. A link target is data, never a scan root.
#[derive(Debug)]
pub struct LocalEntry {
    path: SyncPath,
    metadata: EntryMetadata,
    link_target: Option<Vec<u8>>,
}

impl LocalEntry {
    pub fn path(&self) -> &SyncPath {
        &self.path
    }

    pub fn metadata(&self) -> &EntryMetadata {
        &self.metadata
    }

    pub fn link_target(&self) -> Option<&[u8]> {
        self.link_target.as_deref()
    }
}

/// Returned only after traversal finishes. Even a completed metadata inventory
/// needs content/metadata reads, watchers and freshness checks before planning.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScanStats {
    entries: usize,
    skipped: usize,
}

impl ScanStats {
    pub fn entries(self) -> usize {
        self.entries
    }

    pub fn skipped(self) -> usize {
        self.skipped
    }
}

struct Directory {
    root: Root,
    path: Option<SyncPath>,
    before: EntryMetadata,
    entries: Box<dyn Iterator<Item = io::Result<DirectoryEntry>>>,
}

impl Directory {
    fn open(root: Root, path: Option<SyncPath>) -> Result<Self, ScanError> {
        let before = at(&path, root.root_metadata())?;
        let entries = Box::new(at(&path, root.entries(DirectoryScope::Root))?);
        Ok(Self {
            root,
            path,
            before,
            entries,
        })
    }
}

impl LocalEndpoint {
    pub fn open(path: &Path) -> Result<Self, ScanError> {
        let root = at(&None, Root::open(path))?;
        let identity = at(&None, root.root_metadata())?.identity();
        Ok(Self { root, identity })
    }

    /// Stream depth-first metadata rows without opening file data or buffering a
    /// whole directory. Enumeration order is filesystem-defined; plans must sort.
    /// The sink must discard partial rows when this returns an error. Cancellation
    /// is checked between rows; it does not interrupt a blocked filesystem call.
    pub fn scan_metadata(
        &self,
        sink: &mut impl FnMut(LocalEntry) -> ScanControl,
    ) -> Result<ScanStats, ScanError> {
        let root = at(&None, self.root.try_clone())?;
        if at(&None, root.root_metadata())?.identity() != self.identity {
            return Err(ScanError::Changed(None));
        }
        let mut directories = vec![Directory::open(root, None)?];
        let mut stats = ScanStats::default();

        while let Some(directory) = directories.last_mut() {
            let Some(entry) = directory.entries.next() else {
                if at(&directory.path, directory.root.root_metadata())? != directory.before {
                    return Err(ScanError::Changed(directory.path.clone()));
                }
                let finished = directories.pop().expect("the current directory exists");
                if let Some(parent) = directories.last() {
                    let path = finished.path.as_ref().expect("a descendant has a path");
                    let name = SyncPath::try_from(path.file_name())?;
                    if at(&finished.path, parent.root.metadata(&name))? != finished.before {
                        return Err(ScanError::Changed(finished.path));
                    }
                }
                continue;
            };
            let entry = at(&directory.path, entry)?;

            if protected(entry.name()) {
                stats.skipped += 1;
                continue;
            }
            let path = child_path(directory.path.as_ref(), entry.name())?;
            if entry.name().starts_with(b"._") {
                return Err(ScanError::UnclassifiedSidecar(path));
            }
            let at_path = Some(path.clone());
            let name = SyncPath::try_from(entry.name())?;
            let metadata = entry.metadata().clone();
            if metadata.identity().device() != self.identity.device() {
                return Err(ScanError::NestedFilesystem(path));
            }
            if metadata.kind() == NodeKind::Other {
                return Err(ScanError::UnsupportedEntry(path));
            }
            let link_target = if metadata.kind() == NodeKind::Symlink {
                Some(at(&at_path, directory.root.read_link(&name))?)
            } else {
                None
            };
            if at(&at_path, directory.root.metadata(&name))? != metadata {
                return Err(ScanError::Changed(at_path));
            }
            if sink(LocalEntry {
                path: path.clone(),
                metadata: metadata.clone(),
                link_target,
            }) == ScanControl::Cancel
            {
                return Err(ScanError::Cancelled);
            }
            stats.entries += 1;

            if metadata.kind() != NodeKind::Directory {
                continue;
            }
            let child = at(&at_path, directory.root.open_directory(&name))?;
            let child = Directory::open(child, at_path.clone())?;
            if child.before != metadata {
                return Err(ScanError::Changed(at_path));
            }
            if directories
                .iter()
                .any(|ancestor| ancestor.before.identity() == child.before.identity())
            {
                return Err(ScanError::DirectoryCycle(path));
            }
            directories.push(child);
        }
        Ok(stats)
    }
}

fn child_path(parent: Option<&SyncPath>, name: &[u8]) -> Result<SyncPath, InvalidSyncPath> {
    let mut bytes = Vec::new();
    if let Some(parent) = parent {
        bytes.extend_from_slice(parent.as_bytes());
        bytes.push(b'/');
    }
    bytes.extend_from_slice(name);
    SyncPath::try_from(bytes)
}

fn protected(name: &[u8]) -> bool {
    // Reserve ASCII aliases too: another endpoint may be case-insensitive.
    name.eq_ignore_ascii_case(INTERNAL_DIRECTORY)
        || name.eq_ignore_ascii_case(FINDER_STATE)
        || name
            .get(..TEMPORARY_PREFIX.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(TEMPORARY_PREFIX))
}

fn at<T>(path: &Option<SyncPath>, result: io::Result<T>) -> Result<T, ScanError> {
    result.map_err(|source| ScanError::Io {
        path: path.clone(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    #[test]
    fn an_empty_root_completes_without_inventing_entries() {
        let temporary = tempfile::tempdir().unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        let stats = endpoint
            .scan_metadata(&mut |_| panic!("an empty root has no rows"))
            .unwrap();
        assert_eq!(stats.entries(), 0);
        assert_eq!(stats.skipped(), 0);
    }

    #[test]
    fn inventories_without_internal_paths_or_link_traversal() {
        let temporary = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), b"outside").unwrap();
        fs::create_dir_all(temporary.path().join("tree/empty")).unwrap();
        fs::create_dir_all(temporary.path().join(".foldover/archive")).unwrap();
        fs::write(temporary.path().join(".foldover/archive/old"), b"archive").unwrap();
        fs::create_dir_all(temporary.path().join("tree/.FOLDOVER")).unwrap();
        fs::write(temporary.path().join("tree/.FOLDOVER/id"), b"internal").unwrap();
        fs::write(temporary.path().join(".foldover-tmp-write"), b"staged").unwrap();
        fs::write(temporary.path().join("tree/.DS_Store"), b"finder").unwrap();
        fs::write(temporary.path().join("z-last"), b"last").unwrap();
        fs::write(temporary.path().join("tree/file"), b"content").unwrap();
        symlink(outside.path(), temporary.path().join("a-link")).unwrap();
        symlink("../missing", temporary.path().join("tree/dangling")).unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        let mut entries = Vec::new();
        let stats = endpoint
            .scan_metadata(&mut |entry| {
                entries.push(entry);
                ScanControl::Continue
            })
            .unwrap();
        entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        let paths = entries
            .iter()
            .map(|entry| entry.path().as_bytes())
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            [
                b"a-link".as_slice(),
                b"tree",
                b"tree/dangling",
                b"tree/empty",
                b"tree/file",
                b"z-last",
            ]
        );
        assert_eq!(stats.entries(), entries.len());
        assert_eq!(stats.skipped(), 4);
        assert_eq!(
            entries[0].link_target(),
            Some(outside.path().as_os_str().as_bytes())
        );
        assert_eq!(entries[2].link_target(), Some(b"../missing".as_slice()));
        assert_eq!(entries[4].metadata().kind(), NodeKind::File);
        assert_eq!(entries[4].metadata().size(), 7);
        assert_eq!(entries[4].link_target(), None);
        let mut repeated = Vec::new();
        assert_eq!(
            endpoint
                .scan_metadata(&mut |entry| {
                    repeated.push(entry.path);
                    ScanControl::Continue
                })
                .unwrap(),
            stats,
        );
        repeated.sort_unstable();
        assert_eq!(
            repeated.iter().map(SyncPath::as_bytes).collect::<Vec<_>>(),
            paths
        );
    }

    #[test]
    fn cancellation_never_reports_a_complete_inventory() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("a"), b"a").unwrap();
        fs::write(temporary.path().join("b"), b"b").unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        let mut count = 0;
        let result = endpoint.scan_metadata(&mut |_| {
            count += 1;
            ScanControl::Cancel
        });
        assert!(matches!(result, Err(ScanError::Cancelled)));
        assert_eq!(count, 1);
    }

    #[test]
    fn refuses_a_directory_changed_to_an_outside_link_after_its_row() {
        let temporary = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(temporary.path().join("directory")).unwrap();
        fs::write(outside.path().join("secret"), b"outside").unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        let mut paths = Vec::new();
        let result = endpoint.scan_metadata(&mut |entry| {
            paths.push(entry.path);
            fs::remove_dir(temporary.path().join("directory")).unwrap();
            symlink(outside.path(), temporary.path().join("directory")).unwrap();
            ScanControl::Continue
        });
        assert!(matches!(result, Err(ScanError::Io { .. })));
        assert_eq!(
            paths,
            [SyncPath::try_from(b"directory".as_slice()).unwrap()]
        );
    }

    #[test]
    fn disappearing_entries_and_special_items_are_errors_not_empty_trees() {
        let temporary = tempfile::tempdir().unwrap();
        fs::create_dir(temporary.path().join("directory")).unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        let result = endpoint.scan_metadata(&mut |_| {
            fs::remove_dir(temporary.path().join("directory")).unwrap();
            ScanControl::Continue
        });
        assert!(matches!(result, Err(ScanError::Io { .. })));

        let _socket = UnixListener::bind(temporary.path().join("socket")).unwrap();
        assert!(matches!(
            endpoint.scan_metadata(&mut |_| ScanControl::Continue),
            Err(ScanError::UnsupportedEntry(_)),
        ));
    }

    #[test]
    fn sidecars_are_not_misclassified_as_ordinary_file_data() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("._file"), b"metadata").unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        assert!(matches!(
            endpoint.scan_metadata(&mut |_| panic!("sidecar must not be emitted")),
            Err(ScanError::UnclassifiedSidecar(_)),
        ));
    }

    #[test]
    fn detects_directory_changes_even_after_the_last_row() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("only"), b"one").unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        let result = endpoint.scan_metadata(&mut |_| {
            fs::write(temporary.path().join("new"), b"new").unwrap();
            ScanControl::Continue
        });
        assert!(matches!(result, Err(ScanError::Changed(None))));
    }

    #[test]
    fn held_endpoint_does_not_switch_to_a_replacement_root() {
        let temporary = tempfile::tempdir().unwrap();
        let selected = temporary.path().join("selected");
        fs::create_dir(&selected).unwrap();
        fs::write(selected.join("original"), b"one").unwrap();
        let endpoint = LocalEndpoint::open(&selected).unwrap();
        fs::rename(&selected, temporary.path().join("moved")).unwrap();
        fs::create_dir(&selected).unwrap();
        fs::write(selected.join("replacement"), b"two").unwrap();
        let mut paths = Vec::new();
        let stats = endpoint
            .scan_metadata(&mut |entry| {
                paths.push(entry.path);
                ScanControl::Continue
            })
            .unwrap();
        assert_eq!(stats.entries(), 1);
        assert_eq!(paths, [SyncPath::try_from(b"original".as_slice()).unwrap()]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn preserves_non_utf8_path_and_link_target_bytes() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join(OsStr::from_bytes(b"\xff")), b"file").unwrap();
        symlink(OsStr::from_bytes(b"../\xfe"), temporary.path().join("link")).unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        let mut entries = Vec::new();
        endpoint
            .scan_metadata(&mut |entry| {
                entries.push(entry);
                ScanControl::Continue
            })
            .unwrap();
        entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        assert_eq!(entries[0].link_target(), Some(b"../\xfe".as_slice()));
        assert_eq!(entries[1].path().as_bytes(), b"\xff");
    }
}
