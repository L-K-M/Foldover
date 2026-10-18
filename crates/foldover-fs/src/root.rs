use std::collections::HashSet;
use std::ffi::OsStr;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use cap_fs_ext::DirExt;
use cap_std::fs::{Dir, Metadata, MetadataExt, ReadDir};
use foldover_types::SyncPath;
use nix::fcntl::AtFlags;
use nix::sys::stat::{FileStat, SFlag, fstat, fstatat};

use crate::{FilesystemFeatures, prevent_materialization};

mod content;
pub use content::HashError;

mod extended;
pub use extended::{AccessControl, ExtendedAttribute, ExtendedMetadata, MetadataReadError};

const MAX_ROOT_ANCESTORS: usize = 4096;
// Darwin sys/stat.h defines this synthetic, read-only flag; libc omits it.
#[cfg(target_os = "macos")]
const SF_DATALESS: u32 = 0x4000_0000;
// Owner-settable flags that describe the entry itself, not its storage:
// compression, document tracking and data-vault state stay out.
#[cfg(target_os = "macos")]
const USER_FLAGS: u32 =
    libc::UF_NODUMP | libc::UF_IMMUTABLE | libc::UF_APPEND | libc::UF_OPAQUE | libc::UF_HIDDEN;

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
pub enum RootRelation {
    Same,
    Contains,
    Within,
    Disjoint,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// An observed dataless flag, not a guarantee against later eviction or reads
/// that materialize data. The latter requires an enforced host I/O policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataResidency {
    Materialized,
    Dataless,
    NotReported,
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

    fn from_cap(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryMetadata {
    kind: NodeKind,
    size: u64,
    modified: SystemTime,
    changed: (i64, i64),
    identity: FileIdentity,
    residency: DataResidency,
    permissions: u32,
    owner: u32,
    group: u32,
    links: u64,
    flags: Option<u32>,
    created: Option<SystemTime>,
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

    pub fn data_residency(&self) -> DataResidency {
        self.residency
    }

    pub fn permissions(&self) -> u32 {
        self.permissions
    }

    pub fn owner(&self) -> u32 {
        self.owner
    }

    pub fn group(&self) -> u32 {
        self.group
    }

    pub fn links(&self) -> u64 {
        self.links
    }

    /// Native BSD flags; an unreported field is not an empty flag set.
    pub fn flags(&self) -> Option<u32> {
        self.flags
    }

    pub fn created(&self) -> Option<SystemTime> {
        self.created
    }

    /// Flags an owner sets on the entry itself; `None` when flags are unreported.
    pub fn user_flags(&self) -> Option<u32> {
        self.flags.map(user_flags)
    }

    /// Transparently compressed: compression xattrs then hold storage, not data.
    pub fn is_compressed(&self) -> bool {
        self.flags.is_some_and(compressed)
    }

    fn from_file(file: &impl AsFd) -> io::Result<Self> {
        Self::from_stat(fstat(file)?)
    }

    fn at(parent: &Dir, name: &Path) -> io::Result<Self> {
        Self::from_stat(fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW)?)
    }

    fn from_stat(stat: FileStat) -> io::Result<Self> {
        // Match std metadata's sign extension of Darwin's 32-bit dev_t.
        #[allow(clippy::unnecessary_cast)]
        let device = stat.st_dev as u64;
        #[allow(clippy::unnecessary_cast)]
        let permissions = (stat.st_mode as u32) & (nix::sys::stat::Mode::all().bits() as u32);
        #[allow(clippy::unnecessary_cast)]
        let links = stat.st_nlink as u64;
        let kind = match SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT {
            SFlag::S_IFLNK => NodeKind::Symlink,
            SFlag::S_IFDIR => NodeKind::Directory,
            SFlag::S_IFREG => NodeKind::File,
            _ => NodeKind::Other,
        };
        Ok(Self {
            kind,
            size: u64::try_from(stat.st_size).map_err(|_| invalid_metadata())?,
            modified: modified_time(stat.st_mtime, stat.st_mtime_nsec)?,
            changed: (stat.st_ctime, stat.st_ctime_nsec),
            identity: FileIdentity {
                device,
                inode: stat.st_ino,
            },
            residency: data_residency(&stat),
            permissions,
            owner: stat.st_uid,
            group: stat.st_gid,
            links,
            flags: file_flags(&stat),
            created: creation_time(&stat)?,
        })
    }
}

fn modified_time(seconds: i64, nanoseconds: i64) -> io::Result<SystemTime> {
    let nanoseconds = u32::try_from(nanoseconds).map_err(|_| invalid_metadata())?;
    if u128::from(nanoseconds) >= Duration::from_secs(1).as_nanos() {
        return Err(invalid_metadata());
    }
    let duration = Duration::from_secs(seconds.unsigned_abs());
    let whole = if seconds < 0 {
        SystemTime::UNIX_EPOCH.checked_sub(duration)
    } else {
        SystemTime::UNIX_EPOCH.checked_add(duration)
    };
    whole
        .and_then(|time| time.checked_add(Duration::from_nanos(u64::from(nanoseconds))))
        .ok_or_else(invalid_metadata)
}

fn invalid_metadata() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Invalid filesystem metadata")
}

