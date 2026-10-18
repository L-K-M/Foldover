use zeroize::Zeroizing;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod keychain;

#[cfg(target_os = "macos")]
pub use keychain::KeychainIdentityStore;

#[derive(Debug, thiserror::Error)]
pub enum IdentityStoreError {
    #[error("Peer identity storage is not available on this platform")]
    Unsupported,
    #[error(
        "Keychain {operation} failed (OSStatus {status}); check the login Keychain and agent access"
    )]
    Keychain {
        operation: &'static str,
        status: i32,
    },
    #[error("The Keychain returned invalid peer identity data")]
    InvalidData,
}

/// Credential storage for one local peer identity. Implementations must not
/// overwrite an existing identity or treat access failures as an absent item.
pub trait PeerIdentityStore {
    fn read(&self) -> Result<Option<Zeroizing<Vec<u8>>>, IdentityStoreError>;

    /// Preserve an existing item, including one created concurrently. Read it
    /// again afterwards to obtain the identity that won the exclusive create.
    fn create_if_absent(&self, bytes: &[u8]) -> Result<(), IdentityStoreError>;
}
