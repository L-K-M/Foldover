//! `foldover-os` — driver layer. Host facilities: power assertions,
//! network path (expensive/constrained), Keychain and Secret Service.
//! Calls the OS directly; depends on no other Foldover crate. See
//! PLAN.md §4.4.

mod peer_identity;

pub use peer_identity::{IdentityStoreError, PeerIdentityStore};

#[cfg(target_os = "macos")]
pub use peer_identity::KeychainIdentityStore;