#[cfg(target_os = "macos")]
fn file_flags(stat: &FileStat) -> Option<u32> {
    Some(stat.st_flags)
}

#[cfg(not(target_os = "macos"))]
fn file_flags(_: &FileStat) -> Option<u32> {
    None
}

#[cfg(target_os = "macos")]
fn user_flags(flags: u32) -> u32 {
    flags & USER_FLAGS
}

// Only Darwin reports flags, so these are unreachable elsewhere.
#[cfg(not(target_os = "macos"))]
fn user_flags(flags: u32) -> u32 {
    flags
}

#[cfg(target_os = "macos")]
fn compressed(flags: u32) -> bool {
    flags & libc::UF_COMPRESSED != 0
}

#[cfg(not(target_os = "macos"))]
fn compressed(_: u32) -> bool {
    false
}

#[cfg(target_os = "macos")]
fn creation_time(stat: &FileStat) -> io::Result<Option<SystemTime>> {
    modified_time(stat.st_birthtime, stat.st_birthtime_nsec).map(Some)
}

#[cfg(not(target_os = "macos"))]
fn creation_time(_: &FileStat) -> io::Result<Option<SystemTime>> {
    Ok(None)
}

#[cfg(target_os = "macos")]
fn data_residency(stat: &FileStat) -> DataResidency {
    if stat.st_flags & SF_DATALESS != 0 {
        return DataResidency::Dataless;
    }
    DataResidency::Materialized
}

#[cfg(not(target_os = "macos"))]
fn data_residency(_: &FileStat) -> DataResidency {
    DataResidency::NotReported
}

#[derive(Debug)]
pub struct DirectoryEntry {
    name: Vec<u8>,
    metadata: EntryMetadata,
}

struct DirectoryEntries {
    directory: Dir,
    entries: Option<ReadDir>,
}

