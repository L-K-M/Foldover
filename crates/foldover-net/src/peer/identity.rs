use rustls::SignatureAlgorithm;
use rustls::sign::CertifiedKey;
use zeroize::Zeroizing;

use super::{
    CertificateDer, DeviceId, Identity, PeerError, PrivateKeyDer, PrivatePkcs8KeyDer, provider,
};

const FORMAT: &[u8] = b"foldover-peer-identity-v1\0";
const LENGTH_BYTES: usize = size_of::<u32>();
const MAX_IDENTITY_BYTES: usize = 16 * 1024;

impl Identity {
    /// Encode the certificate and private key for a credential store only.
    /// Keep the certificate bytes: reissuing it changes the pinned device ID.
    pub fn to_secret_bytes(&self) -> Result<Zeroizing<Vec<u8>>, PeerError> {
        let mut bytes = Zeroizing::new(Vec::with_capacity(
            FORMAT.len() + LENGTH_BYTES + self.cert.len() + self.key.secret_der().len(),
        ));
        let length = u32::try_from(self.cert.len()).map_err(|_| PeerError::InvalidIdentity)?;
        bytes.extend_from_slice(FORMAT);
        bytes.extend_from_slice(&length.to_be_bytes());
        bytes.extend_from_slice(self.cert.as_ref());
        bytes.extend_from_slice(self.key.secret_der());
        if bytes.len() > MAX_IDENTITY_BYTES {
            return Err(PeerError::InvalidIdentity);
        }
        Ok(bytes)
    }

    /// Restore an exact Ed25519 identity, rejecting corrupt or mismatched keys.
    /// Never replace a rejected identity with a generated one.
    pub fn from_secret_bytes(bytes: &[u8]) -> Result<Self, PeerError> {
        if bytes.len() > MAX_IDENTITY_BYTES {
            return Err(PeerError::InvalidIdentity);
        }
        let bytes = bytes
            .strip_prefix(FORMAT)
            .ok_or(PeerError::InvalidIdentity)?;
        let (length, bytes) = bytes
            .split_at_checked(LENGTH_BYTES)
            .ok_or(PeerError::InvalidIdentity)?;
        let length = u32::from_be_bytes(length.try_into().map_err(|_| PeerError::InvalidIdentity)?);
        let (cert, key) = bytes
            .split_at_checked(length as usize)
            .ok_or(PeerError::InvalidIdentity)?;
        let cert = CertificateDer::from(cert.to_vec());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.to_vec()));
        let certified = CertifiedKey::from_der(vec![cert.clone()], key.clone_key(), &provider())
            .map_err(|_| PeerError::InvalidIdentity)?;
        if certified.key.algorithm() != SignatureAlgorithm::ED25519
            || certified.keys_match().is_err()
        {
            return Err(PeerError::InvalidIdentity);
        }
        Ok(Self {
            id: DeviceId::of(&cert),
            cert,
            key,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restoring_keeps_the_certificate_and_device_id() {
        let original = Identity::generate().unwrap();
        let encoded = original.to_secret_bytes().unwrap();
        let restored = Identity::from_secret_bytes(&encoded).unwrap();
        assert_eq!(original.id(), restored.id());
        assert_eq!(original.cert, restored.cert);
        assert_eq!(*encoded, *restored.to_secret_bytes().unwrap());
    }

    #[test]
    fn malformed_versions_lengths_certificates_and_keys_are_refused() {
        let original = Identity::generate().unwrap().to_secret_bytes().unwrap();
        for length in 0..original.len() {
            assert!(Identity::from_secret_bytes(&original[..length]).is_err());
        }
        for offset in [
            0,
            FORMAT.len(),
            FORMAT.len() + LENGTH_BYTES,
            original.len() - 1,
        ] {
            let mut corrupt = original.clone();
            corrupt[offset] ^= u8::MAX;
            assert!(Identity::from_secret_bytes(&corrupt).is_err());
        }
        let mut trailing = original.clone();
        trailing.push(0);
        assert!(Identity::from_secret_bytes(&trailing).is_err());
        assert!(Identity::from_secret_bytes(&vec![0; MAX_IDENTITY_BYTES + 1]).is_err());
    }

    #[test]
    fn another_private_key_is_refused() {
        let mut original = Identity::generate().unwrap();
        original.key = Identity::generate().unwrap().key;
        assert!(Identity::from_secret_bytes(&original.to_secret_bytes().unwrap()).is_err());
    }
}
