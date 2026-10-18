//! Audited macOS binding: exact-key, noninteractive SecItem operations.
//! Native pointers stay inside this module and become owned CF values.

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::data::CFData;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use security_framework::os::macos::keychain::SecKeychain;
use security_framework_sys::base::{errSecDuplicateItem, errSecItemNotFound, errSecSuccess};
use security_framework_sys::item::{
    kSecAttrAccount, kSecAttrLabel, kSecAttrService, kSecAttrSynchronizable, kSecClass,
    kSecClassGenericPassword, kSecMatchSearchList, kSecReturnData, kSecUseAuthenticationUI,
    kSecUseKeychain, kSecValueData,
};
use security_framework_sys::keychain_item::{SecItemAdd, SecItemCopyMatching};
use zeroize::Zeroizing;

use super::{IdentityStoreError, PeerIdentityStore};

const SERVICE: &str = "ch.lkmc.foldover.peer-identity";
const ACCOUNT: &str = "local-device";
const LABEL: &str = "Foldover peer identity";
const MAX_SECRET_BYTES: usize = 64 * 1024;

// These SDK declarations are missing from security-framework-sys 2.17.
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecUseAuthenticationUIFail: core_foundation::string::CFStringRef;

    #[cfg(test)]
    fn SecKeychainDelete(keychain: security_framework_sys::base::SecKeychainRef) -> i32;
    #[cfg(test)]
    fn SecKeychainLock(keychain: security_framework_sys::base::SecKeychainRef) -> i32;
}

/// One item in the user's default file Keychain. Its ACL and lock policy stay
/// under Keychain control; no synchronized or plaintext fallback is used.
pub struct KeychainIdentityStore {
    keychain: SecKeychain,
}

impl KeychainIdentityStore {
    pub fn open() -> Result<Self, IdentityStoreError> {
        let keychain =
            SecKeychain::default().map_err(|error| keychain_error("open", error.code()))?;
        Ok(Self { keychain })
    }

    fn query(&self) -> Vec<(CFString, CFType)> {
        // SAFETY: Security.framework exports these immutable CFString constants
        // for the lifetime of the process. Get-rule wrappers retain them.
        unsafe {
            vec![
                (
                    string(kSecClass),
                    string(kSecClassGenericPassword).into_CFType(),
                ),
                (
                    string(kSecAttrService),
                    CFString::new(SERVICE).into_CFType(),
                ),
                (
                    string(kSecAttrAccount),
                    CFString::new(ACCOUNT).into_CFType(),
                ),
                (
                    string(kSecAttrSynchronizable),
                    CFBoolean::false_value().into_CFType(),
                ),
                (
                    string(kSecUseAuthenticationUI),
                    string(kSecUseAuthenticationUIFail).into_CFType(),
                ),
            ]
        }
    }
}

// SAFETY: Callers supply only Security.framework's process-lifetime constants.
unsafe fn string(value: core_foundation::string::CFStringRef) -> CFString {
    // SAFETY: The caller guarantees a live CFString; this retains it.
    unsafe { CFString::wrap_under_get_rule(value) }
}

fn keychain_error(operation: &'static str, status: i32) -> IdentityStoreError {
    IdentityStoreError::Keychain { operation, status }
}

impl PeerIdentityStore for KeychainIdentityStore {
    fn read(&self) -> Result<Option<Zeroizing<Vec<u8>>>, IdentityStoreError> {
        let mut query = self.query();
        // SAFETY: Constants are live CFStrings; the array retains the keychain.
        unsafe {
            query.extend([
                (
                    string(kSecMatchSearchList),
                    CFArray::from_CFTypes(std::slice::from_ref(&self.keychain)).into_CFType(),
                ),
                (
                    string(kSecReturnData),
                    CFBoolean::true_value().into_CFType(),
                ),
            ]);
        }
        let query = CFDictionary::from_CFType_pairs(&query);
        let mut value = std::ptr::null();
        // SAFETY: The dictionary owns all parameters and `value` is a writable
        // output slot. On success the Copy rule transfers one owned reference.
        let status = unsafe { SecItemCopyMatching(query.as_concrete_TypeRef(), &mut value) };
        if status == errSecItemNotFound {
            return Ok(None);
        }
        if status != errSecSuccess {
            return Err(keychain_error("read", status));
        }
        if value.is_null() {
            return Err(IdentityStoreError::InvalidData);
        }
        // SAFETY: A successful CopyMatching call returned an owned CF object.
        let value = unsafe { CFType::wrap_under_create_rule(value) };
        let data = value
            .downcast::<CFData>()
            .ok_or(IdentityStoreError::InvalidData)?;
        if data.is_empty() || data.bytes().len() > MAX_SECRET_BYTES {
            return Err(IdentityStoreError::InvalidData);
        }
        Ok(Some(Zeroizing::new(data.bytes().to_vec())))
    }

