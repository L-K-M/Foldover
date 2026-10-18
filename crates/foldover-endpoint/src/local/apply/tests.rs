use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use foldover_types::{Durability, MetadataPolicy, Propagation};

use super::*;
use crate::local::Observation;

const OPTIONS: PairOptions = PairOptions::new(
    MetadataPolicy::new(Propagation::Sync, Propagation::Sync, Propagation::Sync),
    Durability::Standard,
);

struct Pair {
    left_directory: tempfile::TempDir,
    right_directory: tempfile::TempDir,
    left: LocalEndpoint,
    right: LocalEndpoint,
    run: RunScope,
}

fn pair() -> Pair {
    let left_directory = tempfile::tempdir().unwrap();
    let right_directory = tempfile::tempdir().unwrap();
    Pair {
        left: LocalEndpoint::open(left_directory.path()).unwrap(),
        right: LocalEndpoint::open(right_directory.path()).unwrap(),
        left_directory,
        right_directory,
        run: RunScope::new(std::time::UNIX_EPOCH, OPTIONS),
    }
}

fn path(bytes: &[u8]) -> SyncPath {
    SyncPath::try_from(bytes).unwrap()
}

fn proceed() -> ScanControl {
    ScanControl::Continue
}

fn observe(endpoint: &LocalEndpoint) -> Observation {
    endpoint.observe(OPTIONS.metadata(), &mut proceed).unwrap()
}

