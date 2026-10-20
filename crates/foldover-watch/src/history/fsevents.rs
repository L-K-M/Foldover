//! FSEvents bindings for the event history: the current event ID, a
//! volume's history ID, and a replay from an earlier event. Declarations
//! follow FSEvents.h, CFArray.h, CFString.h, CFUUID.h and
//! dispatch/queue.h; ADR 0002 measured the behavior relied on here.

use std::ffi::{CStr, OsStr, c_char, c_void};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

type CFAllocatorRef = *const c_void;
type CFArrayRef = *const c_void;
type CFStringRef = *const c_void;
type CFUUIDRef = *const c_void;
type FSEventStreamRef = *mut c_void;
type DispatchQueue = *mut c_void;

/// One callback's events: each path with its flags.
type Batch = Vec<(PathBuf, u32)>;

/// FSEvents.h `FSEventStreamCallback`.
type Callback = unsafe extern "C" fn(
    stream: *const c_void,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
);

/// FSEvents.h `FSEventStreamContext`; the callbacks stay NULL because
/// `info` outlives the stream.
#[repr(C)]
struct StreamContext {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

/// A CoreFoundation symbol used only by address.
#[repr(C)]
struct Opaque {
    _private: [u8; 0],
}

/// CFString.h `kCFStringEncodingUTF8`.
const UTF8: u32 = 0x0800_0100;
/// Room for a UUID's 36 characters and the terminating NUL.
const UUID_TEXT: usize = 64;
const QUEUE_LABEL: &CStr = c"ch.lkmc.foldover.history";

// FSEvents.h stream creation flags.
const NO_DEFER: u32 = 0x02;
const WATCH_ROOT: u32 = 0x04;
const FILE_EVENTS: u32 = 0x10;

// FSEvents.h event flags.
const MUST_SCAN_SUBDIRS: u32 = 0x01;
const USER_DROPPED: u32 = 0x02;
const KERNEL_DROPPED: u32 = 0x04;
const IDS_WRAPPED: u32 = 0x08;
const HISTORY_DONE: u32 = 0x10;
const ROOT_CHANGED: u32 = 0x20;
const MOUNT: u32 = 0x40;
const UNMOUNT: u32 = 0x80;
/// After these the history no longer accounts for every change.
const INCOMPLETE: u32 = MUST_SCAN_SUBDIRS
    | USER_DROPPED
    | KERNEL_DROPPED
    | IDS_WRAPPED
    | ROOT_CHANGED
    | MOUNT
    | UNMOUNT;

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventsGetCurrentEventId() -> u64;
    fn FSEventsCopyUUIDForDevice(device: i32) -> CFUUIDRef;
    fn FSEventStreamCreate(
        allocator: CFAllocatorRef,
        callback: Callback,
        context: *mut StreamContext,
        paths: CFArrayRef,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> FSEventStreamRef;
    fn FSEventStreamSetDispatchQueue(stream: FSEventStreamRef, queue: DispatchQueue);
    fn FSEventStreamStart(stream: FSEventStreamRef) -> u8;
    fn FSEventStreamStop(stream: FSEventStreamRef);
    fn FSEventStreamInvalidate(stream: FSEventStreamRef);
    fn FSEventStreamRelease(stream: FSEventStreamRef);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeArrayCallBacks: Opaque;
    fn CFArrayCreate(
        allocator: CFAllocatorRef,
        values: *const *const c_void,
        count: isize,
        callbacks: *const Opaque,
    ) -> CFArrayRef;
    fn CFStringCreateWithBytes(
        allocator: CFAllocatorRef,
        bytes: *const u8,
        count: isize,
        encoding: u32,
        external: u8,
    ) -> CFStringRef;
    fn CFUUIDCreateString(allocator: CFAllocatorRef, uuid: CFUUIDRef) -> CFStringRef;
    fn CFStringGetCString(
        string: CFStringRef,
        buffer: *mut c_char,
        size: isize,
        encoding: u32,
    ) -> u8;
    fn CFRelease(object: *const c_void);
}

// libdispatch, part of libSystem.
unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attributes: *const c_void) -> DispatchQueue;
    fn dispatch_sync_f(
        queue: DispatchQueue,
        context: *mut c_void,
        work: unsafe extern "C" fn(*mut c_void),
    );
    fn dispatch_release(object: DispatchQueue);
}

/// A CoreFoundation object this module owns (the Create and Copy rules).
struct Owned(*const c_void);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a Create or Copy function and
            // is released exactly once, here.
            unsafe { CFRelease(self.0) };
        }
    }
}

