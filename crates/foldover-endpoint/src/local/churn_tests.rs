use super::*;
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};

const ALL: MetadataPolicy = MetadataPolicy::new(
    foldover_types::Propagation::Sync,
    foldover_types::Propagation::Sync,
    foldover_types::Propagation::Sync,
);

#[test]
fn disappearing_directory_entries_do_not_discard_later_readable_entries() {
    for scope in [
        None,
        Some(SyncPath::try_from(b"nested".as_slice()).unwrap()),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("stable"), b"keep").unwrap();
        let root = Root::open(temporary.path()).unwrap();
        let entry = root
            .entries(DirectoryScope::Root)
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        let mut directory = Directory::open(root, scope.clone()).unwrap();
        directory.entries = Box::new(
            [
                Err(io::ErrorKind::NotFound.into()),
                Err(io::ErrorKind::NotADirectory.into()),
                Ok(entry),
            ]
            .into_iter(),
        );
        let mut stats = ScanStats::default();
        assert_eq!(
            directory.next_entry(&mut stats).unwrap().unwrap().name(),
            b"stable"
        );
        assert!(directory.next_entry(&mut stats).unwrap().is_none());
        assert!(stats.unstable.iter().all(|path| path == &scope));
        assert_eq!(stats.unstable.len(), 2);
        assert!(stats.unavailable().is_empty());
    }
}

#[test]
fn root_child_added_after_listing_preserves_healthy_file() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::write(root.join("stable"), b"keep me").unwrap();
    let endpoint = LocalEndpoint::open(root).unwrap();
    let mut mutated = false;
    let observation = endpoint
        .observe_reporting(
            ALL,
            &Filter::none(),
            &BTreeMap::new(),
            &mut || ScanControl::Continue,
            &mut |step| {
                if step == ObserveStep::Listed && !mutated {
                    mutated = true;
                    fs::write(root.join("new_child"), b"late").unwrap();
                }
            },
        )
        .unwrap();
    assert!(mutated);
    assert!(
        observation
            .entries()
            .contains_key(&SyncPath::try_from(b"stable".as_slice()).unwrap())
    );
    assert!(observation.unstable_directories().contains(&None));
    assert!(observation.unavailable().is_empty());
}

#[test]
fn nested_directory_child_added_after_listing_preserves_existing_entries() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub/existing"), b"keep").unwrap();
    fs::write(root.join("sibling"), b"independent").unwrap();
    let endpoint = LocalEndpoint::open(root).unwrap();
    let mut listed = 0;
    let observation = endpoint
        .observe_reporting(
            ALL,
            &Filter::none(),
            &BTreeMap::new(),
            &mut || ScanControl::Continue,
            &mut |step| {
                if step == ObserveStep::Listed {
                    listed += 1;
                    // Either the directory or its child is second in the
                    // depth-first walk, independent of readdir ordering.
                    if listed == 2 {
                        fs::write(root.join("sub/new_child"), b"late").unwrap();
                    }
                }
            },
        )
        .unwrap();
    for name in [b"sub/existing".as_slice(), b"sibling"] {
        assert!(
            observation
                .entries()
                .contains_key(&SyncPath::try_from(name).unwrap())
        );
    }
    assert!(
        observation
            .unstable_directories()
            .contains(&Some(SyncPath::try_from(b"sub".as_slice()).unwrap()))
    );
    assert!(observation.unavailable().is_empty());
}

/// Copies another Mac sends at once each stage a file in the shared root
/// and rename it into place. A name the access probe listed could be gone
/// before it read its metadata, and the probe called the whole folder
/// unreadable, failing the copy it guarded. On one core the race is
/// rarely hit; the next test checks the rule without timing.
#[test]
fn a_name_gone_between_listing_and_reading_still_proves_access() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let endpoint = LocalEndpoint::open(root).unwrap();
    let done = AtomicBool::new(false);
    std::thread::scope(|threads| {
        threads.spawn(|| {
            let (staged, placed) = (root.join(".foldover-tmp-copy"), root.join("copy"));
            while !done.load(Ordering::Relaxed) {
                fs::write(&staged, b"").unwrap();
                fs::rename(&staged, &placed).unwrap();
                fs::remove_file(&placed).unwrap();
            }
        });
        let probed = (0..1_000).try_for_each(|_| endpoint.probe_access());
        done.store(true, Ordering::Relaxed);
        probed.unwrap();
    });
}

#[test]
fn a_first_name_gone_before_its_metadata_still_proves_access() {
    for kind in [io::ErrorKind::NotFound, io::ErrorKind::NotADirectory] {
        assert!(listing_allowed(Some(Err(kind.into()))).is_ok(), "{kind:?}");
    }
    assert!(listing_allowed(None).is_ok());
    let refused = listing_allowed(Some(Err(io::ErrorKind::PermissionDenied.into())));
    assert!(refused.is_err_and(|error| error.is_access_denied()));
}
