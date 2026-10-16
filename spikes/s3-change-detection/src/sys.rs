//! Hand-written `extern "C"` declarations for the frameworks this spike
//! measures: FSEvents (linked through CoreServices, which re-exports the
//! FSEvents subframework), DiskArbitration, the CoreFoundation helpers both
//! need, and the libdispatch entry points used to schedule streams. Each
//! declaration follows the C prototype in the SDK header named above it.
//! CF object reference types come from the core-foundation crate so its
//! `CFString`/`CFArray` builders can be used at the call sites.

use std::ffi::{c_char, c_int, c_void};

// Scalar and opaque-reference C types used by the declared prototypes.
// All CF refs are opaque pointers; defining them here keeps this module
// the single place that knows the ABI shapes.
#[allow(non_camel_case_types)]
pub type dev_t = i32;
pub type CFStringRef = *const c_void;
pub type CFArrayRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFNumberRef = *const c_void;
pub type CFUUIDRef = *const c_void;
pub type CFURLRef = *const c_void;
pub type CFRunLoopRef = *mut c_void;
pub type CFIndex = i64;
pub type CFTypeID = u64;
pub type CFTimeInterval = f64;
pub type CFAbsoluteTime = f64;
pub type Boolean = u8;
pub type CFAllocatorRef = *const c_void;
pub type CFTypeRef = *const c_void;
pub type CFNumberType = i32;
pub type CFStringEncoding = u32;
pub type FSEventStreamRef = *mut c_void;
pub type FSEventStreamEventId = u64;
pub type FSEventStreamEventFlags = u32;
pub type FSEventStreamCreateFlags = u32;
pub type DASessionRef = *mut c_void;
pub type DADiskRef = *const c_void;
pub type DispatchQueue = *mut c_void;

/// CFString.h `kCFStringEncodingUTF8`.
pub const K_CF_STRING_ENCODING_UTF8: CFStringEncoding = 0x0800_0100;
pub const K_CF_NUMBER_SINT64_TYPE: CFNumberType = 4;

/// FSEvents.h: `typedef struct FSEventStreamContext`.
/// `retain`/`release`/`copy_description` stay NULL: the spike keeps the
/// info pointer alive for the stream's whole lifetime instead.
#[repr(C)]
pub struct FSEventStreamContext {
    pub version: CFIndex,
    pub info: *mut c_void,
    pub retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    pub release: Option<unsafe extern "C" fn(*const c_void)>,
    pub copy_description: Option<unsafe extern "C" fn(*const c_void) -> CFStringRef>,
}

/// FSEvents.h: `typedef FSEventStreamCallback`. With
/// kFSEventStreamCreateFlagUseCFTypes `event_paths` is a CFArray whose
/// elements are CFDictionary (kFSEventStreamCreateFlagUseExtendedData) or
/// CFString otherwise.
pub type FSEventStreamCallback = unsafe extern "C" fn(
    stream_ref: FSEventStreamRef,
    client_callback_info: *mut c_void,
    num_events: usize,
    event_paths: *mut c_void,
    event_flags: *const FSEventStreamEventFlags,
    event_ids: *const FSEventStreamEventId,
);

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    pub fn FSEventStreamCreate(
        allocator: CFAllocatorRef,
        callback: FSEventStreamCallback,
        context: *const FSEventStreamContext,
        paths_to_watch: CFArrayRef,
        since_when: FSEventStreamEventId,
        latency: CFTimeInterval,
        flags: FSEventStreamCreateFlags,
    ) -> FSEventStreamRef;

    /// `paths_to_watch` entries are interpreted relative to the root of
    /// `device`. Historically restricted; this spike measures whether an
    /// unprivileged process can use it at all. The device comes fourth,
    /// before the paths, as in the C prototype.
    pub fn FSEventStreamCreateRelativeToDevice(
        allocator: CFAllocatorRef,
        callback: FSEventStreamCallback,
        context: *const FSEventStreamContext,
        device: dev_t,
        paths_to_watch: CFArrayRef,
        since_when: FSEventStreamEventId,
        latency: CFTimeInterval,
        flags: FSEventStreamCreateFlags,
    ) -> FSEventStreamRef;

    /// Schedules the stream on a libdispatch queue instead of a run loop.
    pub fn FSEventStreamSetDispatchQueue(stream: FSEventStreamRef, queue: DispatchQueue);
    pub fn FSEventStreamStart(stream: FSEventStreamRef) -> Boolean;
    pub fn FSEventStreamStop(stream: FSEventStreamRef);
    /// Unschedules the stream from its run loop or dispatch queue.
    pub fn FSEventStreamInvalidate(stream: FSEventStreamRef);
    pub fn FSEventStreamRelease(stream: FSEventStreamRef);

    /// Copy rule: the returned UUID must be released. NULL when the device
    /// has no FSEvents store.
    pub fn FSEventsCopyUUIDForDevice(dev: dev_t) -> CFUUIDRef;
    pub fn FSEventsGetLastEventIdForDeviceBeforeTime(
        dev: dev_t,
        time: CFAbsoluteTime,
    ) -> FSEventStreamEventId;
}

/// Keys of each `event_paths` dictionary when the stream was created with
/// kFSEventStreamCreateFlagUseExtendedData. FSEvents.h defines them as
/// `CFSTR()` macros, so there is no symbol to link against.
pub const FSEVENT_EXTENDED_DATA_PATH_KEY: &str = "path";
pub const FSEVENT_EXTENDED_FILE_ID_KEY: &str = "fileID";

