//! Applying a source entry's native metadata to a target under a pair's
//! metadata policy. Attributes go first, since a read-only mode would block
//! later xattr writes; times go last.

use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::AsFd;
use std::time::{SystemTime, UNIX_EPOCH};

use foldover_types::{MetadataPolicy, Propagation};
use rustix::fs::{Mode, Timespec, Timestamps, XattrFlags};

use super::super::{AttributeRole, EntryMetadata, NodeKind};
use super::{ForeignMetadata, NativeMetadata};

/// Setuid, setgid and sticky bits are never copied.
const PERMISSION_BITS: u32 = 0o777;
const EXECUTE_BITS: u32 = 0o111;
const READ_BITS: u32 = 0o444;
/// Read permission shifted onto execute permission: `r--` becomes `--x`.
const READ_TO_EXECUTE_SHIFT: u32 = 2;
const MAX_VALUE_BYTES: usize = 16 * 1024 * 1024;
const NANOS_PER_SECOND: i64 = 1_000_000_000;
/// Most attribute bytes a copy between Macs carries; its messages hold them.
const MAX_NATIVE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Destination {
    /// Just created by this write; receives the resource fork too.
    New,
    /// Already holds content; only synced metadata changes.
    Existing,
}

pub(super) fn apply(
    source: &impl AsFd,
    source_metadata: &EntryMetadata,
    target: &impl AsFd,
    destination: Destination,
    policy: MetadataPolicy,
) -> io::Result<()> {
    let target_metadata = EntryMetadata::from_file(target)?;

    // The fork is content, so it follows new copies whatever the policy.
    if destination == Destination::New {
        copy_attributes(source, source_metadata, target, AttributeRole::ResourceFork)?;
    }

    if policy.extended_attributes() == Propagation::Sync {
        if destination == Destination::Existing {
            remove_attributes(target, &target_metadata, AttributeRole::Synced)?;
        }
        copy_attributes(source, source_metadata, target, AttributeRole::Synced)?;
        if let Some(flags) = source_metadata.user_flags() {
            set_user_flags(target, &target_metadata, flags)?;
        }
    }

    apply_permissions(
        source,
        source_metadata,
        target,
        &target_metadata,
        policy.permissions(),
    )?;

    if policy.modification_times() == Propagation::Sync && source_metadata.kind() == NodeKind::File
    {
        set_modified(target, source_metadata.modified())?;
    }
    Ok(())
}

/// What a copy to another Mac carries of `file`: its resource fork, and
/// the attributes and user flags the pair syncs.
pub(super) fn native(
    file: &impl AsFd,
    metadata: &EntryMetadata,
    policy: MetadataPolicy,
) -> io::Result<NativeMetadata> {
    let synced = policy.extended_attributes() == Propagation::Sync;
    let mut native = NativeMetadata::default();
    let mut bytes = 0;
    for name in attribute_names(file)? {
        let wanted = match metadata.attribute_role(name.as_bytes()) {
            AttributeRole::ResourceFork => true,
            AttributeRole::Synced => synced,
            AttributeRole::CompressionStorage | AttributeRole::SystemManaged => false,
        };
        if !wanted {
            continue;
        }
        let size = rustix::fs::fgetxattr(file, name.as_c_str(), &mut [0_u8; 0][..])?;
        bytes += name.as_bytes().len() + size;
        if bytes > MAX_NATIVE_BYTES {
            return Err(io::Error::other(
                "Extended attributes exceed what a copy between Macs carries",
            ));
        }
        let mut value = vec![0; size];
        let size = rustix::fs::fgetxattr(file, name.as_c_str(), &mut value)?;
        value.truncate(size);
        native.attributes.push((name.into_bytes(), value));
    }
    if synced {
        native.flags = metadata.user_flags();
    }
    Ok(native)
}

