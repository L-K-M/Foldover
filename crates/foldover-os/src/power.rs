//! IOKit power assertions: `IOPMAssertionCreateWithName` keeps the Mac
//! from sleeping when idle until `IOPMAssertionRelease`. `pmset -g
//! assertions` lists them by name.

use std::io;

use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::string::CFStringRef;

/// `IOPMAssertionID`.
type AssertionId = u32;
/// `IOReturn`, a `kern_return_t`.
type IoReturn = i32;

/// `kIOPMAssertPreventUserIdleSystemSleep` from IOPMLib.h.
const PREVENT_USER_IDLE_SYSTEM_SLEEP: &str = "PreventUserIdleSystemSleep";
/// `kIOPMAssertionLevelOn`.
const LEVEL_ON: u32 = 255;
/// `kIOReturnSuccess`.
const SUCCESS: IoReturn = 0;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionCreateWithName(
        kind: CFStringRef,
        level: u32,
        name: CFStringRef,
        id: *mut AssertionId,
    ) -> IoReturn;
    fn IOPMAssertionRelease(id: AssertionId) -> IoReturn;
}

pub(crate) struct Assertion {
    id: AssertionId,
}

impl Assertion {
    pub(crate) fn prevent_idle_sleep(reason: &str) -> io::Result<Self> {
        let kind = CFString::new(PREVENT_USER_IDLE_SYSTEM_SLEEP);
        let name = CFString::new(reason);
        let mut id: AssertionId = 0;
        // SAFETY: both strings are live CFStrings for the whole call, which
        // copies what it keeps, and `id` is a valid place for the result.
        let status = unsafe {
            IOPMAssertionCreateWithName(
                kind.as_concrete_TypeRef(),
                LEVEL_ON,
                name.as_concrete_TypeRef(),
                &mut id,
            )
        };
        if status != SUCCESS {
            return Err(io::Error::other(format!(
                "IOPMAssertionCreateWithName failed with {status:#x}"
            )));
        }
        Ok(Self { id })
    }
}

impl Drop for Assertion {
    fn drop(&mut self) {
        // SAFETY: `id` came from a successful create and is released once,
        // here. A failed release leaves nothing to retry: the assertion
        // ends with the process at the latest.
        let _ = unsafe { IOPMAssertionRelease(self.id) };
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use core_foundation::base::CFType;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation_sys::dictionary::CFDictionaryRef;

    use super::*;

    /// `kIOPMAssertionNameKey`.
    const NAME_KEY: &str = "AssertName";

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCopyProperties(id: AssertionId) -> CFDictionaryRef;
    }

    /// The assertion's name, or None once it no longer exists.
    fn name(id: AssertionId) -> Option<String> {
        // SAFETY: any id is accepted; an unknown one yields null. A
        // non-null result follows the create rule.
        let properties = unsafe { IOPMAssertionCopyProperties(id) };
        if properties.is_null() {
            return None;
        }
        // SAFETY: non-null, owned, and a dictionary keyed by CFStrings.
        let properties =
            unsafe { CFDictionary::<CFString, CFType>::wrap_under_create_rule(properties) };
        properties
            .find(CFString::new(NAME_KEY))
            .and_then(|value| value.downcast::<CFString>())
            .map(|name| name.to_string())
    }

    #[test]
    fn an_assertion_holds_until_dropped() {
        let assertion = Assertion::prevent_idle_sleep("Foldover power test").unwrap();
        let id = assertion.id;
        assert_eq!(name(id).as_deref(), Some("Foldover power test"));

        drop(assertion);
        // Release may reach powerd asynchronously; allow it a moment.
        let deadline = Instant::now() + Duration::from_secs(5);
        while name(id).is_some() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(name(id), None);
    }
}
