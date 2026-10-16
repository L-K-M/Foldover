//! DiskArbitration wrapper: a session scheduled on a dedicated thread's
//! CFRunLoop, matching how foldoverd would consume mount notifications in
//! a non-app process.

use crate::sys;
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use std::ffi::CStr;
use std::ptr;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub enum DiskEventKind {
    Appeared,
    Disappeared,
}

pub struct DiskEvent {
    pub kind: DiskEventKind,
    pub bsd_name: Option<String>,
    /// DAVolumePath rendered as a plain path, when the disk is mounted.
    pub volume_path: Option<String>,
    /// DAVolumeUUID (the filesystem UUID, distinct from the FSEvents store
    /// UUID).
    pub volume_uuid: Option<String>,
    pub at: Instant,
}

#[derive(Default)]
pub struct DaInner {
    pub appeared: Vec<DiskEvent>,
    pub disappeared: Vec<DiskEvent>,
}

pub struct DaShared {
    pub inner: Mutex<DaInner>,
    /// The worker thread's run loop, published once the session is live.
    pub run_loop: Mutex<Option<usize>>,
}

impl DaShared {
    pub fn inspect<R>(&self, f: impl FnOnce(&DaInner) -> R) -> R {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&inner)
    }
}

/// Pull the reportable fields off a DADisk: BSD name, mount path and
/// filesystem UUID from DADiskCopyDescription.
///
/// # Safety
/// Called from a DiskArbitration callback; `disk` is valid there.
unsafe fn disk_info(disk: sys::DADiskRef) -> (Option<String>, Option<String>, Option<String>) {
    unsafe {
        let bsd = sys::DADiskGetBSDName(disk);
        let bsd_name = if bsd.is_null() {
            None
        } else {
            Some(CStr::from_ptr(bsd).to_string_lossy().into_owned())
        };
        let desc = sys::DADiskCopyDescription(disk);
        if desc.is_null() {
            return (bsd_name, None, None);
        }
        let uuid_key = CFString::new("DAVolumeUUID");
        let path_key = CFString::new("DAVolumePath");
        let uuid_val = sys::CFDictionaryGetValue(desc, uuid_key.as_concrete_TypeRef().cast());
        let volume_uuid = if uuid_val.is_null() {
            None
        } else {
            let s = sys::CFUUIDCreateString(ptr::null(), uuid_val as sys::CFUUIDRef);
            let out = sys::cf_str_to_string(s);
            if !s.is_null() {
                sys::CFRelease(s as sys::CFTypeRef);
            }
            out
        };
        let path_val = sys::CFDictionaryGetValue(desc, path_key.as_concrete_TypeRef().cast());
        let volume_path = if path_val.is_null() {
            None
        } else {
            // DAVolumePath is a CFURL like file:///Volumes/Name/; keep the
            // path part for comparison with the mount point we chose.
            sys::cf_str_to_string(sys::CFURLGetString(path_val as sys::CFURLRef)).map(|u| {
                let p = u.strip_prefix("file://").unwrap_or(&u);
                p.trim_end_matches('/').to_string()
            })
        };
        sys::CFRelease(desc as sys::CFTypeRef);
        (bsd_name, volume_path, volume_uuid)
    }
}

fn push(shared: &DaShared, kind: DiskEventKind, disk: sys::DADiskRef) {
    let (bsd_name, volume_path, volume_uuid) = unsafe { disk_info(disk) };
    let rec = DiskEvent {
        kind,
        bsd_name,
        volume_path,
        volume_uuid,
        at: Instant::now(),
    };
    let mut inner = shared.inner.lock().unwrap_or_else(|e| e.into_inner());
    match kind {
        DiskEventKind::Appeared => inner.appeared.push(rec),
        DiskEventKind::Disappeared => inner.disappeared.push(rec),
    }
}

unsafe extern "C" fn on_appeared(disk: sys::DADiskRef, ctx: *mut std::ffi::c_void) {
    if ctx.is_null() {
        return;
    }
    push(
        unsafe { &*(ctx as *const DaShared) },
        DiskEventKind::Appeared,
        disk,
    );
}

unsafe extern "C" fn on_disappeared(disk: sys::DADiskRef, ctx: *mut std::ffi::c_void) {
    if ctx.is_null() {
        return;
    }
    push(
        unsafe { &*(ctx as *const DaShared) },
        DiskEventKind::Disappeared,
        disk,
    );
}

/// A live DiskArbitration session. `stop` ends the run loop and unregisters
/// the callbacks before the context is dropped.
pub struct DaSession {
    pub shared: Arc<DaShared>,
    thread: Option<JoinHandle<()>>,
}

impl DaSession {
    /// Create a session and register appeared/disappeared callbacks on a
    /// CFRunLoop running on a spawned thread. Blocks until the session is
    /// scheduled (or 10 s elapse).
    pub fn start() -> Result<Self, String> {
        let shared = Arc::new(DaShared {
            inner: Mutex::new(DaInner::default()),
            run_loop: Mutex::new(None),
        });
        let thread_shared = shared.clone();
        let (ready_tx, ready_rx): (Sender<Result<(), String>>, _) = channel();
        let thread = std::thread::spawn(move || unsafe {
            let session = sys::DASessionCreate(ptr::null());
            if session.is_null() {
                let _ = ready_tx.send(Err("DASessionCreate returned NULL".into()));
                return;
            }
            let rl = sys::CFRunLoopGetCurrent();
            sys::DASessionScheduleWithRunLoop(session, rl, sys::kCFRunLoopDefaultMode);
            // The context is an Arc held for the session's whole life; DA
            // retains nothing for us.
            let ctx = Arc::into_raw(thread_shared.clone()) as *mut std::ffi::c_void;
            sys::DARegisterDiskAppearedCallback(session, ptr::null(), on_appeared, ctx);
            sys::DARegisterDiskDisappearedCallback(session, ptr::null(), on_disappeared, ctx);
            {
                let mut guard = thread_shared
                    .run_loop
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                *guard = Some(rl as usize);
            }
            let _ = ready_tx.send(Ok(()));
            sys::CFRunLoopRun();
            // The run loop exited: unregister in the same order Apple
            // documents before dropping the context.
            let appeared_fp = on_appeared as sys::DADiskCallback as *const std::ffi::c_void;
            let disappeared_fp = on_disappeared as sys::DADiskCallback as *const std::ffi::c_void;
            sys::DAUnregisterCallback(session, appeared_fp, ctx);
            sys::DAUnregisterCallback(session, disappeared_fp, ctx);
            sys::DASessionUnscheduleFromRunLoop(session, rl, sys::kCFRunLoopDefaultMode);
            sys::CFRelease(session as sys::CFTypeRef);
            drop(Arc::from_raw(ctx as *const DaShared));
        });
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(Self {
                shared,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err("DiskArbitration thread did not start in 10 s".into()),
        }
    }

    /// Stop the run loop and join the worker. Equivalent to dropping.
    pub fn stop(self) {}
}

impl Drop for DaSession {
    fn drop(&mut self) {
        let rl = self
            .shared
            .run_loop
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(rl) = rl {
            unsafe {
                sys::CFRunLoopStop(rl as sys::CFRunLoopRef);
                sys::CFRunLoopWakeUp(rl as sys::CFRunLoopRef);
            }
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
