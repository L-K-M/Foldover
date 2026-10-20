//! Writes relative to a held root. Every new entry is completed under a
//! temporary name beside its destination, then placed with one atomic rename:
//!
//! ```text
//! create:   .foldover-tmp-X ──rename(no-replace)──▶ name
//! replace:  .foldover-tmp-X ◀──rename(exchange)──▶ name
//!           (the displaced old entry is left at .foldover-tmp-X for archiving)
//! archive:  name ──rename(no-replace)──▶ .foldover/archive/…/name
//! ```
//!
//! No step deletes file data: replaced and removed entries are moved, never
//! unlinked. Only empty directories and unplaced temporaries are removed.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use cap_fs_ext::DirExt;
use cap_std::fs::Dir;
use foldover_types::{
    Digest, Durability, MetadataPolicy, PairOptions, Propagation, ScanControl, SyncPath,
};
use rustix::fs::{AtFlags, Mode, OFlags, RenameFlags};
use rustix::io::Errno;

use super::{EntryMetadata, HashError, NodeKind, Root};

mod metadata;
use metadata::Destination;

const TEMPORARY_PREFIX: &str = ".foldover-tmp-";
/// Until synced permissions are applied, only the owner can read a copy.
const PRIVATE_FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
const PRIVATE_DIRECTORY_MODE: Mode = Mode::RWXU;
/// Without synced permissions, new entries get the umask's defaults.
const DEFAULT_FILE_MODE: Mode = Mode::RUSR
    .union(Mode::WUSR)
    .union(Mode::RGRP)
    .union(Mode::WGRP)
    .union(Mode::ROTH)
    .union(Mode::WOTH);
