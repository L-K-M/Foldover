//! `foldover-os` — driver layer. Host facilities: power assertions,
//! network path (expensive/constrained), Keychain and Secret Service.
//! Calls the OS directly; depends on no other Foldover crate. See
//! PLAN.md §4.4.

mod peer_identity;

pub use peer_identity::{IdentityStoreError, PeerIdentityStore};

#[cfg(target_os = "macos")]
pub use peer_identity::KeychainIdentityStore;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod mount;

use std::io;
use std::path::PathBuf;

/// This computer's name for other Macs, e.g. "Studio-iMac" from
/// `Studio-iMac.local`; "Mac" when the system reports none.
pub fn computer_name() -> String {
    let name = rustix::system::uname()
        .nodename()
        .to_string_lossy()
        .into_owned();
    let name = name
        .strip_suffix(".local")
        .unwrap_or(&name)
        .trim()
        .to_owned();
    if name.is_empty() {
        return String::from("Mac");
    }
    name
}

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
