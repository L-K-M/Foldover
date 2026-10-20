//! Paired Macs (ADR 0031). The peer link runs outside the engine lock; the
//! engine records the devices both people confirmed.

use std::time::Duration;

pub use foldover_endpoint::{
    Answer, Decision, FileServer, FolderCache, LinkError, NearbyMac, NotRecorded, PeerHost,
    PeerIdentity, PeerLink, PendingPairing, Presence, serve_folder,
};

/// This Mac's identity for other Macs: the one in the Keychain on macOS,
/// a fresh one per agent start elsewhere (no Mac-to-Mac outside macOS).
pub fn peer_identity() -> Result<PeerIdentity, LinkError> {
    #[cfg(target_os = "macos")]
    return foldover_endpoint::load_peer_identity()
        .map_err(|error| LinkError::Identity(error.to_string()));
    #[cfg(not(target_os = "macos"))]
    Ok(PeerIdentity::generate()?)
}

/// This computer's name for other Macs.
pub fn computer_name() -> String {
    foldover_host::computer_name()
}

/// How long browsing for file servers may take; the pair editor waits on it.
const SERVER_BROWSE: Duration = Duration::from_secs(2);

/// SMB file servers on the local network (PLAN.md §3.8), for the pair
/// editor's nearby list. Blocks for about two seconds; call it off the
/// runtime.
pub fn nearby_file_servers() -> Result<Vec<FileServer>, LinkError> {
    foldover_endpoint::file_servers(SERVER_BROWSE)
        .map_err(|error| LinkError::Discovery(error.to_string()))
}
