//! Whether a disk is solid state or spinning, from the I/O Registry. The
//! storage device above a volume's BSD disk carries "Device
//! Characteristics" with a "Medium Type":
//!
//! ```text
//! disk3s5 (IOMedia) → … → disk0 (IOMedia) → IOBlockStorageDriver
//!   → IONVMeBlockStorageDevice  "Device Characteristics" = {
//!         "Medium Type" = "Solid State" }
//! ```
//!
//! Disk images and some USB enclosures report none.

use std::ffi::{CStr, CString, c_char};
use std::ptr;

use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::{CFString, CFStringRef};
use core_foundation_sys::base::{CFAllocatorRef, CFTypeRef};
use core_foundation_sys::dictionary::{CFDictionaryRef, CFMutableDictionaryRef};

use crate::Medium;

/// `mach_port_t`, which `io_object_t` and its kinds are.
type MachPort = u32;
type KernReturn = i32;

/// `kIOMainPortDefault`, which is `MACH_PORT_NULL`.
const MAIN_PORT_DEFAULT: MachPort = 0;
/// `IO_OBJECT_NULL`.
const NO_OBJECT: MachPort = 0;
const KERN_SUCCESS: KernReturn = 0;
const SERVICE_PLANE: &CStr = c"IOService";
const CHARACTERISTICS: &str = "Device Characteristics";
const MEDIUM_TYPE: &str = "Medium Type";
const SOLID_STATE: &str = "Solid State";
const ROTATIONAL: &str = "Rotational";
/// Registry levels walked up from a disk; real chains are about a dozen.
const MAX_DEPTH: usize = 64;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOBSDNameMatching(
        main_port: MachPort,
        options: u32,
        name: *const c_char,
    ) -> CFMutableDictionaryRef;
    fn IOServiceGetMatchingService(main_port: MachPort, matching: CFDictionaryRef) -> MachPort;
    fn IORegistryEntryGetParentEntry(
        entry: MachPort,
        plane: *const c_char,
        parent: *mut MachPort,
    ) -> KernReturn;
    fn IORegistryEntryCreateCFProperty(
        entry: MachPort,
        key: CFStringRef,
        allocator: CFAllocatorRef,
        options: u32,
    ) -> CFTypeRef;
    fn IOObjectRelease(object: MachPort) -> KernReturn;
}

/// A registry entry this module holds one reference to.
struct Entry(MachPort);

impl Drop for Entry {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a live object this module owns one reference
        // to, released exactly once, here.
        unsafe { IOObjectRelease(self.0) };
    }
}

impl Entry {
    /// The entry for BSD disk `name`, e.g. `disk3s5`.
    fn disk(name: &str) -> Option<Self> {
        let name = CString::new(name).ok()?;
        // SAFETY: `name` is NUL-terminated and outlives the call, which
        // copies it. The result is null or a dictionary this function owns.
        let matching = unsafe { IOBSDNameMatching(MAIN_PORT_DEFAULT, 0, name.as_ptr()) };
        if matching.is_null() {
            return None;
        }
        // SAFETY: the call consumes the one reference to `matching` this
        // function owns, which is not used again. The result is
        // `NO_OBJECT` or an object this function owns.
        let service = unsafe { IOServiceGetMatchingService(MAIN_PORT_DEFAULT, matching) };
        (service != NO_OBJECT).then_some(Self(service))
    }

    fn parent(&self) -> Option<Self> {
        let mut parent = NO_OBJECT;
        // SAFETY: `self.0` is live, the plane name is static and
        // NUL-terminated, and `parent` is a valid place for the result,
        // which on success is an object this function owns.
        let result = unsafe {
            IORegistryEntryGetParentEntry(self.0, SERVICE_PLANE.as_ptr(), &raw mut parent)
        };
        (result == KERN_SUCCESS && parent != NO_OBJECT).then_some(Self(parent))
    }

    /// The entry's own "Medium Type", if it reports one.
    fn medium(&self) -> Option<Medium> {
        let key = CFString::from_static_string(CHARACTERISTICS);
        // SAFETY: `self.0` is live and `key` outlives the synchronous call.
        // A null allocator is the default one. The result is null or a
        // property owned under the Create rule.
        let property = unsafe {
            IORegistryEntryCreateCFProperty(self.0, key.as_concrete_TypeRef(), ptr::null(), 0)
        };
        if property.is_null() {
            return None;
        }
        // SAFETY: non-null and owned under the Create rule; the wrapper
        // releases it once.
        let property = unsafe { CFType::wrap_under_create_rule(property) };
        let characteristics = property.downcast_into::<CFDictionary>()?;
        let key = CFString::from_static_string(MEDIUM_TYPE);
        let value = *characteristics.find(key.as_CFTypeRef())?;
        // SAFETY: a value of a live dictionary is a live object; the Get
        // rule retains it for the wrapper.
        let value = unsafe { CFType::wrap_under_get_rule(value) };
        match value.downcast::<CFString>()?.to_string().as_str() {
            SOLID_STATE => Some(Medium::SolidState),
            ROTATIONAL => Some(Medium::Rotational),
            _ => None,
        }
    }
}

/// The medium of BSD disk `name`: the nearest entry above it that says.
pub(crate) fn of_disk(name: &str) -> Medium {
    let mut entry = Entry::disk(name);
    for _ in 0..MAX_DEPTH {
        let Some(current) = entry else {
            break;
        };
        if let Some(medium) = current.medium() {
            return medium;
        }
        entry = current.parent();
    }
    Medium::Unknown
}
