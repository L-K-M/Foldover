use foldover_os::IdentityStoreError;
#[cfg(any(target_os = "macos", test))]
use foldover_os::PeerIdentityStore;

use crate::PeerIdentity;

#[derive(Debug, thiserror::Error)]
pub enum PeerIdentityError {
    #[error(transparent)]
    Storage(#[from] IdentityStoreError),
    #[error(transparent)]
    Identity(#[from] foldover_net::peer::PeerError),
    #[error("The peer identity disappeared from the Keychain; reconnect after restoring access")]
    MissingAfterCreate,
}

/// Load this Mac's persistent peer identity, creating it only if absent.
/// Keychain access is blocking; call outside async runtime worker threads.
pub fn load_peer_identity() -> Result<PeerIdentity, PeerIdentityError> {
    #[cfg(target_os = "macos")]
    {
        load_from(&foldover_os::KeychainIdentityStore::open()?)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(IdentityStoreError::Unsupported.into())
    }
}

#[cfg(any(target_os = "macos", test))]
fn load_from(store: &impl PeerIdentityStore) -> Result<PeerIdentity, PeerIdentityError> {
    if let Some(bytes) = store.read()? {
        return Ok(PeerIdentity::from_secret_bytes(&bytes)?);
    }
    let identity = PeerIdentity::generate()?;
    let bytes = identity.to_secret_bytes()?;
    store.create_if_absent(&bytes)?;

    // Always read back: another creator may have won, and an identity must not
    // become usable until its stored certificate and key have been validated.
    let stored = store.read()?.ok_or(PeerIdentityError::MissingAfterCreate)?;
    Ok(PeerIdentity::from_secret_bytes(&stored)?)
}

#[cfg(test)]
mod tests;
