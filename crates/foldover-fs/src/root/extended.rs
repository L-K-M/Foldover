use std::ffi::CString;
use std::io;
use std::os::fd::AsFd;

use foldover_types::{Digest, ScanControl, SyncPath};
use nix::fcntl::{OFlag, openat};
use nix::sys::stat::Mode;

use crate::prevent_materialization;

use super::{DataResidency, EntryMetadata, NodeKind, Root};

const MAX_ATTRIBUTE_LIST_BYTES: usize = 64 * 1024;
const MAX_ATTRIBUTE_VALUE_BYTES: u64 = 16 * 1024 * 1024;
// Darwin's resource-fork xattr reader takes a 32-bit position.
const MAX_RESOURCE_FORK_BYTES: u64 = u32::MAX as u64;
const MAX_METADATA_READ_BYTES: u64 = MAX_RESOURCE_FORK_BYTES + MAX_ATTRIBUTE_VALUE_BYTES;
#[cfg(target_os = "macos")]
const ATTRIBUTE_BUFFER_BYTES: usize = 64 * 1024;
#[cfg(target_os = "macos")]
const RESOURCE_FORK: &std::ffi::CStr = c"com.apple.ResourceFork";

#[derive(Debug, thiserror::Error)]
pub enum MetadataReadError {
    #[error("Extended metadata read failed: {0}")]
    Io(#[from] io::Error),
    #[error("Entry changed since its metadata was observed")]
    Changed,
    #[error("Extended metadata is unavailable for this entry type on this platform")]
    UnsupportedEntry,
    #[error("Dataless metadata requires an explicit materialization policy")]
    Dataless,
    #[error("Metadata read cancelled; no complete result was returned")]
    Cancelled,
    #[error("Extended metadata exceeds the bounded read limit")]
    LimitExceeded,
}

/// Raw native attribute identity and a digest of every value byte. These are
/// observations; endpoint capabilities must select what can actually be stored.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtendedAttribute {
    name: Vec<u8>,
    size: u64,
    digest: Digest,
}

impl ExtendedAttribute {
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn digest(&self) -> Digest {
        self.digest
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AccessControl {
    /// Darwin's big-endian external ACL encoding, including ordered entries.
    MacOs(Option<Vec<u8>>),
    /// Linux POSIX ACLs are xattrs; no Darwin ACL observation is manufactured.
    NotReported,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtendedMetadata {
    attributes: Vec<ExtendedAttribute>,
    access_control: AccessControl,
}

impl ExtendedMetadata {
    /// Attributes are ordered by exact name bytes, independently of native order.
    pub fn attributes(&self) -> &[ExtendedAttribute] {
        &self.attributes
    }

    pub fn access_control(&self) -> &AccessControl {
        &self.access_control
    }
}

struct Progress<'a, F> {
    control: &'a mut F,
    bytes: u64,
}

impl<F: FnMut(u64) -> ScanControl> Progress<'_, F> {
    fn checkpoint(&mut self) -> Result<(), MetadataReadError> {
        if (self.control)(self.bytes) == ScanControl::Cancel {
            return Err(MetadataReadError::Cancelled);
        }
        // A callback can change thread policy; never carry that into native I/O.
        prevent_materialization()?;
        Ok(())
    }
}

impl Root {
    /// Read native xattrs/ACLs from the observed entry, never from its link target.
    /// Errors, cancellation and unsupported metadata never become empty metadata.
    pub fn read_extended_metadata(
        &self,
        path: &SyncPath,
        expected: &EntryMetadata,
        control: &mut impl FnMut(u64) -> ScanControl,
    ) -> Result<ExtendedMetadata, MetadataReadError> {
        let mut progress = Progress { control, bytes: 0 };
        progress.checkpoint()?;
        validate_entry(expected)?;
        let (parent, name) = self.parent_of(path)?;
        if EntryMetadata::at(&parent, &name)? != *expected {
            return Err(MetadataReadError::Changed);
        }
        let mut flags = OFlag::O_RDONLY
            | OFlag::O_CLOEXEC
            | OFlag::O_NOFOLLOW
            | OFlag::O_NONBLOCK
            | OFlag::O_NOCTTY;
        if expected.kind() == NodeKind::Directory {
            flags |= OFlag::O_DIRECTORY;
        }
        #[cfg(target_os = "macos")]
        if expected.kind() == NodeKind::Symlink {
            // O_NOFOLLOW rejects the link even with O_SYMLINK. The latter opens
            // the link itself; the descriptor check still rejects substitutions.
            flags.remove(OFlag::O_NOFOLLOW);
            flags |= OFlag::from_bits_retain(libc::O_SYMLINK);
        }
        let file = openat(&parent, &name, flags, Mode::empty()).map_err(io::Error::from)?;
        if EntryMetadata::from_file(&file)? != *expected {
            return Err(MetadataReadError::Changed);
        }
        let result = capture(&file, &mut progress)?;
        progress.checkpoint()?;
        if EntryMetadata::from_file(&file)? != *expected || self.metadata(path)? != *expected {
            return Err(MetadataReadError::Changed);
        }
        Ok(result)
    }

