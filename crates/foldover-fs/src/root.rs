use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use cap_fs_ext::{DirExt, MetadataExt};
use cap_std::fs::{Dir, Metadata};
use foldover_types::SyncPath;

/// An opened root directory, independent of later changes to its ambient path.
/// This read-only surface does not establish mount availability, volume UUIDs
/// or permission to execute a sync. Endpoint policy still protects internal paths.
#[derive(Debug)]
pub struct Root {
    directory: Dir,
}

pub enum DirectoryScope<'a> {
    Root,
    At(&'a SyncPath),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// Device/inode identity within this observed filesystem session, not a volume UUID.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    pub fn device(self) -> u64 {
        self.device
    }

    pub fn inode(self) -> u64 {
        self.inode
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryMetadata {
    kind: NodeKind,
    size: u64,
    modified: SystemTime,
    identity: FileIdentity,
}

impl EntryMetadata {
    pub fn kind(&self) -> NodeKind {
        self.kind
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn modified(&self) -> SystemTime {
        self.modified
    }

    pub fn identity(&self) -> FileIdentity {
        self.identity
    }

    fn from_cap(metadata: Metadata) -> io::Result<Self> {
        let file_type = metadata.file_type();
        let kind = if file_type.is_symlink() {
            NodeKind::Symlink
        } else if file_type.is_dir() {
            NodeKind::Directory
        } else if file_type.is_file() {
            NodeKind::File
        } else {
            NodeKind::Other
        };
        Ok(Self {
            kind,
            size: metadata.len(),
            modified: metadata.modified()?.into_std(),
            identity: FileIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
        })
    }
}

#[derive(Debug)]
pub struct DirectoryEntry {
    name: Vec<u8>,
    metadata: EntryMetadata,
}

impl DirectoryEntry {
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    pub fn metadata(&self) -> &EntryMetadata {
        &self.metadata
    }
}

impl Root {
    /// Ambient authority is used only to select the root, never for descendants.
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            directory: Dir::open_ambient_dir(path, cap_std::ambient_authority())?,
        })
    }

    pub fn root_metadata(&self) -> io::Result<EntryMetadata> {
        EntryMetadata::from_cap(self.directory.dir_metadata()?)
    }

    /// Does not follow leaf or intermediate symlinks. Errors remain errors,
    /// including disappearing entries; callers must not turn them into empty scans.
    pub fn metadata(&self, path: &SyncPath) -> io::Result<EntryMetadata> {
        let (parent, name) = self.parent_of(path)?;
        EntryMetadata::from_cap(parent.symlink_metadata(name)?)
    }

    /// Return the link's exact target bytes, including absolute/dangling targets,
    /// without resolving them.
    pub fn read_link(&self, path: &SyncPath) -> io::Result<Vec<u8>> {
        let (parent, name) = self.parent_of(path)?;
        Ok(parent.read_link_contents(name)?.into_os_string().into_vec())
    }

    /// Enumerate one directory. Iteration and metadata failures are explicit.
    /// Filtering and recursion belong to the endpoint, not this driver.
    pub fn entries(
        &self,
        scope: DirectoryScope<'_>,
    ) -> io::Result<impl Iterator<Item = io::Result<DirectoryEntry>> + use<>> {
        let directory = match scope {
            DirectoryScope::Root => self.directory.try_clone()?,
            DirectoryScope::At(path) => self.directory_at(path)?,
        };
        Ok(directory.entries()?.map(|result| {
            let entry = result?;
            Ok(DirectoryEntry {
                name: entry.file_name().into_vec(),
                metadata: EntryMetadata::from_cap(entry.metadata()?)?,
            })
        }))
    }

    fn directory_at(&self, path: &SyncPath) -> io::Result<Dir> {
        let mut directory = self.directory.try_clone()?;
        // One component per open makes no-follow apply to every ancestor.
        for component in path.components() {
            directory = directory.open_dir_nofollow(Path::new(OsStr::from_bytes(component)))?;
        }
        Ok(directory)
    }

    fn parent_of(&self, path: &SyncPath) -> io::Result<(Dir, PathBuf)> {
        let parent = match path.parent() {
            Some(parent) => self.directory_at(&parent)?,
            None => self.directory.try_clone()?,
        };
        let name = PathBuf::from(OsStr::from_bytes(path.file_name()));
        Ok((parent, name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::symlink;

    fn path(bytes: &[u8]) -> SyncPath {
        SyncPath::try_from(bytes).unwrap()
    }

    #[test]
    fn enumerates_metadata_without_following_leaf_links() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("file"), b"payload").unwrap();
        fs::create_dir(temporary.path().join("directory")).unwrap();
        symlink("file", temporary.path().join("link")).unwrap();
        let root = Root::open(temporary.path()).unwrap();
        let entries = root
            .entries(DirectoryScope::Root)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.name().to_vec(), entry.metadata().kind())
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(entries.get(b"file".as_slice()), Some(&NodeKind::File));
        assert_eq!(
            entries.get(b"directory".as_slice()),
            Some(&NodeKind::Directory)
        );
        assert_eq!(entries.get(b"link".as_slice()), Some(&NodeKind::Symlink));
        assert_eq!(root.metadata(&path(b"file")).unwrap().size(), 7);
        assert_eq!(
            root.metadata(&path(b"link")).unwrap().kind(),
            NodeKind::Symlink
        );
    }

    #[test]
    fn rejects_intermediate_and_directory_symlinks() {
        let temporary = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), b"outside").unwrap();
        symlink(outside.path(), temporary.path().join("escape")).unwrap();
        fs::create_dir(temporary.path().join("inside")).unwrap();
        symlink("inside", temporary.path().join("alias")).unwrap();
        let root = Root::open(temporary.path()).unwrap();

        assert!(root.metadata(&path(b"escape/secret")).is_err());
        assert!(root.entries(DirectoryScope::At(&path(b"escape"))).is_err());
        assert!(root.entries(DirectoryScope::At(&path(b"alias"))).is_err());
        assert_eq!(
            root.read_link(&path(b"escape")).unwrap(),
            outside.path().as_os_str().as_bytes()
        );
    }

    #[test]
    fn preserves_dangling_link_targets_and_missing_entry_errors() {
        let temporary = tempfile::tempdir().unwrap();
        symlink("../missing", temporary.path().join("dangling")).unwrap();
        let root = Root::open(temporary.path()).unwrap();
        assert_eq!(root.read_link(&path(b"dangling")).unwrap(), b"../missing");
        assert_eq!(
            root.metadata(&path(b"missing")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn held_root_does_not_switch_to_a_replacement_ambient_path() {
        let temporary = tempfile::tempdir().unwrap();
        let original = temporary.path().join("root");
        fs::create_dir(&original).unwrap();
        fs::write(original.join("file"), b"old").unwrap();
        let root = Root::open(&original).unwrap();
        let identity = root.root_metadata().unwrap().identity();
        fs::rename(&original, temporary.path().join("moved")).unwrap();
        fs::create_dir(&original).unwrap();
        fs::write(original.join("file"), b"replacement").unwrap();

        assert_eq!(root.root_metadata().unwrap().identity(), identity);
        assert_eq!(root.metadata(&path(b"file")).unwrap().size(), 3);
        assert_ne!(
            Root::open(&original)
                .unwrap()
                .root_metadata()
                .unwrap()
                .identity(),
            identity
        );
    }

    #[test]
    fn directory_iteration_owns_its_handle_after_root_drop() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("file"), b"payload").unwrap();
        let mut entries = {
            let root = Root::open(temporary.path()).unwrap();
            root.entries(DirectoryScope::Root).unwrap()
        };
        let entry = entries.next().unwrap().unwrap();
        assert_eq!(entry.name(), b"file");
        assert_eq!(entry.metadata().kind(), NodeKind::File);
        assert!(entries.next().is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn preserves_non_utf8_names() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join(OsStr::from_bytes(b"\xff")), b"data").unwrap();
        let root = Root::open(temporary.path()).unwrap();
        let entry = root
            .entries(DirectoryScope::Root)
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(entry.name(), b"\xff");
        assert_eq!(root.metadata(&path(b"\xff")).unwrap().size(), 4);
    }
}
