//! `foldover-net` — driver layer. Unix sockets with peer checks, SFTP,
//! QUIC, mDNS discovery and share mounting. Calls the OS directly;
//! depends on no other Foldover crate. See PLAN.md §4.4.

#[cfg(unix)]
pub mod local;

#[cfg(unix)]
pub mod sftp;

pub mod peer;

pub mod discovery;

#[cfg(any(target_os = "macos", test))]
mod code_identity;

#[cfg(target_os = "macos")]
mod installed;