const DEFAULT_DIRECTORY_MODE: Mode = Mode::RWXU.union(Mode::RWXG).union(Mode::RWXO);

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("Write failed: {0}")]
    Io(#[from] io::Error),
    #[error("Source read failed: {0}")]
    Source(#[from] HashError),
    #[error("Destination already exists")]
    Exists,
    #[error("Directory is not empty")]
    NotEmpty,
    #[error("Entry changed since it was observed")]
    Changed,
    #[error("Entry type cannot be written this way")]
    UnsupportedEntry,
}

impl From<Errno> for WriteError {
    fn from(error: Errno) -> Self {
        Self::Io(error.into())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Placement {
    /// Fail if anything exists at the destination.
    Create,
    /// Swap atomically; the previous entry becomes `Displaced`.
    Replace,
}

/// A complete entry beside its destination under a name scans skip.
/// Dropping an unplaced entry removes it.
pub struct Staged {
    parent: Dir,
    name: PathBuf,
    placed: bool,
}

impl Staged {
    pub fn metadata(&self) -> io::Result<EntryMetadata> {
        EntryMetadata::at(&self.parent, &self.name)
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if self.placed {
            return;
        }
        // Best effort: a leftover temporary holds only a copy, never the original.
        let _ = rustix::fs::unlinkat(&self.parent, &self.name, AtFlags::empty());
    }
}

/// A staged regular file and the digest of the data written into it.
pub struct StagedFile {
    staged: Staged,
    file: File,
    data: Digest,
}

impl StagedFile {
    pub fn data_digest(&self) -> Digest {
        self.data
    }

    pub fn metadata(&self) -> io::Result<EntryMetadata> {
        EntryMetadata::from_file(&self.file)
    }

    /// The staged file's xattrs and ACL, read through its own descriptor.
    pub fn extended_metadata(
        &self,
        control: &mut impl FnMut(u64) -> ScanControl,
    ) -> Result<super::ExtendedMetadata, super::MetadataReadError> {
        super::extended::capture_file(&self.file, control)
    }

    pub fn into_staged(self) -> Staged {
        self.staged
    }
}

/// Where a producer pushes file data, chunk by chunk.
pub type Chunks<'a> = &'a mut dyn FnMut(&[u8]) -> io::Result<()>;

/// What a copy from a system without extended attributes, such as an SFTP
/// server, can carry besides data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ForeignMetadata {
    pub permissions: u32,
    pub modified: std::time::SystemTime,
}

/// What a copy between two Macs carries besides data, mode and time: a
/// file's resource fork, the extended attributes a pair syncs, and user
/// flags. ACLs do not travel yet.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NativeMetadata {
    /// Names and values, the resource fork among them.
    pub attributes: Vec<(Vec<u8>, Vec<u8>)>,
    pub flags: Option<u32>,
}

/// The entry an exchange moved out of its path. It holds user data and is
/// never removed automatically; archive it.
pub struct Displaced {
    parent: Dir,
    name: PathBuf,
}

impl Displaced {
    pub fn metadata(&self) -> io::Result<EntryMetadata> {
        EntryMetadata::at(&self.parent, &self.name)
    }

    /// Where the entry waits until archived, for recovery records.
    pub fn temporary_name(&self) -> &[u8] {
        self.name.as_os_str().as_bytes()
    }
}

impl Root {
    /// Copy an observed regular file's data, resource fork and the pair's
    /// synced metadata from `source` into a new temporary beside `path`.
    /// The source must stay unchanged.
    pub fn stage_file(
        &self,
        path: &SyncPath,
        source: &Root,
        expected: &EntryMetadata,
        options: &PairOptions,
        control: &mut impl FnMut(u64) -> ScanControl,
    ) -> Result<StagedFile, WriteError> {
        let mut source_file = source.open_observed_file(path, expected, control)?;
        let (parent, _) = self.parent_of(path)?;
        let mode = match options.metadata().permissions() {
            Propagation::Sync => PRIVATE_FILE_MODE,
            Propagation::Ignore => DEFAULT_FILE_MODE,
        };
        let (staged, mut file) = create_temporary_file(parent, mode)?;
        let data = source.stream_observed_file(
            path,
            &mut source_file,
            expected,
            control,
            &mut |chunk| file.write_all(chunk),
        )?;
        // Metadata last: data writes would move the mtime it sets.
        metadata::apply(
            &source_file,
            expected,
            &file,
            Destination::New,
            options.metadata(),
        )?;
        if EntryMetadata::from_file(&source_file)? != *expected {
            return Err(WriteError::Changed);
        }
        flush(&file, options.durability())?;
        Ok(StagedFile { staged, file, data })
    }

    /// Write a new temporary beside `path` from chunks `produce` pushes,
    /// and give it the pair's synced part of `metadata` and of `native`,
    /// when the source carries that.
    pub fn stage_stream(
        &self,
        path: &SyncPath,
        metadata: ForeignMetadata,
        native: Option<&NativeMetadata>,
        options: &PairOptions,
        produce: impl FnOnce(Chunks<'_>) -> io::Result<()>,
    ) -> Result<StagedFile, WriteError> {
        let (parent, _) = self.parent_of(path)?;
        let mode = match options.metadata().permissions() {
            Propagation::Sync => PRIVATE_FILE_MODE,
            Propagation::Ignore => DEFAULT_FILE_MODE,
        };
        let (staged, mut file) = create_temporary_file(parent, mode)?;
        let mut hasher = blake3::Hasher::new();
        produce(&mut |chunk| {
            hasher.update(chunk);
            file.write_all(chunk)
        })?;
        // Attributes before mode and time, as a read-only mode blocks them.
        if let Some(native) = native {
            metadata::apply_native(&file, native, Destination::New, options.metadata())?;
        }
        metadata::apply_foreign(&file, metadata, options.metadata())?;
        flush(&file, options.durability())?;
        Ok(StagedFile {
            staged,
            file,
            data: Digest::from_bytes(*hasher.finalize().as_bytes()),
        })
    }

    /// Give an existing entry the synced part of foreign metadata.
    pub fn update_foreign_metadata(
        &self,
        path: &SyncPath,
        expected: &EntryMetadata,
        metadata: ForeignMetadata,
        native: Option<&NativeMetadata>,
        options: &PairOptions,
    ) -> Result<(), WriteError> {
        let target = self.open_observed(path, expected)?;
        // A source without attributes, such as SFTP, leaves them alone.
        if let Some(native) = native {
            metadata::apply_native(&target, native, Destination::Existing, options.metadata())?;
        }
        metadata::apply_foreign(&target, metadata, options.metadata())?;
        flush(&target, options.durability())?;
        Ok(())
    }

    /// Create a symlink with exactly `target` beside `path`.
    pub fn stage_symlink(&self, path: &SyncPath, target: &[u8]) -> Result<Staged, WriteError> {
        let (parent, _) = self.parent_of(path)?;
        let name = temporary_name();
        rustix::fs::symlinkat(OsStr::from_bytes(target), &parent, &name)?;
        Ok(Staged {
            parent,
            name,
            placed: false,
        })
    }

    /// Move a staged entry to `path` atomically. A replacement returns the
    /// displaced entry, which still needs archiving.
    pub fn place(
        &self,
        mut staged: Staged,
        path: &SyncPath,
        placement: Placement,
        durability: Durability,
    ) -> Result<Option<Displaced>, WriteError> {
        let (parent, name) = self.parent_of(path)?;
        // The held parent must still be this path's parent, or the entry
        // would land wherever that directory has been moved.
        if directory_identity(&parent)? != directory_identity(&staged.parent)? {
            return Err(WriteError::Changed);
        }
        let flags = match placement {
            Placement::Create => RenameFlags::NOREPLACE,
            Placement::Replace => RenameFlags::EXCHANGE,
        };
        match rustix::fs::renameat_with(&staged.parent, &staged.name, &parent, &name, flags) {
            Ok(()) => {}
            Err(Errno::EXIST) => return Err(WriteError::Exists),
            Err(Errno::NOENT) if placement == Placement::Replace => {
                return Err(WriteError::Changed);
            }
            Err(error) => return Err(error.into()),
        }
        // From here the temporary name refers to the displaced entry, if any.
        staged.placed = true;
        sync_directory(&parent, durability)?;
        Ok(match placement {
            Placement::Create => None,
            Placement::Replace => Some(Displaced {
                parent: staged.parent.try_clone()?,
                name: staged.name.clone(),
            }),
        })
    }

    /// Move the entry at `path` to `destination`, creating private parents.
    /// It must still match `expected`; nothing is ever overwritten.
    pub fn archive(
        &self,
        path: &SyncPath,
        expected: &EntryMetadata,
        destination: &SyncPath,
        durability: Durability,
    ) -> Result<(), WriteError> {
        let (parent, name) = self.parent_of(path)?;
        if EntryMetadata::at(&parent, &name)? != *expected {
            return Err(WriteError::Changed);
        }
        self.move_into(&parent, &name, destination, durability)
    }

    pub fn archive_displaced(
        &self,
        displaced: Displaced,
        destination: &SyncPath,
        durability: Durability,
    ) -> Result<(), WriteError> {
        self.move_into(&displaced.parent, &displaced.name, destination, durability)
    }

    /// Create a directory at `path` carrying the pair's synced metadata from
    /// the observed source directory.
    pub fn create_directory(
        &self,
        path: &SyncPath,
        source: &Root,
        expected: &EntryMetadata,
        options: &PairOptions,
    ) -> Result<(), WriteError> {
        let source_directory = source.open_observed(path, expected)?;
        let (parent, name) = self.parent_of(path)?;
        let mode = match options.metadata().permissions() {
            Propagation::Sync => PRIVATE_DIRECTORY_MODE,
            Propagation::Ignore => DEFAULT_DIRECTORY_MODE,
        };
        match rustix::fs::mkdirat(&parent, &name, mode) {
            Ok(()) => {}
            Err(Errno::EXIST) => return Err(WriteError::Exists),
            Err(error) => return Err(error.into()),
        }
        let created = rustix::fs::openat(
            &parent,
            &name,
            open_flags(NodeKind::Directory),
            Mode::empty(),
        )?;
        metadata::apply(
            &source_directory,
            expected,
            &created,
            Destination::New,
            options.metadata(),
        )?;
        sync_directory(&parent, options.durability())?;
        Ok(())
    }

    /// Create a folder for an entry from another system, with its mode when
    /// modes are synced.
    pub fn create_foreign_directory(
        &self,
        path: &SyncPath,
        metadata: ForeignMetadata,
        native: Option<&NativeMetadata>,
        options: &PairOptions,
    ) -> Result<(), WriteError> {
        let (parent, name) = self.parent_of(path)?;
        let mode = match options.metadata().permissions() {
            Propagation::Sync => Mode::from_raw_mode((metadata.permissions & 0o777) as _),
            Propagation::Ignore => DEFAULT_DIRECTORY_MODE,
        };
        match rustix::fs::mkdirat(&parent, &name, mode) {
            Ok(()) => {}
            Err(Errno::EXIST) => return Err(WriteError::Exists),
            Err(error) => return Err(error.into()),
        }
        if let Some(native) = native.filter(|native| **native != NativeMetadata::default()) {
            let created = rustix::fs::openat(
                &parent,
                &name,
                open_flags(NodeKind::Directory),
                Mode::empty(),
            )?;
            metadata::apply_native(&created, native, Destination::New, options.metadata())?;
        }
        sync_directory(&parent, options.durability())?;
        Ok(())
    }

    /// What a copy to another Mac carries of `path`, which must still be
    /// `expected`: see `NativeMetadata`.
    pub fn native_metadata(
        &self,
        path: &SyncPath,
        expected: &EntryMetadata,
        policy: MetadataPolicy,
    ) -> Result<NativeMetadata, WriteError> {
        let file = self.open_observed(path, expected)?;
        Ok(metadata::native(&file, expected, policy)?)
    }

    /// Replace the synced metadata of the file or directory at `path` with
    /// the source's, leaving content alone. Both must still match their
    /// observations.
    pub fn update_metadata(
        &self,
        path: &SyncPath,
        expected: &EntryMetadata,
        source: &Root,
        source_expected: &EntryMetadata,
        options: &PairOptions,
    ) -> Result<(), WriteError> {
        if expected.kind() != source_expected.kind() {
            return Err(WriteError::UnsupportedEntry);
        }
        let source_entry = source.open_observed(path, source_expected)?;
        let target = self.open_observed(path, expected)?;
        metadata::apply(
            &source_entry,
            source_expected,
            &target,
            Destination::Existing,
            options.metadata(),
        )?;
        flush(&target, options.durability())?;
        Ok(())
    }

    /// Remove an empty directory. New children make this fail.
    pub fn remove_directory(
        &self,
        path: &SyncPath,
        expected: &EntryMetadata,
        durability: Durability,
    ) -> Result<(), WriteError> {
        let (parent, name) = self.parent_of(path)?;
        let current = EntryMetadata::at(&parent, &name)?;
        if current.kind() != NodeKind::Directory || current.identity() != expected.identity() {
            return Err(WriteError::Changed);
        }
        match rustix::fs::unlinkat(&parent, &name, AtFlags::REMOVEDIR) {
            Ok(()) => {}
            Err(Errno::NOTEMPTY | Errno::EXIST) => return Err(WriteError::NotEmpty),
            Err(error) => return Err(error.into()),
        }
        sync_directory(&parent, durability)?;
        Ok(())
    }

    fn open_observed(
        &self,
        path: &SyncPath,
        expected: &EntryMetadata,
    ) -> Result<rustix::fd::OwnedFd, WriteError> {
        if !matches!(expected.kind(), NodeKind::File | NodeKind::Directory) {
            return Err(WriteError::UnsupportedEntry);
        }
        let (parent, name) = self.parent_of(path)?;
        let file = rustix::fs::openat(&parent, &name, open_flags(expected.kind()), Mode::empty())?;
        if EntryMetadata::from_file(&file)? != *expected {
            return Err(WriteError::Changed);
        }
        Ok(file)
    }

    fn move_into(
        &self,
        parent: &Dir,
        name: &Path,
        destination: &SyncPath,
        durability: Durability,
    ) -> Result<(), WriteError> {
        let (archive, archive_name) = self.create_parents(destination, Parents::Private)?;
        match rustix::fs::renameat_with(
            parent,
            name,
            &archive,
            &archive_name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {}
            Err(Errno::EXIST) => return Err(WriteError::Exists),
            Err(error) => return Err(error.into()),
        }
        sync_directory(&archive, durability)?;
        sync_directory(parent, durability)?;
        Ok(())
    }

    /// Permanently delete `path` and everything below it, never following
    /// links. Only archive retention may call this.
    pub fn remove_tree(&self, path: &SyncPath) -> Result<(), WriteError> {
        let (parent, name) = self.parent_of(path)?;
        remove_entry(&parent, &name)?;
        Ok(())
    }

    /// Move an archived entry back to `destination`, creating missing
    /// folders. Nothing at the destination is ever replaced.
    pub fn restore(
        &self,
        archived: &SyncPath,
        destination: &SyncPath,
        durability: Durability,
    ) -> Result<(), WriteError> {
        let (parent, name) = self.parent_of(archived)?;
        let (target, target_name) = self.create_parents(destination, Parents::Default)?;
        match rustix::fs::renameat_with(
            &parent,
            &name,
            &target,
            &target_name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {}
            Err(Errno::EXIST) => return Err(WriteError::Exists),
            Err(error) => return Err(error.into()),
        }
        sync_directory(&target, durability)?;
        sync_directory(&parent, durability)?;
        Ok(())
    }

    /// Write a small private file that must not exist yet, creating private
    /// parents. The content appears complete or not at all.
    pub fn create_private_file(
        &self,
        path: &SyncPath,
        content: &[u8],
        durability: Durability,
    ) -> Result<(), WriteError> {
        let (parent, name) = self.create_parents(path, Parents::Private)?;
        let (mut staged, mut file) = create_temporary_file(parent, PRIVATE_FILE_MODE)?;
        file.write_all(content)?;
        flush(&file, durability)?;
        match rustix::fs::renameat_with(
            &staged.parent,
            &staged.name,
            &staged.parent,
            &name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {}
            Err(Errno::EXIST) => return Err(WriteError::Exists),
            Err(error) => return Err(error.into()),
        }
        staged.placed = true;
        sync_directory(&staged.parent, durability)?;
        Ok(())
    }

    /// Read a small regular file without following links; `None` if absent.
    pub fn read_private_file(&self, path: &SyncPath, limit: u64) -> io::Result<Option<Vec<u8>>> {
        let (parent, name) = match self.parent_of(path) {
            Ok(found) => found,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let file =
            match rustix::fs::openat(&parent, &name, open_flags(NodeKind::File), Mode::empty()) {
                Ok(file) => File::from(file),
                Err(Errno::NOENT) => return Ok(None),
                Err(error) => return Err(error.into()),
            };
        let metadata = EntryMetadata::from_file(&file)?;
        if metadata.kind() != NodeKind::File || metadata.size() > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Expected a small regular file",
            ));
        }
        let mut content = Vec::new();
        file.take(limit + 1).read_to_end(&mut content)?;
        if content.len() as u64 > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "File grew while reading",
            ));
        }
        Ok(Some(content))
    }

    /// Open `path`'s parent, creating missing directories without
    /// following links.
    fn create_parents(&self, path: &SyncPath, parents: Parents) -> io::Result<(Dir, PathBuf)> {
        let mode = match parents {
            Parents::Private => PRIVATE_DIRECTORY_MODE,
            Parents::Default => DEFAULT_DIRECTORY_MODE,
        };
        let mut directory = self.directory.try_clone()?;
        let mut components: Vec<&[u8]> = path.components().collect();
        let name = PathBuf::from(OsStr::from_bytes(
            components.pop().expect("a path has a name"),
        ));
        for component in components {
            let component = Path::new(OsStr::from_bytes(component));
            match rustix::fs::mkdirat(&directory, component, mode) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(error) => return Err(error.into()),
            }
            directory = directory.open_dir_nofollow(component)?;
        }
        Ok((directory, name))
    }
}

