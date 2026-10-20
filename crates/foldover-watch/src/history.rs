//! The file event history macOS keeps per volume (FSEvents), so changes
//! made while nothing watched can be found later (ADR 0002). Event IDs
//! are system-wide and only grow. Each volume's history has an ID of its
//! own that changes when the history is reset; a replay is trusted only
//! while that ID matches the one recorded with the event. Other systems
//! keep no such history: there is no current event, and replays are
//! unsupported.

use std::io;
use std::path::Path;
use std::time::Duration;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod fsevents;

/// The system's latest file event, or `None` where there is no history.
pub fn current_event() -> Option<u64> {
    #[cfg(target_os = "macos")]
    return Some(fsevents::current_event());
    #[cfg(not(target_os = "macos"))]
    None
}

/// The ID of the event history kept for `path`'s volume; `None` when the
/// volume keeps none, as for most network shares.
pub fn history_id(path: &Path) -> io::Result<Option<String>> {
    #[cfg(target_os = "macos")]
    return fsevents::history_id(path);
    #[cfg(not(target_os = "macos"))]
    {
        std::fs::symlink_metadata(path)?;
        Ok(None)
    }
}

/// Whether anything `relevant` accepts changed below `root` after event
/// `since`. `root` must be canonical, as event paths are, e.g.
/// `/private/var/…` rather than `/var/…`. Fails when the history cannot
/// tell: events were dropped, the root moved or was remounted, or no
/// answer came within `timeout`.
pub fn changed_since(
    root: &Path,
    since: u64,
    timeout: Duration,
    relevant: impl Fn(&Path) -> bool,
) -> io::Result<bool> {
    #[cfg(target_os = "macos")]
    return fsevents::changed_since(root, since, timeout, relevant);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (root, since, timeout, relevant);
        Err(io::ErrorKind::Unsupported.into())
    }
}