    /// Root directory metadata is part of a future snapshot too; keep its access
    /// on the held descriptor rather than reopening an ambient root path.
    pub fn read_root_extended_metadata(
        &self,
        expected: &EntryMetadata,
        control: &mut impl FnMut(u64) -> ScanControl,
    ) -> Result<ExtendedMetadata, MetadataReadError> {
        let mut progress = Progress { control, bytes: 0 };
        progress.checkpoint()?;
        validate_entry(expected)?;
        if self.root_metadata()? != *expected {
            return Err(MetadataReadError::Changed);
        }
        // cap-std may hold an O_PATH descriptor on Linux. Reopen the held
        // directory itself for xattrs; never resolve its ambient pathname.
        let file = openat(
            &self.directory,
            c".",
            OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_DIRECTORY | OFlag::O_NONBLOCK,
            Mode::empty(),
        )
        .map_err(io::Error::from)?;
        if EntryMetadata::from_file(&file)? != *expected {
            return Err(MetadataReadError::Changed);
        }
        let result = capture(&file, &mut progress)?;
        progress.checkpoint()?;
        if EntryMetadata::from_file(&file)? != *expected || self.root_metadata()? != *expected {
            return Err(MetadataReadError::Changed);
        }
        Ok(result)
    }
}

fn validate_entry(expected: &EntryMetadata) -> Result<(), MetadataReadError> {
    if expected.data_residency() == DataResidency::Dataless {
        return Err(MetadataReadError::Dataless);
    }
    if expected.kind() == NodeKind::Other {
        return Err(MetadataReadError::UnsupportedEntry);
    }
    #[cfg(not(target_os = "macos"))]
    if expected.kind() == NodeKind::Symlink {
        // Linux fgetxattr cannot read an O_PATH symlink. Do not use an ambient
        // pathname or claim that an unread link has no extended metadata.
        return Err(MetadataReadError::UnsupportedEntry);
    }
    Ok(())
}

fn capture(
    file: &impl AsFd,
    progress: &mut Progress<'_, impl FnMut(u64) -> ScanControl>,
) -> Result<ExtendedMetadata, MetadataReadError> {
    let names = attribute_names(file)?;
    let mut attributes = Vec::with_capacity(names.len());
    for name in &names {
        progress.checkpoint()?;
        let size = attribute_value(file, name, 0, &mut [])? as u64;
        if size > MAX_METADATA_READ_BYTES - progress.bytes {
            return Err(MetadataReadError::LimitExceeded);
        }
        let digest = attribute_digest(file, name, size, progress)?;
        if attribute_value(file, name, 0, &mut [])? as u64 != size {
            return Err(MetadataReadError::Changed);
        }
        attributes.push(ExtendedAttribute {
            name: name.as_bytes().to_vec(),
            size,
            digest,
        });
    }
    progress.checkpoint()?;
    let access_control = access_control(file)?;
    if attribute_names(file)? != names {
        return Err(MetadataReadError::Changed);
    }
    Ok(ExtendedMetadata {
        attributes,
        access_control,
    })
}

fn attribute_names(file: &impl AsFd) -> Result<Vec<CString>, MetadataReadError> {
    prevent_materialization()?;
    let size = attribute_list(file, &mut [])?;
    if size > MAX_ATTRIBUTE_LIST_BYTES {
        return Err(MetadataReadError::LimitExceeded);
    }
    let mut bytes = vec![0; size];
    if attribute_list(file, &mut bytes)? != size {
        return Err(MetadataReadError::Changed);
    }
    parse_names(&bytes).map_err(MetadataReadError::from)
}

fn parse_names(bytes: &[u8]) -> io::Result<Vec<CString>> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "Invalid xattr name list");
    let mut names = bytes
        .split_inclusive(|byte| *byte == 0)
        .map(|name| CString::from_vec_with_nul(name.to_vec()).map_err(|_| invalid()))
        .collect::<io::Result<Vec<_>>>()?;
    names.sort_unstable();
    if names.iter().any(|name| name.as_bytes().is_empty())
        || names.windows(2).any(|pair| pair[0] == pair[1])
    {
        return Err(invalid());
    }
    Ok(names)
}