/// Give `target` attributes and flags another Mac sent. Only names this
/// side would sync itself are set, so another Mac cannot plant system
/// state; a fork only on a new file, as with local copies.
pub(super) fn apply_native(
    target: &impl AsFd,
    native: &NativeMetadata,
    destination: Destination,
    policy: MetadataPolicy,
) -> io::Result<()> {
    let target_metadata = EntryMetadata::from_file(target)?;
    let synced = policy.extended_attributes() == Propagation::Sync;
    if synced && destination == Destination::Existing {
        remove_attributes(target, &target_metadata, AttributeRole::Synced)?;
    }
    for (name, value) in &native.attributes {
        let allowed = match target_metadata.attribute_role(name) {
            AttributeRole::ResourceFork => destination == Destination::New,
            AttributeRole::Synced => synced,
            AttributeRole::CompressionStorage | AttributeRole::SystemManaged => false,
        };
        if !allowed {
            continue;
        }
        let name = CString::new(name.clone())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid xattr name"))?;
        rustix::fs::fsetxattr(target, name.as_c_str(), value, XattrFlags::empty())?;
    }
    if synced && let Some(flags) = native.flags {
        set_user_flags(target, &target_metadata, flags)?;
    }
    Ok(())
}

/// Give `target` the synced part of metadata from a system without
/// extended attributes, such as an SFTP server.
pub(super) fn apply_foreign(
    target: &impl AsFd,
    metadata: ForeignMetadata,
    policy: MetadataPolicy,
) -> io::Result<()> {
    let target_metadata = EntryMetadata::from_file(target)?;
    let current = target_metadata.permissions() & PERMISSION_BITS;
    let wanted = match policy.permissions() {
        Propagation::Sync => metadata.permissions & PERMISSION_BITS,
        Propagation::Ignore if target_metadata.kind() == NodeKind::File => {
            if metadata.permissions & EXECUTE_BITS != 0 {
                current | ((current & READ_BITS) >> READ_TO_EXECUTE_SHIFT)
            } else {
                current & !EXECUTE_BITS
            }
        }
        Propagation::Ignore => current,
    };
    if wanted != current {
        rustix::fs::fchmod(target, Mode::from_raw_mode(wanted as _))?;
    }
    if policy.modification_times() == Propagation::Sync && target_metadata.kind() == NodeKind::File
    {
        set_modified(target, metadata.modified)?;
    }
    Ok(())
}

fn apply_permissions(
    source: &impl AsFd,
    source_metadata: &EntryMetadata,
    target: &impl AsFd,
    target_metadata: &EntryMetadata,
    permissions: Propagation,
) -> io::Result<()> {
    let current = target_metadata.permissions() & PERMISSION_BITS;
    let wanted = match permissions {
        Propagation::Sync => {
            copy_access_control(source, target)?;
            source_metadata.permissions() & PERMISSION_BITS
        }
        // Like git: only whether a file is executable follows it. Execute is
        // granted to whoever may read, e.g. 644 becomes 755.
        Propagation::Ignore if source_metadata.kind() == NodeKind::File => {
            if source_metadata.permissions() & EXECUTE_BITS != 0 {
                current | ((current & READ_BITS) >> READ_TO_EXECUTE_SHIFT)
            } else {
                current & !EXECUTE_BITS
            }
        }
        Propagation::Ignore => current,
    };
    if wanted != current {
        rustix::fs::fchmod(target, Mode::from_raw_mode(wanted as _))?;
    }
    Ok(())
}

fn copy_attributes(
    source: &impl AsFd,
    source_metadata: &EntryMetadata,
    target: &impl AsFd,
    role: AttributeRole,
) -> io::Result<()> {
    for name in attribute_names(source)? {
        if source_metadata.attribute_role(name.as_bytes()) != role {
            continue;
        }
        copy_attribute(source, target, &name, role)?;
    }
    Ok(())
}

