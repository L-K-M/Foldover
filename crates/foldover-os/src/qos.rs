//! Thread quality-of-service classes from pthread/qos.h. A thread at the
//! utility class yields to the person's own work, and Apple silicon
//! prefers efficiency cores for it. The class belongs to one thread:
//! threads it starts begin at the default class, never inheriting it.

use std::ffi::c_int;
use std::marker::PhantomData;

/// `qos_class_t` values from sys/qos.h, ordered from lowest to highest.
const UNSPECIFIED: u32 = 0x00;
const UTILITY: u32 = 0x11;
const DEFAULT: u32 = 0x15;

unsafe extern "C" {
    fn pthread_set_qos_class_self_np(class: u32, relative_priority: c_int) -> c_int;
    fn pthread_get_qos_class_np(
        thread: libc::pthread_t,
        class: *mut u32,
        relative_priority: *mut c_int,
    ) -> c_int;
}

/// The calling thread was lowered to utility; dropping restores its
/// previous class on the same thread.
pub(crate) struct Lowered {
    class: u32,
    relative_priority: c_int,
    /// The class belongs to the thread that set it.
    _thread: PhantomData<*const ()>,
}

/// The calling thread's class and relative priority.
fn current() -> Option<(u32, c_int)> {
    let mut class = UNSPECIFIED;
    let mut relative_priority = 0;
    // SAFETY: `pthread_self` names the live calling thread, and both
    // out-pointers are valid places for the duration of the call.
    let status = unsafe {
        pthread_get_qos_class_np(libc::pthread_self(), &mut class, &mut relative_priority)
    };
    (status == 0).then_some((class, relative_priority))
}

/// Move the calling thread down to utility. `None` when it already runs
/// at utility or below, so nothing is ever raised, or when the system
/// refused.
pub(crate) fn lower_to_utility() -> Option<Lowered> {
    let (class, relative_priority) = current()?;
    // An unspecified thread is scheduled like a default one, and the
    // system refuses to set unspecified again, so default restores it.
    let class = if class == UNSPECIFIED { DEFAULT } else { class };
    if class <= UTILITY {
        return None;
    }
    // SAFETY: value arguments only; the call changes the calling thread.
    if unsafe { pthread_set_qos_class_self_np(UTILITY, 0) } != 0 {
        return None;
    }
    Some(Lowered {
        class,
        relative_priority,
        _thread: PhantomData,
    })
}

impl Drop for Lowered {
    fn drop(&mut self) {
        // SAFETY: value arguments only, read from this same thread by
        // `lower_to_utility`. A refused restore leaves the thread at
        // utility, which only slows its later work.
        let _ = unsafe { pthread_set_qos_class_self_np(self.class, self.relative_priority) };
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    fn class() -> u32 {
        current().expect("the calling thread has a class").0
    }

    #[test]
    fn lowering_lasts_until_dropped_and_restores_the_previous_class() {
        thread::spawn(|| {
            let before = class();
            assert!(before > UTILITY, "a new thread starts above utility");
            let lowered = lower_to_utility().expect("a default thread lowers");
            assert_eq!(class(), UTILITY);
            drop(lowered);
            assert_eq!(class(), before);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn a_thread_at_utility_or_below_is_left_alone() {
        thread::spawn(|| {
            let _outer = lower_to_utility().unwrap();
            assert!(lower_to_utility().is_none());
            assert_eq!(class(), UTILITY);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn threads_do_not_inherit_their_creators_class() {
        // Why every thread a gentle run starts lowers itself.
        thread::spawn(|| {
            let _lowered = lower_to_utility().unwrap();
            let child = thread::spawn(class).join().unwrap();
            assert!(child > UTILITY, "child started at {child:#x}");
        })
        .join()
        .unwrap();
    }
}
