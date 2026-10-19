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

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod power;

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

/// Keeps the Mac from sleeping when idle until dropped. Sleep the person
/// chooses, such as closing the lid, still happens.
pub struct IdleSleepAssertion {
    /// Held for its release on drop.
    #[cfg(target_os = "macos")]
    _held: power::Assertion,
}

impl IdleSleepAssertion {
    /// `reason` names the assertion in `pmset -g assertions`, e.g.
    /// "Foldover is syncing Photos". Fails with `Unsupported` on systems
    /// without power assertions.
    pub fn take(reason: &str) -> io::Result<Self> {
        #[cfg(target_os = "macos")]
        return power::Assertion::prevent_idle_sleep(reason).map(|held| Self { _held: held });
        #[cfg(not(target_os = "macos"))]
        {
            let _ = reason;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Power assertions are not supported on this system",
            ))
        }
    }
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