/// Who may read folders a write creates on the way to its destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Parents {
    /// Foldover's own folders, such as the archive.
    Private,
    /// Folders in the user's tree; the umask applies.
    Default,
}

/// Open the directory at `path` below `parent`, one no-follow step per
/// component. Depth costs iterations, not simultaneous descriptors.
fn open_below(parent: &Dir, path: &Path) -> io::Result<Dir> {
    let mut directory = parent.try_clone()?;
    for component in path.components() {
        directory = directory.open_dir_nofollow(component.as_os_str())?;
    }
    Ok(directory)
}

/// Permanently delete `name` below `parent`, never following links. Each
/// directory's root-relative path is collected breadth-first — one allocation
/// per directory, reopened component by component — then removed deepest
/// first, so nesting depth is bounded by memory rather than descriptors or a
/// fixed cap.
fn remove_entry(parent: &Dir, name: &Path) -> io::Result<()> {
    if EntryMetadata::at(parent, name)?.kind() != NodeKind::Directory {
        rustix::fs::unlinkat(parent, name, AtFlags::empty())?;
        return Ok(());
    }
    let mut directories: Vec<PathBuf> = vec![name.to_path_buf()];
    let mut next = 0;
    while next < directories.len() {
        let directory = open_below(parent, &directories[next])?;
        let mut children = Vec::new();
        for entry in directory.entries()? {
            let child = PathBuf::from(entry?.file_name());
            if EntryMetadata::at(&directory, &child)?.kind() == NodeKind::Directory {
                children.push(child);
            } else {
                rustix::fs::unlinkat(&directory, &child, AtFlags::empty())?;
            }
        }
        for child in children {
            let mut deeper = directories[next].clone();
            deeper.push(child);
            directories.push(deeper);
        }
        next += 1;
    }
    // Every directory follows its parent in `directories`, so reversed order
    // leaves each parent empty when it is removed.
    for path in directories.iter().rev() {
        let ancestors = path.parent().unwrap_or_else(|| Path::new(""));
        let directory = open_below(parent, ancestors)?;
        let leaf = Path::new(path.file_name().expect("a collected path has a name"));
        rustix::fs::unlinkat(&directory, leaf, AtFlags::REMOVEDIR)?;
    }
    Ok(())
}

