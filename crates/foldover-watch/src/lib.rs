//! `foldover-watch` — driver layer. Change and mount events: FSEvents,
//! inotify, polling, and volume notifications (DiskArbitration, udev).
//! Calls the OS directly; depends on no other Foldover crate. See
//! PLAN.md §4.4.
