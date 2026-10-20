//! `foldover-os` — driver layer. Host facilities: power assertions,
//! network path (expensive/constrained), Keychain and Secret Service,
//! cloud-managed folder detection. Calls the OS directly; depends on no
//! other Foldover crate. See PLAN.md §4.4.

mod cloud;
mod peer_identity;

pub use cloud::{CloudProvider, cloud_provider, current_cloud_provider};
pub use peer_identity::{IdentityStoreError, PeerIdentityStore};

#[cfg(target_os = "macos")]
pub use peer_identity::KeychainIdentityStore;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod mount;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod power;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod environment;

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
    /// "Foldover is syncing Photos". `None` on systems without power
    /// assertions, so a failure where they exist is always an error.
    pub fn take(reason: &str) -> io::Result<Option<Self>> {
        #[cfg(target_os = "macos")]
        return power::Assertion::prevent_idle_sleep(reason).map(|held| Some(Self { _held: held }));
        #[cfg(not(target_os = "macos"))]
        {
            let _ = reason;
            Ok(None)
        }
    }
}

/// Whether the system asks apps to save power.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PowerMode {
    Normal,
    LowPower,
}

/// Low Power Mode on macOS; always `Normal` elsewhere.
pub fn power_mode() -> PowerMode {
    #[cfg(target_os = "macos")]
    if environment::low_power_mode() {
        return PowerMode::LowPower;
    }
    PowerMode::Normal
}

/// What using the network costs right now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkCost {
    Unrestricted,
    /// Cellular, or a phone's hotspot.
    Expensive,
    /// Low Data Mode.
    Constrained,
}

/// The current network path's cost on macOS, `None` until the system
/// first reports it, shortly after the first call; `Unrestricted`
/// elsewhere.
pub fn network_cost() -> Option<NetworkCost> {
    #[cfg(target_os = "macos")]
    return environment::network_cost();
    #[cfg(not(target_os = "macos"))]
    Some(NetworkCost::Unrestricted)
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
