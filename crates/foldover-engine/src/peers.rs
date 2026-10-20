//! Paired Macs (ADR 0031). The peer link runs outside the engine lock; the
//! engine records the devices both people confirmed.

pub use foldover_endpoint::{Decision, LinkError, PeerIdentity, PeerLink, PendingPairing};

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
