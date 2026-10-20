//! What the Mac's state says about syncing now (PLAN.md §3.5): Low Power
//! Mode from Foundation's `NSProcessInfo`, and whether the network path is
//! expensive (cellular, a phone's hotspot) or constrained (Low Data Mode)
//! from Network.framework's path monitor.

use std::ffi::{c_char, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use block2::RcBlock;
use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject};

use crate::NetworkCost;

/// Network.framework's `nw_path_monitor_t` and `nw_path_t`, and
/// libdispatch's `dispatch_queue_t`: opaque object pointers.
type PathMonitor = *mut c_void;
type Path = *mut c_void;
type DispatchQueue = *mut c_void;

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

#[link(name = "Network", kind = "framework")]
unsafe extern "C" {
    fn nw_path_monitor_create() -> PathMonitor;
    fn nw_path_monitor_set_queue(monitor: PathMonitor, queue: DispatchQueue);
    fn nw_path_monitor_set_update_handler(
        monitor: PathMonitor,
        handler: &block2::Block<dyn Fn(Path)>,
    );
    fn nw_path_monitor_start(monitor: PathMonitor);
    fn nw_path_is_expensive(path: Path) -> bool;
    fn nw_path_is_constrained(path: Path) -> bool;
}

// libdispatch, in libSystem.
unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attributes: *mut c_void) -> DispatchQueue;
}

const UNKNOWN: u8 = 0;
const UNRESTRICTED: u8 = 1;
const EXPENSIVE: u8 = 2;
const CONSTRAINED: u8 = 3;

/// The latest path's cost, written by the monitor's queue.
static COST: AtomicU8 = AtomicU8::new(UNKNOWN);
static MONITOR: OnceLock<()> = OnceLock::new();

pub(crate) fn low_power_mode() -> bool {
    let Some(class) = AnyClass::get(c"NSProcessInfo") else {
        return false;
    };
    // SAFETY: +[NSProcessInfo processInfo] takes no arguments and returns
    // the process's shared instance, which is never released;
    // -isLowPowerModeEnabled takes no arguments and returns a BOOL.
    unsafe {
        let info: *mut AnyObject = msg_send![class, processInfo];
        if info.is_null() {
            return false;
        }
        msg_send![info, isLowPowerModeEnabled]
    }
}

/// The current path's cost; unknown until the monitor's first report,
/// which arrives shortly after the first call.
pub(crate) fn network_cost() -> Option<NetworkCost> {
    MONITOR.get_or_init(start_monitor);
    match COST.load(Ordering::Relaxed) {
        UNRESTRICTED => Some(NetworkCost::Unrestricted),
        EXPENSIVE => Some(NetworkCost::Expensive),
        CONSTRAINED => Some(NetworkCost::Constrained),
        _ => None,
    }
}

fn start_monitor() {
    let handler = RcBlock::new(|path: Path| {
        // SAFETY: the monitor passes a valid path that lives for the
        // handler's call; both functions only read it.
        let cost = unsafe {
            if nw_path_is_constrained(path) {
                CONSTRAINED
            } else if nw_path_is_expensive(path) {
                EXPENSIVE
            } else {
                UNRESTRICTED
            }
        };
        COST.store(cost, Ordering::Relaxed);
    });
    // SAFETY: the label is a static C string; the queue and monitor are
    // created here and kept for the process's life, never released; the
    // monitor copies the handler block before this returns.
    unsafe {
        let queue =
            dispatch_queue_create(c"ch.lkmc.foldover.network".as_ptr(), std::ptr::null_mut());
        let monitor = nw_path_monitor_create();
        nw_path_monitor_set_queue(monitor, queue);
        nw_path_monitor_set_update_handler(monitor, &handler);
        nw_path_monitor_start(monitor);
    }
    // The monitor copies the handler, but it lives as long as the process
    // anyway, so keep this reference too rather than rely on the copy.
    std::mem::forget(handler);
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn the_power_mode_reads_without_failing() {
        // CI Macs run on mains power; either answer is fine, a crash is not.
        let _ = low_power_mode();
    }

    #[test]
    fn the_path_monitor_reports_a_cost_soon() {
        let deadline = Instant::now() + Duration::from_secs(10);
        while network_cost().is_none() {
            assert!(Instant::now() < deadline, "no network path reported");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
