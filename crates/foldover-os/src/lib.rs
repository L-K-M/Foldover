//! `foldover-os` — driver layer. Host facilities: power assertions,
//! network path (expensive/constrained), Keychain and Secret Service,
//! cloud-managed folder detection, thread priority and local wall-clock
//! time. Calls the
//! OS directly; depends only on the vocabulary crate. See PLAN.md §4.4.

mod cloud;

#[allow(unsafe_code)]
mod clock;

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

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod qos;

use std::io;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::time::SystemTime;

use foldover_types::{Effort, LocalDateTime};

/// `at` in the machine's local time zone, for calendar schedules
/// (PLAN.md §3.5); `None` when the C library cannot answer.
pub fn local_time(at: SystemTime) -> Option<LocalDateTime> {
    clock::local_time(at)
}

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

/// Holds the calling thread at the priority a run's effort asks for, and
/// restores the thread's previous priority when dropped (ADR 0054).
/// macOS threads never inherit their creator's priority, so every thread
/// a gentle run starts enters its effort itself.
#[must_use = "the priority lasts only while the guard lives"]
pub struct ThreadEffort {
    #[cfg(target_os = "macos")]
    _lowered: Option<qos::Lowered>,
    /// Restoring changes the calling thread, so the guard stays on it.
    _thread: PhantomData<*const ()>,
}

impl ThreadEffort {
    /// `Full` leaves the thread as it is. `Gentle` moves a thread above
    /// the utility class down to it, so the system runs it after the
    /// person's own work and prefers efficiency cores. A thread the system
    /// refuses to lower keeps its priority: priority is advice, and
    /// stopping a sync over it would be worse. Elsewhere this does nothing.
    pub fn enter(effort: Effort) -> Self {
        #[cfg(target_os = "macos")]
        return Self {
            _lowered: match effort {
                Effort::Full => None,
                Effort::Gentle => qos::lower_to_utility(),
            },
            _thread: PhantomData,
        };
        #[cfg(not(target_os = "macos"))]
        {
            let _ = effort;
            Self {
                _thread: PhantomData,
            }
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