pub(super) fn current_event() -> u64 {
    // SAFETY: takes no arguments and only reads the system's event counter.
    unsafe { FSEventsGetCurrentEventId() }
}

pub(super) fn history_id(path: &Path) -> io::Result<Option<String>> {
    // `dev_t` is a signed 32-bit number that std widens to u64.
    let device = i32::try_from(std::fs::metadata(path)?.dev().cast_signed())
        .map_err(|_| io::Error::other("The volume's device number is out of range"))?;
    // SAFETY: reads the device's history ID; returns NULL or a CFUUID the
    // caller owns under the Copy rule.
    let uuid = Owned(unsafe { FSEventsCopyUUIDForDevice(device) });
    if uuid.0.is_null() {
        return Ok(None);
    }
    // SAFETY: `uuid` is a valid CFUUID; the result is an owned CFString
    // or NULL.
    let text = Owned(unsafe { CFUUIDCreateString(ptr::null(), uuid.0) });
    if text.0.is_null() {
        return Err(unreadable_id());
    }
    let mut buffer = [0 as c_char; UUID_TEXT];
    // SAFETY: `text` is a valid CFString and the buffer is writable for
    // the length given; on success the result is NUL-terminated.
    let copied =
        unsafe { CFStringGetCString(text.0, buffer.as_mut_ptr(), UUID_TEXT as isize, UTF8) };
    if copied == 0 {
        return Err(unreadable_id());
    }
    // SAFETY: CFStringGetCString succeeded, so the buffer holds a
    // NUL-terminated string within its bounds.
    let id = unsafe { CStr::from_ptr(buffer.as_ptr()) };
    Ok(Some(id.to_string_lossy().into_owned()))
}

fn unreadable_id() -> io::Error {
    io::Error::other("Cannot read the volume's event history ID")
}

pub(super) fn changed_since(
    root: &Path,
    since: u64,
    timeout: Duration,
    relevant: impl Fn(&Path) -> bool,
) -> io::Result<bool> {
    let (sender, receiver) = mpsc::channel::<Batch>();
    // Boxed so the address the callback holds stays put; declared before
    // the stream, so the stream is dropped, and stops calling, first.
    let sender = Box::new(sender);
    let _stream = Stream::open(root, since, &sender)?;
    // A timeout too long to represent waits as long as the stream lives.
    let deadline = Instant::now().checked_add(timeout);
    loop {
        let left = deadline.map_or(Duration::MAX, |deadline| {
            deadline.saturating_duration_since(Instant::now())
        });
        let batch = receiver.recv_timeout(left).map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "The event history did not answer in time",
            )
        })?;
        for (path, flags) in batch {
            if flags & INCOMPLETE != 0 {
                return Err(io::Error::other("The event history is incomplete"));
            }
            if flags & HISTORY_DONE != 0 {
                return Ok(false);
            }
            if relevant(&path) {
                return Ok(true);
            }
        }
    }
}

/// A started stream replaying `root`'s history on its own serial queue.
struct Stream {
    stream: FSEventStreamRef,
    queue: DispatchQueue,
}