impl Iterator for DirectoryEntries {
    type Item = io::Result<DirectoryEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        self.entries.as_ref()?;
        // Iterators can move between threads after creation. Enforce policy on
        // the thread that actually enumerates, before readdir or metadata I/O.
        if let Err(error) = prevent_materialization() {
            self.entries = None;
            return Some(Err(error));
        }
        let Some(entry) = self.entries.as_mut()?.next() else {
            self.entries = None;
            return None;
        };
        Some(entry.and_then(|entry| {
            let name = entry.file_name();
            Ok(DirectoryEntry {
                metadata: EntryMetadata::at(&self.directory, Path::new(&name))?,
                name: name.into_vec(),
            })
        }))
    }
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
        prevent_materialization()?;
        Ok(Self {
            directory: Dir::open_ambient_dir(path, cap_std::ambient_authority())?,
        })
    }

    pub fn root_metadata(&self) -> io::Result<EntryMetadata> {
        prevent_materialization()?;
        EntryMetadata::from_file(&self.directory)
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            directory: self.directory.try_clone()?,
        })
    }

    /// Observe the held root's filesystem UUID without following its ambient
    /// path. Missing platform/filesystem support is not a persistent identity.
    /// Mount continuity and endpoint identity still require separate gates.
    pub fn volume_uuid(&self) -> io::Result<Option<uuid::Uuid>> {
        prevent_materialization()?;
        #[cfg(target_os = "macos")]
        {
            self.inspect_volume(crate::volume::uuid)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(None)
        }
    }

    /// Read the held volume's declarations. Missing declarations are unknown;
    /// this does not test copy/rename fidelity or authorize filesystem writes.
    pub fn filesystem_features(&self) -> io::Result<FilesystemFeatures> {
        prevent_materialization()?;
        #[cfg(target_os = "macos")]
        {
            self.inspect_volume(crate::volume::filesystem_features)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(FilesystemFeatures::default())
        }
    }

    #[cfg(target_os = "macos")]
    fn inspect_volume<T>(&self, inspect: impl FnOnce(&Dir) -> io::Result<T>) -> io::Result<T> {
        let device = crate::volume::real_device(&self.directory)?;
        let mut directory = self.directory.try_clone()?;
        let mut visited = HashSet::new();
        for _ in 0..MAX_ROOT_ANCESTORS {
            let identity = FileIdentity::from_cap(&directory.dir_metadata()?);
            if !visited.insert(identity) {
                return Err(io::Error::other(
                    "Volume ancestry changed or contains a cycle",
                ));
            }
            let parent = directory.open_parent_dir(cap_std::ambient_authority())?;
            let parent_device = crate::volume::real_device(&parent)?;
            let parent_identity = FileIdentity::from_cap(&parent.dir_metadata()?);
            if parent_device != device || parent_identity == identity {
                // Volume attributes require the physical mount root. Real
                // device IDs prevent APFS volume groups from hiding its edge.
                let observation = inspect(&directory)?;
                if crate::volume::real_device(&self.directory)? != device {
                    return Err(io::Error::other(
                        "Root volume changed during identity lookup",
                    ));
                }
                return Ok(observation);
            }
            directory = parent;
        }
        Err(io::Error::other(
            "Volume ancestry exceeds the verification limit",
        ))
    }

    /// Compare held directories, including selected symlink/case aliases.
    /// This observes current ancestry; callers must recheck before executing.
    pub fn relation_to(&self, other: &Self) -> io::Result<RootRelation> {
        let left = self.root_metadata()?.identity();
        let right = other.root_metadata()?.identity();
        if left == right {
            return Ok(RootRelation::Same);
        }
        if other.has_ancestor(left)? {
            return Ok(RootRelation::Contains);
        }
        if self.has_ancestor(right)? {
            return Ok(RootRelation::Within);
        }
        Ok(RootRelation::Disjoint)
    }

    /// Hold a descendant directory without following any symlink component.
    pub fn open_directory(&self, path: &SyncPath) -> io::Result<Self> {
        Ok(Self {
            directory: self.directory_at(path)?,
        })
    }

    /// Does not follow leaf or intermediate symlinks. Errors remain errors,
    /// including disappearing entries; callers must not turn them into empty scans.
    pub fn metadata(&self, path: &SyncPath) -> io::Result<EntryMetadata> {
        let (parent, name) = self.parent_of(path)?;
        EntryMetadata::at(&parent, &name)
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
        prevent_materialization()?;
        let directory = match scope {
            DirectoryScope::Root => self.directory.try_clone()?,
            DirectoryScope::At(path) => self.directory_at(path)?,
        };
        Ok(DirectoryEntries {
            entries: Some(directory.entries()?),
            directory,
        })
    }

    fn has_ancestor(&self, target: FileIdentity) -> io::Result<bool> {
        let mut directory = self.directory.try_clone()?;
        let mut visited = HashSet::new();
        for _ in 0..MAX_ROOT_ANCESTORS {
            let identity = FileIdentity::from_cap(&directory.dir_metadata()?);
            if identity == target {
                return Ok(true);
            }
            if !visited.insert(identity) {
                return Err(io::Error::other(
                    "Directory ancestry changed or contains a cycle",
                ));
            }
            // Ambient authority is confined to ancestor identity probes for root
            // selection. Parent handles never escape into sync traversal.
            let parent = directory.open_parent_dir(cap_std::ambient_authority())?;
            if FileIdentity::from_cap(&parent.dir_metadata()?) == identity {
                return Ok(false);
            }
            directory = parent;
        }
        Err(io::Error::other(
            "Directory ancestry exceeds the verification limit",
        ))
    }

    fn directory_at(&self, path: &SyncPath) -> io::Result<Dir> {
        prevent_materialization()?;
        let mut directory = self.directory.try_clone()?;
        // One component per open makes no-follow apply to every ancestor.
        for component in path.components() {
            directory = directory.open_dir_nofollow(Path::new(OsStr::from_bytes(component)))?;
        }
        Ok(directory)
    }

    fn parent_of(&self, path: &SyncPath) -> io::Result<(Dir, PathBuf)> {
        prevent_materialization()?;
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
    fn descriptor_and_relative_metadata_match_standard_observations() {
        use std::os::unix::fs::MetadataExt;

        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("file");
        fs::write(&file, b"abc").unwrap();
        symlink("file", temporary.path().join("link")).unwrap();
        let root = Root::open(temporary.path()).unwrap();
        for (name, kind) in [
            (b"file".as_slice(), NodeKind::File),
            (b"link", NodeKind::Symlink),
        ] {
            let observed = root.metadata(&path(name)).unwrap();
            let standard =
                fs::symlink_metadata(temporary.path().join(OsStr::from_bytes(name))).unwrap();
            assert_eq!(observed.kind(), kind);
            assert_eq!(observed.size(), standard.len());
            assert_eq!(observed.modified(), standard.modified().unwrap());
            assert_eq!(observed.identity().device(), standard.dev());
            assert_eq!(observed.identity().inode(), standard.ino());
            assert_eq!(observed.owner(), standard.uid());
            assert_eq!(observed.group(), standard.gid());
            assert_eq!(observed.links(), standard.nlink());
            #[cfg(target_os = "macos")]
            {
                use std::os::macos::fs::MetadataExt;
                assert_eq!(observed.flags(), Some(standard.st_flags()));
                assert_eq!(observed.created(), Some(standard.created().unwrap()));
            }
            #[cfg(not(target_os = "macos"))]
            {
                assert_eq!(observed.flags(), None);
                assert_eq!(observed.created(), None);
            }
        }
        let descriptor = fs::File::open(file).unwrap();
        assert_eq!(
            EntryMetadata::from_file(&descriptor).unwrap(),
            root.metadata(&path(b"file")).unwrap()
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            root.root_metadata().unwrap().data_residency(),
            DataResidency::Materialized
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            root.root_metadata().unwrap().data_residency(),
            DataResidency::NotReported
        );
    }

    #[test]
    fn permissions_and_hard_link_topology_are_observed_without_content_changes() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().unwrap();
        let selected = temporary.path().join("file");
        fs::write(&selected, b"unchanged").unwrap();
        let root = Root::open(temporary.path()).unwrap();
        let before = root.metadata(&path(b"file")).unwrap();
        fs::set_permissions(&selected, fs::Permissions::from_mode(0o640)).unwrap();
        fs::hard_link(&selected, temporary.path().join("other")).unwrap();
        let after = root.metadata(&path(b"file")).unwrap();
        assert_eq!(after.permissions(), 0o640);
        assert_eq!(after.links(), before.links() + 1);
        assert_eq!(after.size(), before.size());
        assert_eq!(after.modified(), before.modified());
        assert_eq!(after.identity(), before.identity());
        assert_ne!(before, after);
    }

    #[test]
    fn invalid_stat_values_fail_and_pre_epoch_times_remain_exact() {
        let temporary = tempfile::tempfile().unwrap();
        let stat = fstat(&temporary).unwrap();
        let mut invalid = stat;
        invalid.st_size = -1;
        assert_eq!(
            EntryMetadata::from_stat(invalid).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let mut invalid = stat;
        invalid.st_mtime_nsec = -1;
        assert_eq!(
            EntryMetadata::from_stat(invalid).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let mut invalid = stat;
        invalid.st_mtime_nsec = Duration::from_secs(1).as_nanos() as i64;
        assert_eq!(
            EntryMetadata::from_stat(invalid).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            modified_time(-1, 500_000_000).unwrap(),
            SystemTime::UNIX_EPOCH - Duration::from_millis(500)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_flags_remain_part_of_the_observed_fingerprint() {
        let temporary = tempfile::tempdir().unwrap();
        let selected = temporary.path().join("file");
        fs::write(&selected, b"unchanged").unwrap();
        let root = Root::open(temporary.path()).unwrap();
        let before = root.metadata(&path(b"file")).unwrap();
        assert!(
            std::process::Command::new("/usr/bin/chflags")
                .arg("hidden")
                .arg(&selected)
                .status()
                .unwrap()
                .success()
        );
        let after = root.metadata(&path(b"file")).unwrap();
        assert_eq!(after.flags().unwrap() & libc::UF_HIDDEN, libc::UF_HIDDEN);
        assert_eq!(before.modified(), after.modified());
        assert_ne!(before.flags(), after.flags());
        assert_ne!(before, after);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn dataless_flag_is_captured_in_the_same_stat_as_file_identity() {
        let temporary = tempfile::tempfile().unwrap();
        let mut stat = fstat(&temporary).unwrap();
        stat.st_flags &= !SF_DATALESS;
        let materialized = EntryMetadata::from_stat(stat).unwrap();
        stat.st_flags |= SF_DATALESS;
        let dataless = EntryMetadata::from_stat(stat).unwrap();
        assert_eq!(materialized.data_residency(), DataResidency::Materialized);
        assert_eq!(dataless.data_residency(), DataResidency::Dataless);
        assert_eq!(materialized.identity(), dataless.identity());
        assert_ne!(materialized, dataless);
    }

    #[test]
    fn compares_physical_roots_without_name_prefix_assumptions() {
        let temporary = tempfile::tempdir().unwrap();
        fs::create_dir_all(temporary.path().join("one/child")).unwrap();
        fs::create_dir(temporary.path().join("one-more")).unwrap();
        symlink("one", temporary.path().join("alias")).unwrap();
        let one = Root::open(&temporary.path().join("one")).unwrap();
        let child = Root::open(&temporary.path().join("one/child")).unwrap();
        let alias = Root::open(&temporary.path().join("alias")).unwrap();
        let sibling = Root::open(&temporary.path().join("one-more")).unwrap();
        assert_eq!(one.relation_to(&alias).unwrap(), RootRelation::Same);
        assert_eq!(one.relation_to(&child).unwrap(), RootRelation::Contains);
        assert_eq!(child.relation_to(&alias).unwrap(), RootRelation::Within);
        assert_eq!(one.relation_to(&sibling).unwrap(), RootRelation::Disjoint);
        assert_eq!(sibling.relation_to(&one).unwrap(), RootRelation::Disjoint);
    }

    #[test]
    fn root_relations_follow_held_directories_after_moves() {
        let temporary = tempfile::tempdir().unwrap();
        let selected = temporary.path().join("selected");
        let container = temporary.path().join("container");
        fs::create_dir(&selected).unwrap();
        fs::create_dir(&container).unwrap();
        let held = Root::open(&selected).unwrap();
        let parent = Root::open(&container).unwrap();
        assert_eq!(held.relation_to(&parent).unwrap(), RootRelation::Disjoint);
        fs::rename(&selected, container.join("moved")).unwrap();
        fs::create_dir(&selected).unwrap();
        assert_eq!(held.relation_to(&parent).unwrap(), RootRelation::Within);
        assert_eq!(parent.relation_to(&held).unwrap(), RootRelation::Contains);
        let replacement = Root::open(&selected).unwrap();
        assert_eq!(
            held.relation_to(&replacement).unwrap(),
            RootRelation::Disjoint
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn an_unimplemented_volume_probe_keeps_all_features_unreported() {
        let temporary = tempfile::tempdir().unwrap();
        let root = Root::open(temporary.path()).unwrap();
        assert_eq!(
            root.filesystem_features().unwrap(),
            FilesystemFeatures::default()
        );
    }

    #[test]
    fn case_aliases_use_filesystem_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let selected = temporary.path().join("MixedCase");
        let alias = temporary.path().join("mixedcase");
        fs::create_dir(&selected).unwrap();
        let original = Root::open(&selected).unwrap();
        match Root::open(&alias) {
            Ok(alias) => assert_eq!(original.relation_to(&alias).unwrap(), RootRelation::Same),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&alias).unwrap();
                let alias = Root::open(&alias).unwrap();
                assert_eq!(
                    original.relation_to(&alias).unwrap(),
                    RootRelation::Disjoint
                );
            }
            Err(error) => panic!("Cannot probe case alias: {error}"),
        }
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