    fn create_if_absent(&self, bytes: &[u8]) -> Result<(), IdentityStoreError> {
        if bytes.is_empty() || bytes.len() > MAX_SECRET_BYTES {
            return Err(IdentityStoreError::InvalidData);
        }
        let mut query = self.query();
        // SAFETY: Constants are live CFStrings; values retain the keychain and
        // copy the bytes for the duration of SecItemAdd.
        unsafe {
            query.extend([
                (string(kSecUseKeychain), self.keychain.as_CFType()),
                (string(kSecAttrLabel), CFString::new(LABEL).into_CFType()),
                (
                    string(kSecValueData),
                    CFData::from_buffer(bytes).into_CFType(),
                ),
            ]);
        }
        // SecItemAdd is exclusive. The password setters would overwrite a
        // concurrent creator's identity and invalidate its device fingerprint.
        let query = CFDictionary::from_CFType_pairs(&query);
        // SAFETY: The dictionary owns all parameters. A null output slot asks
        // SecItemAdd not to return an object, so there is no reference to free.
        let status = unsafe { SecItemAdd(query.as_concrete_TypeRef(), std::ptr::null_mut()) };
        if status == errSecSuccess || status == errSecDuplicateItem {
            return Ok(());
        }
        Err(keychain_error("create", status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use security_framework::os::macos::keychain::CreateOptions;

    const PASSWORD: &str = "Foldover temporary Keychain test";

    struct Fixture {
        store: KeychainIdentityStore,
        _directory: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let keychain = CreateOptions::new()
                .password(PASSWORD)
                .create(directory.path().join("peer-identity.keychain"))
                .unwrap();
            Self {
                store: KeychainIdentityStore { keychain },
                _directory: directory,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // SAFETY: This handle refers only to this fixture's temporary
            // keychain. Delete also removes it from the system search list.
            let status = unsafe { SecKeychainDelete(self.store.keychain.as_concrete_TypeRef()) };
            if !std::thread::panicking() {
                assert_eq!(status, errSecSuccess, "temporary Keychain cleanup failed");
            }
        }
    }

    #[test]
    fn native_storage_reopens_and_never_replaces_an_identity() {
        let fixture = Fixture::new();
        assert!(fixture.store.read().unwrap().is_none());
        fixture.store.create_if_absent(b"first identity").unwrap();
        fixture.store.create_if_absent(b"replacement").unwrap();

        let path = fixture._directory.path().join("peer-identity.keychain");
        let reopened = KeychainIdentityStore {
            keychain: SecKeychain::open(path).unwrap(),
        };
        assert_eq!(
            reopened.read().unwrap().unwrap().as_slice(),
            b"first identity"
        );
        assert!(matches!(
            fixture.store.create_if_absent(&[]),
            Err(IdentityStoreError::InvalidData)
        ));
        assert!(matches!(
            fixture
                .store
                .create_if_absent(&vec![0; MAX_SECRET_BYTES + 1]),
            Err(IdentityStoreError::InvalidData)
        ));
    }

    #[test]
    fn a_locked_keychain_fails_without_becoming_an_absent_identity() {
        let fixture = Fixture::new();
        fixture
            .store
            .create_if_absent(b"original identity")
            .unwrap();
        // SAFETY: This locks only the fixture's valid temporary Keychain.
        assert_eq!(
            unsafe { SecKeychainLock(fixture.store.keychain.as_concrete_TypeRef()) },
            errSecSuccess
        );
        assert!(matches!(
            fixture.store.read(),
            Err(IdentityStoreError::Keychain { .. })
        ));

        let mut keychain = fixture.store.keychain.clone();
        keychain.unlock(Some(PASSWORD)).unwrap();
        assert_eq!(
            fixture.store.read().unwrap().unwrap().as_slice(),
            b"original identity"
        );
    }
}
