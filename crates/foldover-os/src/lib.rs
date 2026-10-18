//! `foldover-os` — driver layer. Host facilities: power assertions,
//! network path (expensive/constrained), Keychain and Secret Service.
//! Calls the OS directly; depends on no other Foldover crate. See
//! PLAN.md §4.4.

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod mount;

use std::io;
use std::path::PathBuf;

/// Mount the network share at `url`, e.g. `smb://alice@nas.local/Photos`,
/// under `/Volumes` the way Finder does, with credentials saved in the
/// user's Keychain and never a dialog. Returns the mount points.
pub fn mount_share(url: &str) -> io::Result<Vec<PathBuf>> {
    #[cfg(target_os = "macos")]
    return mount::mount_share(url);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = url;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Mounting shares is not supported on this system",
        ))
    }
}
