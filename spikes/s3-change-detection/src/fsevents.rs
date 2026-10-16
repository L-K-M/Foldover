//! Safe-enough wrapper over the sys layer for FSEvents streams: a serial
//! dispatch queue per stream, a mutex-guarded event log as the callback
//! context, and a drain barrier before the context is freed.

use crate::sys;
use core_foundation::array::CFArray;
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use std::ffi::CString;
use std::path::Path;
use std::ptr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// One event as the callback observed it.
pub struct EventRec {
    /// The path FSEvents reported; form depends on the stream kind.
    pub path: String,
    pub flags: u32,
    pub id: u64,
    pub at: Instant,
    /// kFSEventStreamEventExtendedFileIDKey value, when present.
    pub file_id: Option<i64>,
}

#[derive(Default)]
pub struct SharedInner {
    pub events: Vec<EventRec>,
    pub union_flags: u32,
}

/// Callback context. Lives in a Box whose address is handed to FSEvents as
/// `client_callback_info`; freed only after the dispatch queue drains.
pub struct SharedEvents {
    pub inner: Mutex<SharedInner>,
}

impl SharedEvents {
    pub fn inspect<R>(&self, f: impl FnOnce(&SharedInner) -> R) -> R {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&inner)
    }

    fn inspect_mut<R>(&self, f: impl FnOnce(&mut SharedInner) -> R) -> R {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut inner)
    }
}

/// Extract the path (and optional file ID) from one element of the callback's
/// path array. With UseExtendedData elements are CFDictionary; otherwise
/// CFString. Falls back to the string element when extended data is absent.
///
/// # Safety
/// `paths` must be the `event_paths` argument of a live FSEvents callback.
unsafe fn extract_path(paths: sys::CFArrayRef, i: sys::CFIndex) -> (String, Option<i64>) {
    unsafe {
        let el = sys::CFArrayGetValueAtIndex(paths, i);
        if el.is_null() {
            return (String::new(), None);
        }
        let tid = sys::CFGetTypeID(el);
        if tid == sys::CFDictionaryGetTypeID() {
            let dict = el as sys::CFDictionaryRef;
            let path_key = CFString::from_static_string(sys::FSEVENT_EXTENDED_DATA_PATH_KEY);
            let file_id_key = CFString::from_static_string(sys::FSEVENT_EXTENDED_FILE_ID_KEY);
            let path_val = sys::CFDictionaryGetValue(dict, path_key.as_concrete_TypeRef().cast());
            let path = sys::cf_str_to_string(path_val as sys::CFStringRef)
                .unwrap_or_else(|| "<unreadable path>".into());
            let fid_val = sys::CFDictionaryGetValue(dict, file_id_key.as_concrete_TypeRef().cast());
            let file_id =
                if !fid_val.is_null() && sys::CFGetTypeID(fid_val) == sys::CFNumberGetTypeID() {
                    let mut v: i64 = 0;
                    if sys::CFNumberGetValue(
                        fid_val as sys::CFNumberRef,
                        sys::K_CF_NUMBER_SINT64_TYPE,
                        &mut v as *mut i64 as *mut std::ffi::c_void,
                    ) != 0
                    {
                        Some(v)
                    } else {
                        None
                    }
                } else {
                    None
                };
            (path, file_id)
        } else if tid == sys::CFStringGetTypeID() {
            (
                sys::cf_str_to_string(el as sys::CFStringRef)
                    .unwrap_or_else(|| "<unreadable path>".into()),
                None,
            )
        } else {
            (String::new(), None)
        }
    }
}

/// FSEvents callback. Runs on the stream's serial dispatch queue; must not
/// panic or block. Pushes records into the shared log.
unsafe extern "C" fn on_events(
    _stream: sys::FSEventStreamRef,
    info: *mut std::ffi::c_void,
    num_events: usize,
    event_paths: *mut std::ffi::c_void,
    event_flags: *const u32,
    event_ids: *const u64,
) {
    if info.is_null() {
        return;
    }
    let shared = unsafe { &*(info as *const SharedEvents) };
    let paths = event_paths as sys::CFArrayRef;
    let now = Instant::now();
    shared.inspect_mut(|inner| {
        for i in 0..num_events {
            let flags = unsafe { *event_flags.add(i) };
            let id = unsafe { *event_ids.add(i) };
            let (path, file_id) = unsafe { extract_path(paths, i as sys::CFIndex) };
            inner.union_flags |= flags;
            inner.events.push(EventRec {
                path,
                flags,
                id,
                at: now,
                file_id,
            });
        }
    });
}

unsafe extern "C" fn noop(_ctx: *mut std::ffi::c_void) {}

/// A running or stopped FSEvents stream. Drop stops it and drains the
/// queue so no callback can touch the shared log afterwards.
pub struct EventStream {
    raw: sys::FSEventStreamRef,
    queue: sys::DispatchQueue,
    // The Arc keeps the log alive for inspection after the stream drops;
    // the context's info pointer borrows it.
    shared: Arc<SharedEvents>,
    _ctx: Box<sys::FSEventStreamContext>,
}

