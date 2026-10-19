//! Which pair roots changed, from change notifications. Foldover's own
//! archive, identity and temporary files never count, so a sync does not
//! trigger itself through them.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use foldover_types::LocalRoot;
use foldover_watch::{Change, Watcher};

use crate::local::is_internal_name;

/// Watches roots and reports the keys of those that changed.
pub struct RootWatcher<K> {
    watcher: Watcher,
    roots: Arc<Mutex<BTreeMap<PathBuf, Vec<K>>>>,
}

impl<K: Clone + Eq + Send + 'static> RootWatcher<K> {
    /// `changed` runs on the notification thread, once per affected key.
    pub fn new(changed: impl Fn(K) + Send + 'static) -> io::Result<Self> {
        let roots: Arc<Mutex<BTreeMap<PathBuf, Vec<K>>>> = Arc::default();
        let watched = Arc::clone(&roots);
        let watcher = Watcher::new(move |change| {
            let roots = watched.lock().unwrap_or_else(PoisonError::into_inner);
            let mut affected: Vec<K> = Vec::new();
            for (root, keys) in roots.iter() {
                let hit = match &change {
                    Change::Unknown => true,
                    Change::Paths(paths) => paths.iter().any(|path| relevant(root, path)),
                };
                if hit {
                    for key in keys {
                        if !affected.contains(key) {
                            affected.push(key.clone());
                        }
                    }
                }
            }
            drop(roots);
            for key in affected {
                changed(key);
            }
        })?;
        Ok(Self { watcher, roots })
    }

    /// Watch exactly these roots. A root that cannot be watched, such as an
    /// unmounted disk, is skipped and returned; schedules still cover it.
    pub fn set_roots(&mut self, wanted: &[(LocalRoot, K)]) -> Vec<(LocalRoot, io::Error)> {
        let mut failures = Vec::new();
        let mut next: BTreeMap<PathBuf, Vec<K>> = BTreeMap::new();
        for (root, key) in wanted {
            let path = Path::new(OsStr::from_bytes(root.as_bytes()));
            // Notifications report resolved paths, e.g. /private/var for /var.
            match path.canonicalize() {
                Ok(path) => next.entry(path).or_default().push(key.clone()),
                Err(error) => failures.push((root.clone(), error)),
            }
        }
        let mut roots = self.roots.lock().unwrap_or_else(PoisonError::into_inner);
        for path in roots.keys() {
            if !next.contains_key(path) {
                let _ = self.watcher.unwatch(path);
            }
        }
        let mut watched = BTreeMap::new();
        for (path, keys) in next {
            if roots.contains_key(&path) || self.watcher.watch(&path).is_ok() {
                watched.insert(path, keys);
            } else if let Some((root, _)) = wanted.iter().find(|(_, key)| keys.contains(key)) {
                failures.push((root.clone(), io::Error::other("Cannot watch this folder")));
            }
        }
        *roots = watched;
        failures
    }
}

/// A change inside `root` that is not Foldover's own bookkeeping. Scanning
/// excludes internal names at every depth, so watching does too.
fn relevant(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    // The root itself, or a path whose components all survive filtering.
    relative.components().all(|component| match component {
        Component::Normal(name) => !is_internal_name(name.as_bytes()),
        _ => true,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[test]
    fn internal_paths_never_count_as_changes() {
        let root = Path::new("/r");
        assert!(relevant(root, Path::new("/r")));
        assert!(relevant(root, Path::new("/r/a/b.txt")));
        assert!(!relevant(root, Path::new("/r/.foldover/archive/x")));
        assert!(!relevant(root, Path::new("/r/a/.foldover-tmp-12")));
        assert!(!relevant(root, Path::new("/r/a/.DS_Store")));
        // Scanning excludes internal names at every depth, not just the ends.
        assert!(!relevant(root, Path::new("/r/a/.foldover-tmp-12/b")));
        assert!(!relevant(root, Path::new("/r/deep/.foldover/x")));
        assert!(!relevant(root, Path::new("/other/a")));
        assert!(!relevant(root, Path::new("/rx/a")));
    }

    #[test]
    fn changes_are_reported_per_root_key() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let root =
            |path: &Path| LocalRoot::from_bytes(path.as_os_str().as_bytes().to_vec()).unwrap();
        let (sender, receiver) = mpsc::channel();
        let mut watcher = RootWatcher::new(move |key: u8| {
            let _ = sender.send(key);
        })
        .unwrap();
        let missing = first.path().join("missing");
        let failures = watcher.set_roots(&[
            (root(first.path()), 1),
            (root(second.path()), 2),
            (root(&missing), 3),
        ]);
        assert_eq!(failures.len(), 1);

        std::fs::create_dir(first.path().join(".foldover")).unwrap();
        std::fs::write(first.path().join(".foldover/id"), b"x").unwrap();
        std::fs::write(second.path().join("file"), b"x").unwrap();
        let keys: Vec<u8> =
            std::iter::from_fn(|| receiver.recv_timeout(Duration::from_secs(5)).ok()).collect();
        assert!(keys.contains(&2), "{keys:?}");

        // Unwatched roots stop reporting.
        watcher.set_roots(&[]);
        while receiver.try_recv().is_ok() {}
        std::fs::write(second.path().join("later"), b"x").unwrap();
        assert!(receiver.recv_timeout(Duration::from_secs(2)).is_err());
    }
}
