//! NetFS bindings: `NetFSMountURLSync` with its UI disabled. NetFS looks
//! up Keychain credentials for the URL itself, as Finder's Connect to
//! Server does.

use std::io;
use std::path::PathBuf;
use std::ptr;

use core_foundation::array::CFArray;
use core_foundation::base::TCFType;
use core_foundation::dictionary::CFMutableDictionary;
use core_foundation::string::CFString;
use core_foundation::url::CFURL;
use core_foundation_sys::array::CFArrayRef;
use core_foundation_sys::base::kCFAllocatorDefault;
use core_foundation_sys::dictionary::CFMutableDictionaryRef;
use core_foundation_sys::string::CFStringRef;
use core_foundation_sys::url::{CFURLCreateWithString, CFURLRef};

/// `kNAUIOptionKey` and `kNAUIOptionNoUI` from NetFS.h.
const UI_OPTION: &str = "UIOption";
const NO_UI: &str = "NoUI";

#[link(name = "NetFS", kind = "framework")]
unsafe extern "C" {
    fn NetFSMountURLSync(
        url: CFURLRef,
        mountpath: CFURLRef,
        user: CFStringRef,
        passwd: CFStringRef,
        open_options: CFMutableDictionaryRef,
        mount_options: CFMutableDictionaryRef,
        mountpoints: *mut CFArrayRef,
    ) -> i32;
}

pub(crate) fn mount_share(url: &str) -> io::Result<Vec<PathBuf>> {
    let string = CFString::new(url);
    // SAFETY: `string` is a live CFString and a null base URL is allowed.
    // The result follows the create rule and is checked for null before
    // it is wrapped, which takes ownership.
    let url = unsafe {
        let created = CFURLCreateWithString(
            kCFAllocatorDefault,
            string.as_concrete_TypeRef(),
            ptr::null(),
        );
        if created.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Not a valid share URL",
            ));
        }
        CFURL::wrap_under_create_rule(created)
    };
    let mut open_options = CFMutableDictionary::<CFString, CFString>::new();
    open_options.add(&CFString::new(UI_OPTION), &CFString::new(NO_UI));

    let mut mountpoints: CFArrayRef = ptr::null();
    // SAFETY: every reference is a live CF object or null where NetFS
    // allows it (default mount directory, Keychain credentials, default
    // mount options). NetFS stores an owned array, or nothing, in
    // `mountpoints`.
    let status = unsafe {
        NetFSMountURLSync(
            url.as_concrete_TypeRef(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            open_options.as_concrete_TypeRef(),
            ptr::null_mut(),
            &mut mountpoints,
        )
    };
    let points = if mountpoints.is_null() {
        Vec::new()
    } else {
        // SAFETY: a non-null array from NetFS follows the create rule and
        // holds CFStrings.
        let array = unsafe { CFArray::<CFString>::wrap_under_create_rule(mountpoints) };
        array
            .iter()
            .map(|point| PathBuf::from(point.to_string()))
            .collect()
    };
    match status {
        0 => Ok(points),
        // Positive codes are errno values; negative ones NetFS's own.
        errno if errno > 0 => Err(io::Error::from_raw_os_error(errno)),
        code => Err(io::Error::other(format!("NetFS error {code}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreachable_shares_fail_without_asking() {
        assert!(mount_share("not a url").is_err());
        // Port 1 on loopback refuses at once; no dialog may appear.
        assert!(mount_share("smb://127.0.0.1:1/none").is_err());
    }
}