/// Distinguishes where stream setup failed; a NULL stream and a failed
/// Start are different findings (device-relative streams are expected to
/// fail in one of the two ways for unprivileged callers).
pub enum StreamStart {
    FailedCreate,
    FailedStart,
    Started(EventStream),
}

impl EventStream {
    /// Create and start a path-scoped stream.
    pub fn start(paths: &[String], since: u64, latency_secs: f64) -> StreamStart {
        Self::start_impl(paths, since, latency_secs, None)
    }

    /// Create and start a stream relative to a device root. `dev` is the
    /// volume's st_dev; `rel_paths` are relative to the volume root.
    pub fn start_relative(
        dev: i32,
        rel_paths: &[String],
        since: u64,
        latency_secs: f64,
    ) -> StreamStart {
        Self::start_impl(rel_paths, since, latency_secs, Some(dev))
    }

    fn start_impl(
        paths: &[String],
        since: u64,
        latency_secs: f64,
        dev: Option<i32>,
    ) -> StreamStart {
        let cf_paths: Vec<CFString> = paths.iter().map(|p| CFString::new(p)).collect();
        let paths_arr = CFArray::from_CFTypes(&cf_paths);
        let shared = Arc::new(SharedEvents {
            inner: Mutex::new(SharedInner::default()),
        });
        let ctx = Box::new(sys::FSEventStreamContext {
            version: 0,
            info: Arc::as_ptr(&shared) as *mut std::ffi::c_void,
            retain: None,
            release: None,
            copy_description: None,
        });
        unsafe {
            let queue = sys::dispatch_queue_create(
                CString::new("ch.lkmc.foldover.spike.s3").unwrap().as_ptr(),
                ptr::null(),
            );
            if queue.is_null() {
                return StreamStart::FailedCreate;
            }
            let raw = match dev {
                None => sys::FSEventStreamCreate(
                    ptr::null(),
                    on_events,
                    &*ctx,
                    paths_arr.as_concrete_TypeRef() as sys::CFArrayRef,
                    since,
                    latency_secs,
                    crate::flags::STREAM_CREATE_FLAGS,
                ),
                Some(dev) => sys::FSEventStreamCreateRelativeToDevice(
                    ptr::null(),
                    on_events,
                    &*ctx,
                    paths_arr.as_concrete_TypeRef() as sys::CFArrayRef,
                    since,
                    latency_secs,
                    crate::flags::STREAM_CREATE_FLAGS,
                    dev,
                ),
            };
            if raw.is_null() {
                sys::dispatch_release(queue);
                return StreamStart::FailedCreate;
            }
            sys::FSEventStreamSetDispatchQueue(raw, queue);
            let mut stream = Self {
                raw,
                queue,
                shared,
                _ctx: ctx,
            };
            if sys::FSEventStreamStart(raw) == 0 {
                stream.stop_now();
                return StreamStart::FailedStart;
            }
            StreamStart::Started(stream)
        }
    }

    /// The event log; valid during and after the stream's life.
    pub fn shared(&self) -> Arc<SharedEvents> {
        self.shared.clone()
    }

    fn stop_now(&mut self) {
        if self.raw.is_null() {
            return;
        }
        unsafe {
            sys::FSEventStreamStop(self.raw);
            sys::FSEventStreamInvalidate(self.raw);
            sys::FSEventStreamRelease(self.raw);
            // Barrier: callbacks already queued on the serial queue run
            // before this returns, so `shared` can be freed afterwards.
            sys::dispatch_sync_f(self.queue, ptr::null_mut(), noop);
            sys::dispatch_release(self.queue);
        }
        self.raw = ptr::null_mut();
        self.queue = ptr::null_mut();
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.stop_now();
    }
}

/// Poll the event log until `pred` holds or `timeout` elapses.
pub fn wait_until(
    shared: &SharedEvents,
    timeout: std::time::Duration,
    pred: impl Fn(&SharedInner) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if shared.inspect(|inner| pred(inner)) {
            return true;
        }
        if Instant::now() >= deadline {
            return shared.inspect(|inner| pred(inner));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Path comparison tolerant of a trailing slash on directory events and of
/// path-form differences: FSEvents may report the firmlink-real path
/// (`/private/...`), a volume-rooted path, or the watched form, so a target
/// also matches on its unique `foldover-s3-<pid>` rooted suffix.
pub fn path_is(event_path: &str, target: &Path) -> bool {
    let ev = event_path.trim_end_matches('/');
    let t = target.to_string_lossy();
    let t = t.trim_end_matches('/');
    if ev == t {
        return true;
    }
    match t.find("foldover-s3-") {
        Some(i) => ev.ends_with(&t[i..]),
        None => false,
    }
}