#[link(name = "DiskArbitration", kind = "framework")]
unsafe extern "C" {
    /// DASession.h / DADisk.h. The session must be scheduled on a run loop
    /// (or dispatch queue) for callbacks to fire.
    pub fn DASessionCreate(allocator: CFAllocatorRef) -> DASessionRef;
    pub fn DASessionScheduleWithRunLoop(
        session: DASessionRef,
        run_loop: CFRunLoopRef,
        run_loop_mode: CFStringRef,
    );
    pub fn DASessionUnscheduleFromRunLoop(
        session: DASessionRef,
        run_loop: CFRunLoopRef,
        run_loop_mode: CFStringRef,
    );
    /// `match` of NULL registers for every disk.
    pub fn DARegisterDiskAppearedCallback(
        session: DASessionRef,
        match_: CFDictionaryRef,
        callback: DADiskCallback,
        context: *mut c_void,
    );
    pub fn DARegisterDiskDisappearedCallback(
        session: DASessionRef,
        match_: CFDictionaryRef,
        callback: DADiskCallback,
        context: *mut c_void,
    );
    pub fn DAUnregisterCallback(
        session: DASessionRef,
        callback: *const c_void,
        context: *mut c_void,
    );
    pub fn DADiskGetBSDName(disk: DADiskRef) -> *const c_char;
    /// Copy rule: the returned dictionary must be released.
    pub fn DADiskCopyDescription(disk: DADiskRef) -> CFDictionaryRef;
}

pub type DADiskCallback = unsafe extern "C" fn(disk: DADiskRef, context: *mut c_void);

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    pub fn CFStringGetTypeID() -> CFTypeID;
    pub fn CFDictionaryGetTypeID() -> CFTypeID;
    pub fn CFNumberGetTypeID() -> CFTypeID;
    pub fn CFArrayGetValueAtIndex(the_array: CFArrayRef, idx: CFIndex) -> *const c_void;
    pub fn CFDictionaryGetValue(the_dict: CFDictionaryRef, key: *const c_void) -> *const c_void;
    pub fn CFNumberGetValue(
        number: CFNumberRef,
        the_type: CFNumberType,
        value_ptr: *mut c_void,
    ) -> Boolean;
    /// Fast path: may return NULL for strings not stored as UTF-8; callers
    /// fall back to CFStringGetCString.
    pub fn CFStringGetCStringPtr(
        the_string: CFStringRef,
        encoding: CFStringEncoding,
    ) -> *const c_char;
    pub fn CFStringGetCString(
        the_string: CFStringRef,
        buffer: *mut c_char,
        buffer_size: CFIndex,
        encoding: CFStringEncoding,
    ) -> Boolean;
    pub fn CFStringGetLength(the_string: CFStringRef) -> CFIndex;
    pub fn CFStringGetMaximumSizeForEncoding(
        length: CFIndex,
        encoding: CFStringEncoding,
    ) -> CFIndex;
    /// Copy rule: the returned string must be released.
    pub fn CFUUIDCreateString(alloc: CFAllocatorRef, uuid: CFUUIDRef) -> CFStringRef;
    pub fn CFURLGetString(an_url: CFURLRef) -> CFStringRef;
    pub fn CFAbsoluteTimeGetCurrent() -> CFAbsoluteTime;

    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopRun();
    /// Stop and wake are safe to call from another thread; the wake makes
    /// the loop process the stop request promptly.
    pub fn CFRunLoopStop(rl: CFRunLoopRef);
    pub fn CFRunLoopWakeUp(rl: CFRunLoopRef);
    pub static kCFRunLoopDefaultMode: CFStringRef;
}

// libSystem: no #[link] needed, it is always linked.
unsafe extern "C" {
    /// A NULL attribute requests a serial queue.
    pub fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> DispatchQueue;
    /// `dispatch_sync_f` is the function-pointer variant of
    /// `dispatch_sync`; it avoids needing an ObjC block.
    pub fn dispatch_sync_f(
        queue: DispatchQueue,
        context: *mut c_void,
        work: unsafe extern "C" fn(*mut c_void),
    );
    pub fn dispatch_release(object: DispatchQueue);

    /// <sys/xattr.h>; position 0, options 0 for a plain user.* attribute.
    pub fn setxattr(
        path: *const c_char,
        name: *const c_char,
        value: *const c_void,
        size: usize,
        position: u32,
        options: c_int,
    ) -> c_int;
}

/// Convert a CFStringRef (borrowed) to a Rust String.
///
/// # Safety
/// `s` must be a valid CFStringRef or null.
pub unsafe fn cf_str_to_string(s: CFStringRef) -> Option<String> {
    if s.is_null() {
        return None;
    }
    unsafe {
        let fast = CFStringGetCStringPtr(s, K_CF_STRING_ENCODING_UTF8);
        if !fast.is_null() {
            return Some(
                std::ffi::CStr::from_ptr(fast)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        let len = CFStringGetLength(s);
        let cap = CFStringGetMaximumSizeForEncoding(len, K_CF_STRING_ENCODING_UTF8) + 1;
        let mut buf = vec![0u8; cap as usize];
        if CFStringGetCString(
            s,
            buf.as_mut_ptr() as *mut c_char,
            cap,
            K_CF_STRING_ENCODING_UTF8,
        ) == 0
        {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr(buf.as_ptr() as *const c_char)
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// The FSEvents store UUID for a device, or None when the device has none.
///
/// # Safety
/// Calls CoreServices; `dev` must be a valid dev_t.
pub unsafe fn fsevents_uuid_for_device(dev: dev_t) -> Option<String> {
    unsafe {
        let uuid = FSEventsCopyUUIDForDevice(dev);
        if uuid.is_null() {
            return None;
        }
        let s = CFUUIDCreateString(std::ptr::null(), uuid);
        let out = cf_str_to_string(s);
        if !s.is_null() {
            CFRelease(s as CFTypeRef);
        }
        CFRelease(uuid as CFTypeRef);
        out
    }
}