fn remove_attributes(
    target: &impl AsFd,
    target_metadata: &EntryMetadata,
    role: AttributeRole,
) -> io::Result<()> {
    for name in attribute_names(target)? {
        if target_metadata.attribute_role(name.as_bytes()) == role {
            rustix::fs::fremovexattr(target, name.as_c_str())?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn copy_attribute(
    source: &impl AsFd,
    target: &impl AsFd,
    name: &CStr,
    role: AttributeRole,
) -> io::Result<()> {
    if role == AttributeRole::ResourceFork {
        return crate::mac_metadata::copy_resource_fork(source, target);
    }
    copy_attribute_value(source, target, name)
}

#[cfg(not(target_os = "macos"))]
fn copy_attribute(
    source: &impl AsFd,
    target: &impl AsFd,
    name: &CStr,
    _: AttributeRole,
) -> io::Result<()> {
    copy_attribute_value(source, target, name)
}

fn copy_attribute_value(source: &impl AsFd, target: &impl AsFd, name: &CStr) -> io::Result<()> {
    let size = rustix::fs::fgetxattr(source, name, &mut [0_u8; 0][..])?;
    if size > MAX_VALUE_BYTES {
        return Err(io::Error::other(
            "Extended attribute exceeds the copy limit",
        ));
    }
    let mut value = vec![0; size];
    let size = rustix::fs::fgetxattr(source, name, &mut value)?;
    value.truncate(size);
    rustix::fs::fsetxattr(target, name, &value, XattrFlags::empty())?;
    Ok(())
}

pub(super) fn attribute_names(file: &impl AsFd) -> io::Result<Vec<CString>> {
    let size = rustix::fs::flistxattr(file, &mut [0_u8; 0][..])?;
    let mut bytes = vec![0; size];
    let size = rustix::fs::flistxattr(file, &mut bytes)?;
    bytes.truncate(size);
    bytes
        .split_inclusive(|byte| *byte == 0)
        .map(|name| {
            CString::from_vec_with_nul(name.to_vec())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid xattr name list"))
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn set_user_flags(
    target: &impl AsFd,
    target_metadata: &EntryMetadata,
    flags: u32,
) -> io::Result<()> {
    let Some(current) = target_metadata.flags() else {
        return Ok(());
    };
    // Keep the target's own lock, storage and system flags.
    let wanted = (current & !super::super::USER_FLAGS) | (flags & super::super::USER_FLAGS);
    if wanted == current {
        return Ok(());
    }
    crate::mac_metadata::set_flags(target, wanted)
}

#[cfg(not(target_os = "macos"))]
fn set_user_flags(_: &impl AsFd, _: &EntryMetadata, _: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn copy_access_control(source: &impl AsFd, target: &impl AsFd) -> io::Result<()> {
    crate::mac_metadata::copy_access_control(source, target)
}

/// Linux ACLs live in privileged xattr namespaces, which are not copied.
#[cfg(not(target_os = "macos"))]
fn copy_access_control(_: &impl AsFd, _: &impl AsFd) -> io::Result<()> {
    Ok(())
}

fn set_modified(target: &impl AsFd, modified: SystemTime) -> io::Result<()> {
    let omit = Timespec {
        tv_sec: 0,
        tv_nsec: rustix::fs::UTIME_OMIT,
    };
    rustix::fs::futimens(
        target,
        &Timestamps {
            last_access: omit,
            last_modification: timespec(modified)?,
        },
    )?;
    Ok(())
}

fn timespec(time: SystemTime) -> io::Result<Timespec> {
    let out_of_range = || io::Error::other("Modification time is outside the supported range");
    let (seconds, nanoseconds) = match time.duration_since(UNIX_EPOCH) {
        Ok(after) => (
            i64::try_from(after.as_secs()).map_err(|_| out_of_range())?,
            i64::from(after.subsec_nanos()),
        ),
        Err(before) => {
            let before = before.duration();
            let seconds = i64::try_from(before.as_secs()).map_err(|_| out_of_range())?;
            match i64::from(before.subsec_nanos()) {
                0 => (-seconds, 0),
                nanoseconds => (-seconds - 1, NANOS_PER_SECOND - nanoseconds),
            }
        }
    };
    Ok(Timespec {
        tv_sec: seconds as _,
        tv_nsec: nanoseconds as _,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn times_before_the_epoch_round_down_to_whole_seconds() {
        let before = UNIX_EPOCH - Duration::from_millis(1_500);
        let spec = timespec(before).unwrap();
        assert_eq!((spec.tv_sec as i64, spec.tv_nsec as i64), (-2, 500_000_000));
        let after = UNIX_EPOCH + Duration::from_millis(1_500);
        let spec = timespec(after).unwrap();
        assert_eq!((spec.tv_sec as i64, spec.tv_nsec as i64), (1, 500_000_000));
    }
}
