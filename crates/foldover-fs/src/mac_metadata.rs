use std::ffi::{CStr, c_int, c_void};
use std::io;
use std::os::fd::{AsFd, AsRawFd};
use std::ptr::NonNull;

use crate::prevent_materialization;

// Darwin sys/acl.h. Native tests compare this with the installed SDK.
const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;
const MAX_ACL_BYTES: usize = 64 * 1024;
const FORK_CHUNK_BYTES: usize = 1024 * 1024;
const RESOURCE_FORK: &CStr = c"com.apple.ResourceFork";

// SAFETY: Darwin's acl_t is an opaque owned pointer. All signatures match
// sys/acl.h; ACL buffers are private, synchronous and freed with acl_free.
unsafe extern "C" {
    fn acl_get_fd_np(fd: c_int, kind: c_int) -> *mut c_void;
    fn acl_size(acl: *mut c_void) -> libc::ssize_t;
    fn acl_copy_ext(buffer: *mut c_void, acl: *mut c_void, size: libc::ssize_t) -> libc::ssize_t;
    fn acl_free(acl: *mut c_void) -> c_int;
}

struct Acl(NonNull<c_void>);

impl Drop for Acl {
    fn drop(&mut self) {
        // SAFETY: this pointer came from acl_get_fd_np and has one private owner.
        // It is freed once, including when sizing/copying returns an error.
        let result = unsafe { acl_free(self.0.as_ptr()) };
        debug_assert_eq!(result, 0);
    }
}

pub(super) fn attribute_list(file: &impl AsFd, bytes: &mut [u8]) -> io::Result<usize> {
    prevent_materialization()?;
    let buffer = if bytes.is_empty() {
        std::ptr::null_mut()
    } else {
        bytes.as_mut_ptr()
    };
    // SAFETY: the descriptor is live and the initialized buffer has the supplied
    // capacity. A zero capacity is the documented size query. No pointers escape.
    let count = unsafe {
        libc::flistxattr(
            file.as_fd().as_raw_fd(),
            buffer.cast(),
            bytes.len(),
            libc::XATTR_SHOWCOMPRESSION,
        )
    };
    checked_count(count)
}

pub(super) fn attribute_value(
    file: &impl AsFd,
    name: &CStr,
    position: u32,
    bytes: &mut [u8],
) -> io::Result<usize> {
    prevent_materialization()?;
    let buffer = if bytes.is_empty() {
        std::ptr::null_mut()
    } else {
        bytes.as_mut_ptr()
    };
    // SAFETY: name is NUL terminated; the borrowed descriptor and initialized
    // buffer remain live. Only resource forks receive a nonzero position.
    // A zero capacity is the documented size query; libc retains no pointers.
    let count = unsafe {
        libc::fgetxattr(
            file.as_fd().as_raw_fd(),
            name.as_ptr(),
            buffer.cast(),
            bytes.len(),
            position,
            libc::XATTR_SHOWCOMPRESSION,
        )
    };
    checked_count(count)
}

pub(super) fn access_control(file: &impl AsFd) -> io::Result<Option<Vec<u8>>> {
    prevent_materialization()?;
    // SAFETY: the descriptor is borrowed for the call; a successful result is
    // a new ACL allocation, immediately given exactly one RAII owner.
    let pointer = unsafe { acl_get_fd_np(file.as_fd().as_raw_fd(), ACL_TYPE_EXTENDED) };
    let Some(pointer) = NonNull::new(pointer) else {
        let error = io::Error::last_os_error();
        // filesec_get_property reports ENOENT when this held file has no ACL.
        // Unsupported filesystems and other lookup errors remain failures.
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        return Err(error);
    };
    let acl = Acl(pointer);
    // SAFETY: acl is a live library-owned ACL, not caller-authored bytes.
    let size = checked_count(unsafe { acl_size(acl.0.as_ptr()) })?;
    if size > MAX_ACL_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            "ACL exceeds the read limit",
        ));
    }
    let mut bytes = vec![0; size];
    // SAFETY: bytes has the exact reported capacity and is initialized. acl is
    // still live. The portable big-endian representation is copied synchronously.
    let count = unsafe {
        acl_copy_ext(
            bytes.as_mut_ptr().cast(),
            acl.0.as_ptr(),
            size as libc::ssize_t,
        )
    };
    if checked_count(count)? != size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ACL encoding changed size",
        ));
    }
    Ok(Some(bytes))
}

/// Copy only the ACL between open descriptors. Data, xattrs, mode and times
/// follow the pair's policy elsewhere.
pub(super) fn copy_access_control(source: &impl AsFd, target: &impl AsFd) -> io::Result<()> {
    prevent_materialization()?;
    // SAFETY: both descriptors are live for the call. A null state is the
    // documented stateless form; fcopyfile retains no pointers afterwards.
    let result = unsafe {
        libc::fcopyfile(
            source.as_fd().as_raw_fd(),
            target.as_fd().as_raw_fd(),
            std::ptr::null_mut(),
            libc::COPYFILE_ACL,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn set_flags(file: &impl AsFd, flags: u32) -> io::Result<()> {
    // SAFETY: the borrowed descriptor is live; fchflags takes plain values.
    let result = unsafe { libc::fchflags(file.as_fd().as_raw_fd(), flags) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Stream a resource fork in positional chunks, so forks up to Darwin's
/// 32-bit limit never need one allocation.
pub(super) fn copy_resource_fork(source: &impl AsFd, target: &impl AsFd) -> io::Result<()> {
    let size = u32::try_from(attribute_value(source, RESOURCE_FORK, 0, &mut [])?)
        .map_err(|_| io::Error::other("Resource fork exceeds Darwin's positional limit"))?;
    let mut buffer = vec![0; FORK_CHUNK_BYTES.min(size as usize)];
    let mut offset: u32 = 0;
    loop {
        let count = attribute_value(source, RESOURCE_FORK, offset, &mut buffer)?;
        set_attribute(target, RESOURCE_FORK, offset, &buffer[..count])?;
        offset = offset
            .checked_add(u32::try_from(count).map_err(|_| io::Error::other("Oversized read"))?)
            .ok_or_else(|| io::Error::other("Resource fork exceeds Darwin's positional limit"))?;
        if offset >= size {
            return Ok(());
        }
        if count == 0 {
            return Err(io::Error::other("Resource fork shrank while copying"));
        }
    }
}

fn set_attribute(file: &impl AsFd, name: &CStr, position: u32, bytes: &[u8]) -> io::Result<()> {
    prevent_materialization()?;
    // SAFETY: name is NUL terminated; the borrowed descriptor and initialized
    // buffer stay live. Only resource forks receive a nonzero position.
    let result = unsafe {
        libc::fsetxattr(
            file.as_fd().as_raw_fd(),
            name.as_ptr(),
            bytes.as_ptr().cast(),
            bytes.len(),
            position,
            0,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn checked_count(count: libc::ssize_t) -> io::Result<usize> {
    if count == -1 {
        return Err(io::Error::last_os_error());
    }
    usize::try_from(count)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid native metadata length"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acl_type_matches_the_native_sdk() {
        let output = std::process::Command::new("/usr/bin/xcrun")
            .args([
                "swift",
                "-e",
                "import Darwin; print(ACL_TYPE_EXTENDED.rawValue)",
            ])
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout)
                .unwrap()
                .trim()
                .parse::<c_int>()
                .unwrap(),
            ACL_TYPE_EXTENDED
        );
    }
}
