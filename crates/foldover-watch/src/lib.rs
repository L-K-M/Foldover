//! `foldover-watch` — driver layer. Change and mount events: FSEvents,
//! inotify, polling, and volume notifications (DiskArbitration, udev).
//! Calls the OS directly; depends on no other Foldover crate. See
//! PLAN.md §4.4.
//!
//! Events are hints, never truth (PLAN.md §4.8): a change only prompts a
//! full observation, so a dropped or duplicated event costs time, not data.

use std::io;
use std::path::{Path, PathBuf};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher as _};

/// What changed below the watched folders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Change {
    Paths(Vec<PathBuf>),
    /// Events were lost or the backend failed; anything may have changed.
    Unknown,
}

/// Recursive change notifications for folders.
pub struct Watcher {
    inner: RecommendedWatcher,
}

impl Watcher {
    /// `changed` runs on the backend's notification thread.
    pub fn new(changed: impl Fn(Change) + Send + 'static) -> io::Result<Self> {
        let inner = notify::recommended_watcher(move |event: notify::Result<Event>| {
            changed(match event {
                Ok(event) if event.need_rescan() || event.paths.is_empty() => Change::Unknown,
                Ok(event) => Change::Paths(event.paths),
                Err(_) => Change::Unknown,
            });
        })
        .map_err(io_error)?;
        Ok(Self { inner })
    }

    pub fn watch(&mut self, folder: &Path) -> io::Result<()> {
        self.inner
            .watch(folder, RecursiveMode::Recursive)
            .map_err(io_error)
    }

    pub fn unwatch(&mut self, folder: &Path) -> io::Result<()> {
        self.inner.unwatch(folder).map_err(io_error)
    }
}

fn io_error(error: notify::Error) -> io::Error {
    match error.kind {
        notify::ErrorKind::Io(error) => error,
        kind => io::Error::other(format!("{kind:?}")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[test]
    fn writes_below_a_watched_folder_are_reported() {
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().canonicalize().unwrap();
        std::fs::create_dir(folder.join("nested")).unwrap();
        let (sender, receiver) = mpsc::channel();
        let mut watcher = Watcher::new(move |change| {
            let _ = sender.send(change);
        })
        .unwrap();
        watcher.watch(&folder).unwrap();
        std::fs::write(folder.join("nested/file"), b"x").unwrap();

        let target = folder.join("nested/file");
        let seen = std::iter::from_fn(|| receiver.recv_timeout(Duration::from_secs(10)).ok()).any(
            |change| match change {
                Change::Paths(paths) => paths.contains(&target),
                Change::Unknown => true,
            },
        );
        assert!(seen);
        watcher.unwatch(&folder).unwrap();
    }
}