fn names(directory: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn creates_parents_then_files_and_returns_the_stored_version() {
    let pair = pair();
    fs::create_dir(pair.left_directory.path().join("a")).unwrap();
    fs::write(pair.left_directory.path().join("a/file"), b"data").unwrap();
    symlink("file", pair.left_directory.path().join("a/link")).unwrap();
    let left = observe(&pair.left);

    for name in [b"a".as_slice(), b"a/file", b"a/link"] {
        let name = path(name);
        let stored = pair
            .right
            .copy_from(
                &pair.left,
                &name,
                &left.entries()[&name],
                Expected::Absent,
                &pair.run,
                &mut proceed,
            )
            .unwrap();
        assert_eq!(stored.content(), left.entries()[&name].version().content());
    }
    let right = observe(&pair.right);
    assert_eq!(
        right.versions().keys().collect::<Vec<_>>(),
        left.versions().keys().collect::<Vec<_>>()
    );
    for (name, entry) in right.entries() {
        assert_eq!(
            entry.version().content(),
            left.entries()[name].version().content()
        );
    }
    assert_eq!(
        fs::read(pair.right_directory.path().join("a/file")).unwrap(),
        b"data"
    );
}

#[test]
fn replacements_archive_the_previous_version() {
    let pair = pair();
    fs::write(pair.left_directory.path().join("file"), b"new").unwrap();
    fs::write(pair.right_directory.path().join("file"), b"old").unwrap();
    let (left, right) = (observe(&pair.left), observe(&pair.right));
    let file = path(b"file");

    pair.right
        .copy_from(
            &pair.left,
            &file,
            &left.entries()[&file],
            Expected::Present(&right.entries()[&file]),
            &pair.run,
            &mut proceed,
        )
        .unwrap();
    let target = pair.right_directory.path();
    assert_eq!(fs::read(target.join("file")).unwrap(), b"new");
    assert_eq!(
        fs::read(target.join(".foldover/archive/1970-01-01T00-00-00.000Z/file")).unwrap(),
        b"old"
    );
    assert_eq!(names(target), [".foldover", "file"]);
}

#[test]
fn changes_after_the_preview_stop_the_operation_without_writing() {
    let pair = pair();
    let (source, target) = (pair.left_directory.path(), pair.right_directory.path());
    fs::write(source.join("file"), b"new").unwrap();
    fs::write(target.join("file"), b"old").unwrap();
    let (left, right) = (observe(&pair.left), observe(&pair.right));
    let file = path(b"file");
    let copy = || {
        pair.right.copy_from(
            &pair.left,
            &file,
            &left.entries()[&file],
            Expected::Present(&right.entries()[&file]),
            &pair.run,
            &mut proceed,
        )
    };

    fs::write(target.join("file"), b"edited on the target").unwrap();
    assert!(matches!(copy(), Err(ApplyError::DestinationChanged(_))));
    assert_eq!(
        fs::read(target.join("file")).unwrap(),
        b"edited on the target"
    );

    fs::write(target.join("file"), b"old").unwrap();
    fs::write(source.join("file"), b"edited on the source").unwrap();
    // The target's rewrite moved its ctime, but its content still matches.
    assert!(matches!(copy(), Err(ApplyError::SourceChanged(_))));
    assert_eq!(fs::read(target.join("file")).unwrap(), b"old");

    // A create never replaces something that appeared since the preview.
    fs::write(source.join("new"), b"data").unwrap();
    let left = observe(&pair.left);
    fs::write(target.join("new"), b"appeared").unwrap();
    let new = path(b"new");
    assert!(matches!(
        pair.right.copy_from(
            &pair.left,
            &new,
            &left.entries()[&new],
            Expected::Absent,
            &pair.run,
            &mut proceed,
        ),
        Err(ApplyError::DestinationChanged(_))
    ));
    assert_eq!(fs::read(target.join("new")).unwrap(), b"appeared");
    assert!(!target.join(".foldover").exists());
}

#[test]
fn metadata_updates_leave_content_and_return_the_stored_version() {
    let pair = pair();
    let (source, target) = (pair.left_directory.path(), pair.right_directory.path());
    fs::write(source.join("file"), b"same").unwrap();
    fs::write(target.join("file"), b"same").unwrap();
    fs::set_permissions(source.join("file"), fs::Permissions::from_mode(0o600)).unwrap();
    let (left, right) = (observe(&pair.left), observe(&pair.right));
    let file = path(b"file");

    let stored = pair
        .right
        .update_metadata_from(
            &pair.left,
            &file,
            &left.entries()[&file],
            &right.entries()[&file],
            &pair.run,
            &mut proceed,
        )
        .unwrap();
    assert_eq!(
        fs::metadata(target.join("file"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(stored, observe(&pair.right).entries()[&file].version());
    assert_eq!(
        stored.metadata(),
        left.entries()[&file].version().metadata()
    );
}

#[test]
fn removals_archive_files_and_remove_only_empty_folders() {
    let pair = pair();
    let target = pair.right_directory.path();
    fs::create_dir(target.join("folder")).unwrap();
    fs::write(target.join("folder/file"), b"data").unwrap();
    let right = observe(&pair.right);
    let (folder, file) = (path(b"folder"), path(b"folder/file"));

    fs::write(target.join("folder/late"), b"").unwrap();
    pair.right
        .remove(&file, &right.entries()[&file], &pair.run, &mut proceed)
        .unwrap();
    assert!(matches!(
        pair.right
            .remove(&folder, &right.entries()[&folder], &pair.run, &mut proceed),
        Err(ApplyError::NotEmpty(_))
    ));
    assert_eq!(
        fs::read(target.join(".foldover/archive/1970-01-01T00-00-00.000Z/folder/file")).unwrap(),
        b"data"
    );

    fs::remove_file(target.join("folder/late")).unwrap();
    let right = observe(&pair.right);
    pair.right
        .remove(&folder, &right.entries()[&folder], &pair.run, &mut proceed)
        .unwrap();
    assert_eq!(names(target), [".foldover"]);
}

#[test]
fn leftover_temporaries_are_reported_and_archived_not_deleted() {
    let pair = pair();
    let target = pair.right_directory.path();
    fs::create_dir(target.join("folder")).unwrap();
    fs::write(target.join("folder/.foldover-tmp-0123"), b"displaced").unwrap();
    let right = observe(&pair.right);
    let leftover = path(b"folder/.foldover-tmp-0123");
    assert_eq!(right.leftovers(), std::slice::from_ref(&leftover));
    assert!(!right.entries().contains_key(&leftover));

    pair.right.recover_leftover(&leftover, &pair.run).unwrap();
    assert_eq!(
        fs::read(
            target.join(".foldover/archive/1970-01-01T00-00-00.000Z/folder/.foldover-tmp-0123")
        )
        .unwrap(),
        b"displaced"
    );
    assert!(observe(&pair.right).leftovers().is_empty());
}

#[test]
fn settled_fingerprints_still_catch_rewrites_that_restore_size_and_mtime() {
    let pair = pair();
    let target = pair.right_directory.path().join("file");
    fs::write(pair.left_directory.path().join("file"), b"new").unwrap();
    fs::write(&target, b"old").unwrap();
    let past = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    let handle = fs::File::options().write(true).open(&target).unwrap();
    handle.set_modified(past).unwrap();
    let (left, right) = (observe(&pair.left), observe(&pair.right));

    fs::write(&target, b"odd").unwrap();
    handle.set_modified(past).unwrap();
    let file = path(b"file");
    assert!(matches!(
        pair.right.copy_from(
            &pair.left,
            &file,
            &left.entries()[&file],
            Expected::Present(&right.entries()[&file]),
            &pair.run,
            &mut proceed,
        ),
        Err(ApplyError::DestinationChanged(_))
    ));
    assert_eq!(fs::read(&target).unwrap(), b"odd");
}