impl Stream {
    fn open(root: &Path, since: u64, sender: &Sender<Batch>) -> io::Result<Self> {
        let bytes = root.as_os_str().as_bytes();
        let length = isize::try_from(bytes.len()).map_err(|_| unopenable())?;
        // SAFETY: `bytes` is valid for `length` bytes, which are copied
        // (not external); the result is an owned CFString, or NULL when
        // they are not UTF-8.
        let path =
            Owned(unsafe { CFStringCreateWithBytes(ptr::null(), bytes.as_ptr(), length, UTF8, 0) });
        if path.0.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "The folder's path is not UTF-8",
            ));
        }
        let values = [path.0];
        // SAFETY: one valid CFString; with the CFType callbacks the array
        // retains it, and the array is owned here.
        let paths = Owned(unsafe {
            CFArrayCreate(
                ptr::null(),
                values.as_ptr(),
                1,
                &raw const kCFTypeArrayCallBacks,
            )
        });
        if paths.0.is_null() {
            return Err(unopenable());
        }
        let mut context = StreamContext {
            version: 0,
            info: ptr::from_ref(sender).cast_mut().cast(),
            retain: ptr::null(),
            release: ptr::null(),
            copy_description: ptr::null(),
        };
        // SAFETY: `on_events` matches FSEventStreamCallback; the context is
        // copied during the call; `info` points at a Sender that outlives
        // the stream (see `changed_since` and `Drop`); `paths` is a valid
        // array of CFStrings, retained by the stream as needed.
        let stream = unsafe {
            FSEventStreamCreate(
                ptr::null(),
                on_events,
                &raw mut context,
                paths.0,
                since,
                0.0,
                NO_DEFER | WATCH_ROOT | FILE_EVENTS,
            )
        };
        if stream.is_null() {
            return Err(unopenable());
        }
        // SAFETY: the label is a static NUL-terminated string; NULL
        // attributes make a serial queue, which this module releases.
        let queue = unsafe { dispatch_queue_create(QUEUE_LABEL.as_ptr(), ptr::null()) };
        if queue.is_null() {
            // SAFETY: the stream was created above and never scheduled.
            unsafe { FSEventStreamRelease(stream) };
            return Err(unopenable());
        }
        // SAFETY: both are valid; callbacks run on this queue from now on.
        unsafe { FSEventStreamSetDispatchQueue(stream, queue) };
        // SAFETY: the stream is scheduled, as starting requires.
        if unsafe { FSEventStreamStart(stream) } == 0 {
            // SAFETY: scheduled but never started: invalidate before
            // releasing, then release the queue the stream no longer uses.
            unsafe {
                FSEventStreamInvalidate(stream);
                FSEventStreamRelease(stream);
                dispatch_release(queue);
            }
            return Err(unopenable());
        }
        Ok(Self { stream, queue })
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: `open` created, scheduled and started both. Stopping ends
        // deliveries; an empty synchronous block on the serial queue waits
        // out a callback already running, so none reads the Sender after
        // this; then the stream is invalidated and released, and the queue
        // released.
        unsafe {
            FSEventStreamStop(self.stream);
            dispatch_sync_f(self.queue, ptr::null_mut(), nothing);
            FSEventStreamInvalidate(self.stream);
            FSEventStreamRelease(self.stream);
            dispatch_release(self.queue);
        }
    }
}

fn unopenable() -> io::Error {
    io::Error::other("Cannot open the folder's event history")
}

unsafe extern "C" fn nothing(_context: *mut c_void) {}

/// Forward one callback's events to the replaying thread. Must not
/// panic: it is called from C.
unsafe extern "C" fn on_events(
    _stream: *const c_void,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    _ids: *const u64,
) {
    if count == 0 || paths.is_null() || flags.is_null() || info.is_null() {
        return;
    }
    // SAFETY: `info` is the Sender the stream was created with, alive
    // until `Stream::drop` has drained this queue.
    let sender = unsafe { &*info.cast::<Sender<Batch>>() };
    // SAFETY: without kFSEventStreamCreateFlagUseCFTypes, `paths` is an
    // array of `count` C strings and `flags` one of `count` flags, both
    // valid for this call (FSEvents.h).
    let (paths, flags) = unsafe {
        (
            std::slice::from_raw_parts(paths.cast::<*const c_char>().cast_const(), count),
            std::slice::from_raw_parts(flags, count),
        )
    };
    let batch = paths
        .iter()
        .zip(flags)
        .filter(|(path, _)| !path.is_null())
        .map(|(path, flags)| {
            // SAFETY: a non-null entry is a NUL-terminated path FSEvents
            // owns for this call; it is copied out.
            let path = unsafe { CStr::from_ptr(*path) };
            (PathBuf::from(OsStr::from_bytes(path.to_bytes())), *flags)
        })
        .collect();
    let _ = sender.send(batch);
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::thread::sleep;

    use super::*;

    /// Long enough for the history to record recent events, as ADR 0002
    /// measured: replaying right after a write can miss the last one.
    const SETTLE: Duration = Duration::from_secs(3);
    /// The budget the engine gives a replay.
    const ANSWER: Duration = Duration::from_secs(30);

    #[test]
    fn the_history_replays_what_changed_after_an_event() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let (quiet, busy) = (root.join("quiet"), root.join("busy"));
        fs::create_dir(&quiet).unwrap();
        fs::create_dir(&busy).unwrap();
        sleep(SETTLE);
        let since = current_event();

        fs::write(busy.join("file"), b"new").unwrap();
        sleep(SETTLE);
        assert!(changed_since(&busy, since, ANSWER, |_| true).unwrap());
        assert!(!changed_since(&quiet, since, ANSWER, |_| true).unwrap());
        // What the caller does not count is no change.
        assert!(!changed_since(&busy, since, ANSWER, |_| false).unwrap());
        assert!(history_id(&root).unwrap().is_some());
        assert!(current_event() >= since);
    }
}