fn attribute_digest(
    file: &impl AsFd,
    name: &std::ffi::CStr,
    size: u64,
    progress: &mut Progress<'_, impl FnMut(u64) -> ScanControl>,
) -> Result<Digest, MetadataReadError> {
    #[cfg(target_os = "macos")]
    if name == RESOURCE_FORK {
        return resource_fork_digest(file, size, progress);
    }
    if size > MAX_ATTRIBUTE_VALUE_BYTES {
        return Err(MetadataReadError::LimitExceeded);
    }
    let mut value = vec![0; size as usize];
    progress.checkpoint()?;
    if attribute_value(file, name, 0, &mut value)? as u64 != size {
        return Err(MetadataReadError::Changed);
    }
    progress.bytes += size;
    Ok(Digest::from_bytes(*blake3::hash(&value).as_bytes()))
}

#[cfg(target_os = "macos")]
fn resource_fork_digest(
    file: &impl AsFd,
    size: u64,
    progress: &mut Progress<'_, impl FnMut(u64) -> ScanControl>,
) -> Result<Digest, MetadataReadError> {
    if size > MAX_RESOURCE_FORK_BYTES {
        return Err(MetadataReadError::LimitExceeded);
    }
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0; ATTRIBUTE_BUFFER_BYTES];
    let mut offset = 0;
    while offset < size {
        progress.checkpoint()?;
        let requested = (size - offset).min(buffer.len() as u64) as usize;
        let count = attribute_value(file, RESOURCE_FORK, offset as u32, &mut buffer[..requested])?;
        if count == 0 || count > requested {
            return Err(MetadataReadError::Changed);
        }
        hasher.update(&buffer[..count]);
        offset += count as u64;
        progress.bytes += count as u64;
    }
    Ok(Digest::from_bytes(*hasher.finalize().as_bytes()))
}

#[cfg(target_os = "macos")]
use crate::mac_metadata::{attribute_list, attribute_value};

#[cfg(target_os = "macos")]
fn access_control(file: &impl AsFd) -> io::Result<AccessControl> {
    crate::mac_metadata::access_control(file).map(AccessControl::MacOs)
}

#[cfg(not(target_os = "macos"))]
fn access_control(_: &impl AsFd) -> io::Result<AccessControl> {
    Ok(AccessControl::NotReported)
}

#[cfg(target_os = "linux")]
fn attribute_list(file: &impl AsFd, bytes: &mut [u8]) -> io::Result<usize> {
    rustix::fs::flistxattr(file, bytes).map_err(io::Error::from)
}

#[cfg(target_os = "linux")]
fn attribute_value(
    file: &impl AsFd,
    name: &std::ffi::CStr,
    _: u32,
    bytes: &mut [u8],
) -> io::Result<usize> {
    rustix::fs::fgetxattr(file, name, bytes).map_err(io::Error::from)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn attribute_list(_: &impl AsFd, _: &mut [u8]) -> io::Result<usize> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Native xattr reading is unavailable",
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn attribute_value(_: &impl AsFd, _: &std::ffi::CStr, _: u32, _: &mut [u8]) -> io::Result<usize> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Native xattr reading is unavailable",
    ))
}

#[cfg(test)]
mod tests;