fn temporary_name() -> PathBuf {
    PathBuf::from(format!(
        "{TEMPORARY_PREFIX}{}",
        uuid::Uuid::new_v4().simple()
    ))
}

fn create_temporary_file(parent: Dir, mode: Mode) -> Result<(Staged, File), WriteError> {
    let name = temporary_name();
    let file = rustix::fs::openat(
        &parent,
        &name,
        OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        mode,
    )?;
    let staged = Staged {
        parent,
        name,
        placed: false,
    };
    Ok((staged, File::from(file)))
}

fn open_flags(kind: NodeKind) -> OFlags {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    match kind {
        NodeKind::Directory => flags | OFlags::DIRECTORY,
        _ => flags,
    }
}

fn directory_identity(directory: &Dir) -> io::Result<(u64, u64)> {
    let stat = rustix::fs::fstat(directory)?;
    Ok((stat.st_dev as u64, stat.st_ino as u64))
}

/// cap-std may hold `O_PATH` descriptors on Linux, which cannot be synced;
/// reopen the held directory itself.
fn sync_directory(directory: &Dir, durability: Durability) -> io::Result<()> {
    let reopened = rustix::fs::openat(
        directory,
        c".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    flush(&reopened, durability)
}

fn flush(file: &impl AsFd, durability: Durability) -> io::Result<()> {
    match durability {
        Durability::Standard => rustix::fs::fsync(file)?,
        Durability::Full => flush_device_cache(file)?,
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn flush_device_cache(file: &impl AsFd) -> io::Result<()> {
    Ok(rustix::fs::fcntl_fullfsync(file)?)
}

/// Linux `fsync` already flushes the device cache.
#[cfg(not(target_os = "macos"))]
fn flush_device_cache(file: &impl AsFd) -> io::Result<()> {
    Ok(rustix::fs::fsync(file)?)
}

#[cfg(test)]
mod tests;
