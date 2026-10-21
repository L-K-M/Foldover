use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use super::*;
use foldover_types::MetadataPolicy;

const ALL: PairOptions = PairOptions::new(
    MetadataPolicy::new(Propagation::Sync, Propagation::Sync, Propagation::Sync),
    Durability::Standard,
);
const NOTHING: PairOptions = PairOptions::new(
    MetadataPolicy::new(
        Propagation::Ignore,
        Propagation::Ignore,
        Propagation::Ignore,
    ),
    Durability::Full,
);
const STANDARD: Durability = Durability::Standard;

#[cfg(target_os = "linux")]
const ATTRIBUTE: &str = "user.ch.lkmc.foldover.test";
#[cfg(not(target_os = "linux"))]
const ATTRIBUTE: &str = "ch.lkmc.foldover.test";

fn path(bytes: &[u8]) -> SyncPath {
    SyncPath::try_from(bytes).unwrap()
}

fn proceed(_: u64) -> ScanControl {
    ScanControl::Continue
}

struct Roots {
    _source_directory: tempfile::TempDir,
    target_directory: tempfile::TempDir,
    source: Root,
    target: Root,
}

fn roots() -> Roots {
    let source_directory = tempfile::tempdir().unwrap();
    let target_directory = tempfile::tempdir().unwrap();
    Roots {
        source: Root::open(source_directory.path()).unwrap(),
        target: Root::open(target_directory.path()).unwrap(),
        _source_directory: source_directory,
        target_directory,
    }
}

fn names(directory: &Path) -> Vec<Vec<u8>> {
    let mut names: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().as_bytes().to_vec())
        .collect();
    names.sort();
    names
}

#[test]
fn staged_copies_carry_data_mode_mtime_and_xattrs_and_place_without_replacing() {
    let roots = roots();
    let source_file = roots._source_directory.path().join("file");
    fs::write(&source_file, b"data").unwrap();
    fs::set_permissions(&source_file, fs::Permissions::from_mode(0o640)).unwrap();
    let handle = fs::File::open(&source_file).unwrap();
    rustix::fs::fsetxattr(&handle, ATTRIBUTE, b"tag", rustix::fs::XattrFlags::empty()).unwrap();
    let file = path(b"file");
    let observed = roots.source.metadata(&file).unwrap();

    let staged = roots
        .target
        .stage_file(&file, &roots.source, &observed, &ALL, &mut proceed)
        .unwrap();
    assert_eq!(
        staged.data_digest(),
        roots
            .source
            .hash_file(&file, &observed, &mut proceed)
            .unwrap()
    );
    let copied = staged.metadata().unwrap();
    assert_eq!(copied.permissions() & 0o7777, 0o640);
    assert_eq!(copied.modified(), observed.modified());
    let attributes = staged.extended_metadata(&mut proceed).unwrap();
    assert!(
        attributes
            .attributes()
            .iter()
            .any(|attribute| attribute.name() == ATTRIBUTE.as_bytes())
    );
    // Unplaced, the copy is invisible under a temporary name.
    let temporary = names(roots.target_directory.path());
    assert_eq!(temporary.len(), 1);
    assert!(temporary[0].starts_with(TEMPORARY_PREFIX.as_bytes()));

    assert!(
        roots
            .target
            .place(staged.into_staged(), &file, Placement::Create, STANDARD)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fs::read(roots.target_directory.path().join("file")).unwrap(),
        b"data"
    );
    assert_eq!(names(roots.target_directory.path()), [b"file".to_vec()]);

    // A second create never replaces the placed file, and cleans up its copy.
    let again = roots
        .target
        .stage_file(&file, &roots.source, &observed, &ALL, &mut proceed)
        .unwrap();
    assert!(matches!(
        roots
            .target
            .place(again.into_staged(), &file, Placement::Create, STANDARD),
        Err(WriteError::Exists)
    ));
    assert_eq!(names(roots.target_directory.path()), [b"file".to_vec()]);
}

#[test]
fn replacement_displaces_the_old_entry_for_archiving() {
    let roots = roots();
    let target = roots.target_directory.path();
    fs::write(roots._source_directory.path().join("file"), b"new").unwrap();
    fs::write(target.join("file"), b"old").unwrap();
    let file = path(b"file");
    let observed = roots.source.metadata(&file).unwrap();
    let old = roots.target.metadata(&file).unwrap();

    let staged = roots
        .target
        .stage_file(&file, &roots.source, &observed, &ALL, &mut proceed)
        .unwrap();
    let displaced = roots
        .target
        .place(staged.into_staged(), &file, Placement::Replace, STANDARD)
        .unwrap()
        .unwrap();
    assert_eq!(fs::read(target.join("file")).unwrap(), b"new");
    assert_eq!(displaced.metadata().unwrap().identity(), old.identity());

    let archived = path(b".foldover/archive/run/file");
    roots
        .target
        .archive_displaced(displaced, &archived, STANDARD)
        .unwrap();
    assert_eq!(
        fs::read(target.join(".foldover/archive/run/file")).unwrap(),
        b"old"
    );
    assert_eq!(
        fs::metadata(target.join(".foldover"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(names(target), [b".foldover".to_vec(), b"file".to_vec()]);
}

#[test]
fn folders_without_an_archive_delete_what_is_replaced_or_removed() {
    let roots = roots();
    let target = roots.target_directory.path();
    fs::write(roots._source_directory.path().join("file"), b"new").unwrap();
    fs::write(target.join("file"), b"old").unwrap();
    fs::write(target.join("other"), b"other").unwrap();
    let (file, other) = (path(b"file"), path(b"other"));
    let observed = roots.source.metadata(&file).unwrap();

    let staged = roots
        .target
        .stage_file(&file, &roots.source, &observed, &ALL, &mut proceed)
        .unwrap();
    let displaced = roots
        .target
        .place(staged.into_staged(), &file, Placement::Replace, STANDARD)
        .unwrap()
        .unwrap();
    roots.target.discard_displaced(displaced, STANDARD).unwrap();
    assert_eq!(fs::read(target.join("file")).unwrap(), b"new");

    // Only the observed entry is deleted, and never a folder.
    let seen = roots.target.metadata(&other).unwrap();
    fs::write(target.join("other"), b"edited").unwrap();
    assert!(matches!(
        roots.target.delete(&other, &seen, STANDARD),
        Err(WriteError::Changed)
    ));
    let seen = roots.target.metadata(&other).unwrap();
    roots.target.delete(&other, &seen, STANDARD).unwrap();
    fs::create_dir(target.join("folder")).unwrap();
    let folder = roots.target.metadata(&path(b"folder")).unwrap();
    assert!(matches!(
        roots.target.delete(&path(b"folder"), &folder, STANDARD),
        Err(WriteError::UnsupportedEntry)
    ));
    assert_eq!(names(target), [b"file".to_vec(), b"folder".to_vec()]);
}

#[test]
fn moves_take_only_the_observed_entry_and_never_replace() {
    let roots = roots();
    let target = roots.target_directory.path();
    fs::create_dir(target.join("new")).unwrap();
    fs::write(target.join("old.txt"), b"moved").unwrap();
    fs::write(target.join("taken.txt"), b"keep").unwrap();
    let (old, moved, taken) = (path(b"old.txt"), path(b"new/moved.txt"), path(b"taken.txt"));
    let observed = roots.target.metadata(&old).unwrap();

    assert!(matches!(
        roots.target.move_entry(&old, &observed, &taken, STANDARD),
        Err(WriteError::Exists)
    ));
    fs::write(target.join("old.txt"), b"edited").unwrap();
    assert!(matches!(
        roots.target.move_entry(&old, &observed, &moved, STANDARD),
        Err(WriteError::Changed)
    ));
    let observed = roots.target.metadata(&old).unwrap();
    roots
        .target
        .move_entry(&old, &observed, &moved, STANDARD)
        .unwrap();
    assert_eq!(fs::read(target.join("new/moved.txt")).unwrap(), b"edited");
    assert_eq!(fs::read(target.join("taken.txt")).unwrap(), b"keep");
    assert!(!target.join("old.txt").exists());
}

#[test]
fn moves_make_missing_folders_and_never_take_or_replace_a_folder() {
    let roots = roots();
    let target = roots.target_directory.path();
    fs::write(target.join("file.txt"), b"moved").unwrap();
    fs::create_dir(target.join("dir")).unwrap();
    let (file, folder, deep) = (
        path(b"file.txt"),
        path(b"dir"),
        path(b"missing/parent/moved.txt"),
    );
    let observed = roots.target.metadata(&file).unwrap();
    let folder_metadata = roots.target.metadata(&folder).unwrap();

    assert!(matches!(
        roots.target.move_entry(&file, &observed, &folder, STANDARD),
        Err(WriteError::Exists)
    ));
    assert!(matches!(
        roots
            .target
            .move_entry(&folder, &folder_metadata, &deep, STANDARD),
        Err(WriteError::UnsupportedEntry)
    ));
    roots
        .target
        .move_entry(&file, &observed, &deep, STANDARD)
        .unwrap();
    assert_eq!(
        fs::read(target.join("missing/parent/moved.txt")).unwrap(),
        b"moved"
    );
    assert!(!target.join("file.txt").exists());
}

#[test]
fn replacing_a_missing_entry_fails_and_keeps_nothing() {
    let roots = roots();
    fs::write(roots._source_directory.path().join("file"), b"new").unwrap();
    let file = path(b"file");
    let observed = roots.source.metadata(&file).unwrap();
    let staged = roots
        .target
        .stage_file(&file, &roots.source, &observed, &ALL, &mut proceed)
        .unwrap();
    assert!(matches!(
        roots
            .target
            .place(staged.into_staged(), &file, Placement::Replace, STANDARD),
        Err(WriteError::Changed)
    ));
    assert!(names(roots.target_directory.path()).is_empty());
}

#[test]
fn archives_move_only_unchanged_entries_and_never_overwrite() {
    let roots = roots();
    let target = roots.target_directory.path();
    fs::write(target.join("file"), b"old").unwrap();
    let file = path(b"file");
    let observed = roots.target.metadata(&file).unwrap();
    let destination = path(b".foldover/archive/run/file");

    fs::write(target.join("file"), b"edited").unwrap();
    assert!(matches!(
        roots
            .target
            .archive(&file, &observed, &destination, STANDARD),
        Err(WriteError::Changed)
    ));
    let observed = roots.target.metadata(&file).unwrap();
    roots
        .target
        .archive(&file, &observed, &destination, STANDARD)
        .unwrap();
    assert_eq!(
        fs::read(target.join(".foldover/archive/run/file")).unwrap(),
        b"edited"
    );

    fs::write(target.join("file"), b"again").unwrap();
    let observed = roots.target.metadata(&file).unwrap();
    assert!(matches!(
        roots
            .target
            .archive(&file, &observed, &destination, STANDARD),
        Err(WriteError::Exists)
    ));
    assert_eq!(fs::read(target.join("file")).unwrap(), b"again");
}

#[test]
fn symlinks_are_staged_with_exact_targets() {
    let roots = roots();
    let link = path(b"link");
    let staged = roots
        .target
        .stage_symlink(&link, b"../\xffoutside")
        .unwrap();
    assert_eq!(staged.metadata().unwrap().kind(), NodeKind::Symlink);
    roots
        .target
        .place(staged, &link, Placement::Create, STANDARD)
        .unwrap();
    assert_eq!(roots.target.read_link(&link).unwrap(), b"../\xffoutside");
}

#[test]
fn directories_are_created_with_source_metadata_and_removed_only_when_empty() {
    let roots = roots();
    let source = roots._source_directory.path();
    fs::create_dir(source.join("folder")).unwrap();
    fs::set_permissions(source.join("folder"), fs::Permissions::from_mode(0o750)).unwrap();
    let folder = path(b"folder");
    let observed = roots.source.metadata(&folder).unwrap();

    roots
        .target
        .create_directory(&folder, &roots.source, &observed, &ALL)
        .unwrap();
    let created = roots.target.metadata(&folder).unwrap();
    assert_eq!(created.kind(), NodeKind::Directory);
    assert_eq!(created.permissions() & 0o7777, 0o750);
    assert!(matches!(
        roots
            .target
            .create_directory(&folder, &roots.source, &observed, &ALL),
        Err(WriteError::Exists)
    ));

    fs::write(roots.target_directory.path().join("folder/new"), b"").unwrap();
    let current = roots.target.metadata(&folder).unwrap();
    assert!(matches!(
        roots.target.remove_directory(&folder, &current, STANDARD),
        Err(WriteError::NotEmpty)
    ));
    fs::remove_file(roots.target_directory.path().join("folder/new")).unwrap();
    roots
        .target
        .remove_directory(&folder, &current, STANDARD)
        .unwrap();
    assert!(names(roots.target_directory.path()).is_empty());
}

#[test]
fn metadata_copies_replace_stale_attributes_and_require_fresh_observations() {
    let roots = roots();
    let source = roots._source_directory.path().join("file");
    let target = roots.target_directory.path().join("file");
    fs::write(&source, b"data").unwrap();
    fs::write(&target, b"data").unwrap();
    let handle = fs::File::open(&target).unwrap();
    rustix::fs::fsetxattr(
        &handle,
        ATTRIBUTE,
        b"stale",
        rustix::fs::XattrFlags::empty(),
    )
    .unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
    let file = path(b"file");
    let source_observed = roots.source.metadata(&file).unwrap();
    let target_observed = roots.target.metadata(&file).unwrap();

    roots
        .target
        .update_metadata(
            &file,
            &target_observed,
            &roots.source,
            &source_observed,
            &ALL,
        )
        .unwrap();
    let copied = roots.target.metadata(&file).unwrap();
    assert_eq!(copied.permissions() & 0o7777, 0o600);
    assert_eq!(copied.modified(), source_observed.modified());
    assert!(rustix::fs::fgetxattr(&handle, ATTRIBUTE, &mut [0; 16]).is_err());
    assert!(matches!(
        roots.target.update_metadata(
            &file,
            &target_observed,
            &roots.source,
            &source_observed,
            &ALL
        ),
        Err(WriteError::Changed)
    ));
}

#[test]
fn changed_sources_are_not_staged() {
    let roots = roots();
    let source = roots._source_directory.path().join("file");
    fs::write(&source, b"data").unwrap();
    let file = path(b"file");
    let observed = roots.source.metadata(&file).unwrap();
    fs::write(&source, b"edited").unwrap();
    assert!(matches!(
        roots
            .target
            .stage_file(&file, &roots.source, &observed, &ALL, &mut proceed),
        Err(WriteError::Source(HashError::Changed))
    ));
    assert!(names(roots.target_directory.path()).is_empty());

    let link = roots._source_directory.path().join("link");
    symlink("file", &link).unwrap();
    let link_observed = roots.source.metadata(&path(b"link")).unwrap();
    assert!(matches!(
        roots.target.stage_file(
            &path(b"link"),
            &roots.source,
            &link_observed,
            &ALL,
            &mut proceed
        ),
        Err(WriteError::Source(HashError::NotRegular))
    ));
}

fn mode_of(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn ignored_categories_are_not_copied_but_the_executable_bit_is() {
    let roots = roots();
    let source = roots._source_directory.path().join("script");
    fs::write(&source, b"#!/bin/sh").unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
    let handle = fs::File::open(&source).unwrap();
    rustix::fs::fsetxattr(&handle, ATTRIBUTE, b"tag", rustix::fs::XattrFlags::empty()).unwrap();
    let past = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    fs::File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_modified(past)
        .unwrap();
    let script = path(b"script");
    let observed = roots.source.metadata(&script).unwrap();

    let staged = roots
        .target
        .stage_file(&script, &roots.source, &observed, &NOTHING, &mut proceed)
        .unwrap();
    let copied = staged.metadata().unwrap();
    assert_ne!(copied.modified(), past);
    assert!(
        staged
            .extended_metadata(&mut proceed)
            .unwrap()
            .attributes()
            .iter()
            .all(|attribute| attribute.name() != ATTRIBUTE.as_bytes())
    );
    // Executable for whoever may read, whatever the umask allows.
    let mode = copied.permissions() & 0o777;
    assert_eq!(mode & 0o100, 0o100);
    assert_eq!((mode & 0o444) >> 2, mode & 0o111);
    roots
        .target
        .place(staged.into_staged(), &script, Placement::Create, STANDARD)
        .unwrap();

    // Clearing the bit on the source clears it on the target too.
    fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
    let observed = roots.source.metadata(&script).unwrap();
    let target_observed = roots.target.metadata(&script).unwrap();
    roots
        .target
        .update_metadata(
            &script,
            &target_observed,
            &roots.source,
            &observed,
            &NOTHING,
        )
        .unwrap();
    let target = roots.target_directory.path().join("script");
    assert_eq!(mode_of(&target) & 0o111, 0);
    assert_eq!(mode_of(&target) & 0o666, mode & 0o666);
}

#[test]
fn synced_permissions_copy_exact_bits_but_never_setuid() {
    let roots = roots();
    let source = roots._source_directory.path().join("file");
    fs::write(&source, b"data").unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o4751)).unwrap();
    let file = path(b"file");
    let observed = roots.source.metadata(&file).unwrap();
    let staged = roots
        .target
        .stage_file(&file, &roots.source, &observed, &ALL, &mut proceed)
        .unwrap();
    assert_eq!(staged.metadata().unwrap().permissions() & 0o7777, 0o751);
}

#[test]
fn ignored_attributes_leave_existing_target_attributes_alone() {
    let roots = roots();
    let source = roots._source_directory.path().join("file");
    let target = roots.target_directory.path().join("file");
    fs::write(&source, b"data").unwrap();
    fs::write(&target, b"data").unwrap();
    let handle = fs::File::open(&target).unwrap();
    rustix::fs::fsetxattr(&handle, ATTRIBUTE, b"kept", rustix::fs::XattrFlags::empty()).unwrap();
    let file = path(b"file");
    let source_observed = roots.source.metadata(&file).unwrap();
    let target_observed = roots.target.metadata(&file).unwrap();
    roots
        .target
        .update_metadata(
            &file,
            &target_observed,
            &roots.source,
            &source_observed,
            &NOTHING,
        )
        .unwrap();
    let mut value = [0; 16];
    let size = rustix::fs::fgetxattr(&handle, ATTRIBUTE, &mut value).unwrap();
    assert_eq!(&value[..size], b"kept");
}

/// PLAN.md §4.10: a run that leaves Gatekeeper's mark alone neither
/// copies it nor removes the target's; other attributes still sync.
#[cfg(target_os = "macos")]
#[test]
fn quarantine_stays_behind_where_a_run_leaves_it_alone() {
    const QUARANTINE: &str = "com.apple.quarantine";
    let mark = |file: &std::path::Path, value: &[u8]| {
        let handle = fs::File::open(file).unwrap();
        rustix::fs::fsetxattr(&handle, QUARANTINE, value, rustix::fs::XattrFlags::empty()).unwrap();
    };
    let read = |file: &std::path::Path| {
        let handle = fs::File::open(file).unwrap();
        let mut value = [0; 64];
        rustix::fs::fgetxattr(&handle, QUARANTINE, &mut value)
            .ok()
            .map(|size| value[..size].to_vec())
    };
    let roots = roots();
    let source = roots._source_directory.path().join("file");
    let target = roots.target_directory.path().join("file");
    fs::write(&source, b"data").unwrap();
    fs::write(&target, b"data").unwrap();
    mark(&source, b"0081;6538a6f1;Safari;");
    mark(&target, b"0081;6538a6f2;Mail;");
    let file = path(b"file");
    let left_alone = ALL
        .clone()
        .with_metadata(ALL.metadata().with_quarantine(Propagation::Ignore));

    let source_observed = roots.source.metadata(&file).unwrap();
    let target_observed = roots.target.metadata(&file).unwrap();
    roots
        .target
        .update_metadata(
            &file,
            &target_observed,
            &roots.source,
            &source_observed,
            &left_alone,
        )
        .unwrap();
    assert_eq!(read(&target).unwrap(), b"0081;6538a6f2;Mail;");

    // New copies get it only where it travels.
    let copy = |options: &PairOptions, name: &[u8]| {
        let staged = roots
            .target
            .stage_file(
                &file,
                &roots.source,
                &source_observed,
                options,
                &mut proceed,
            )
            .unwrap();
        roots
            .target
            .place(
                staged.into_staged(),
                &path(name),
                Placement::Create,
                STANDARD,
            )
            .unwrap();
        read(
            &roots
                .target_directory
                .path()
                .join(std::str::from_utf8(name).unwrap()),
        )
    };
    assert_eq!(copy(&left_alone, b"left-alone"), None);
    assert_eq!(copy(&ALL, b"travels").unwrap(), b"0081;6538a6f1;Safari;");
    // Copying the ACL alone carries it on macOS; ignored attributes keep it out.
    let permissions_only = PairOptions::new(
        MetadataPolicy::new(Propagation::Sync, Propagation::Ignore, Propagation::Sync),
        STANDARD,
    );
    assert_eq!(copy(&permissions_only, b"permissions-only"), None);
}

#[cfg(target_os = "macos")]
#[test]
fn forks_always_copy_hidden_follows_attributes_and_locks_stay_behind() {
    let roots = roots();
    let source = roots._source_directory.path().join("file");
    fs::write(&source, b"data").unwrap();
    // Larger than one copy chunk, so positional writes are exercised.
    let fork: Vec<u8> = (0..1_536 * 1024).map(|index| (index % 251) as u8).collect();
    let handle = fs::File::open(&source).unwrap();
    rustix::fs::fsetxattr(
        &handle,
        "com.apple.ResourceFork",
        &fork,
        rustix::fs::XattrFlags::empty(),
    )
    .unwrap();
    let chflags = |flags: &str| {
        let status = std::process::Command::new("/usr/bin/chflags")
            .args([flags, source.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(status.success());
    };
    chflags("hidden,uchg");
    let file = path(b"file");
    let observed = roots.source.metadata(&file).unwrap();

    for (options, hidden) in [(NOTHING, false), (ALL, true)] {
        let staged = roots
            .target
            .stage_file(&file, &roots.source, &observed, &options, &mut proceed)
            .unwrap();
        let flags = staged.metadata().unwrap().flags().unwrap();
        assert_eq!(flags & libc::UF_HIDDEN != 0, hidden);
        assert_eq!(flags & libc::UF_IMMUTABLE, 0);
        let attributes = staged.extended_metadata(&mut proceed).unwrap();
        let copied = attributes
            .attributes()
            .iter()
            .find(|attribute| attribute.name() == b"com.apple.ResourceFork")
            .unwrap();
        assert_eq!(copied.size(), fork.len() as u64);
        assert_eq!(copied.digest().as_bytes(), blake3::hash(&fork).as_bytes());
    }
    chflags("nouchg");
}

#[test]
fn private_files_are_created_once_and_read_without_following_links() {
    let roots = roots();
    let id = path(b".foldover/id");
    assert_eq!(roots.target.read_private_file(&id, 64).unwrap(), None);
    roots
        .target
        .create_private_file(&id, b"one", STANDARD)
        .unwrap();
    assert!(matches!(
        roots.target.create_private_file(&id, b"two", STANDARD),
        Err(WriteError::Exists)
    ));
    assert_eq!(
        roots.target.read_private_file(&id, 64).unwrap().unwrap(),
        b"one"
    );
    assert!(roots.target.read_private_file(&id, 2).is_err());
    let target = roots.target_directory.path();
    assert_eq!(names(&target.join(".foldover")), [b"id".to_vec()]);

    let elsewhere = tempfile::tempdir().unwrap();
    fs::write(elsewhere.path().join("id"), b"outside").unwrap();
    fs::remove_file(target.join(".foldover/id")).unwrap();
    symlink(elsewhere.path().join("id"), target.join(".foldover/id")).unwrap();
    assert!(roots.target.read_private_file(&id, 64).is_err());
}

#[test]
fn trees_are_removed_without_following_links() {
    let roots = roots();
    let target = roots.target_directory.path();
    fs::create_dir_all(target.join("old/a/b")).unwrap();
    fs::write(target.join("old/a/b/file"), b"x").unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("keep"), b"keep").unwrap();
    symlink(outside.path(), target.join("old/a/link")).unwrap();
    fs::write(target.join("other"), b"y").unwrap();

    roots.target.remove_tree(&path(b"old")).unwrap();
    assert_eq!(names(target), [b"other".to_vec()]);
    assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"keep");
    roots.target.remove_tree(&path(b"other")).unwrap();
    assert!(names(target).is_empty());
    assert!(roots.target.remove_tree(&path(b"missing")).is_err());
}

#[test]
fn deep_trees_are_removed_without_hitting_a_descriptor_cap() {
    let roots = roots();
    let target = roots.target_directory.path();
    let mut deep = PathBuf::from("old");
    for _ in 0..140 {
        deep.push("d");
    }
    fs::create_dir_all(target.join(&deep)).unwrap();
    fs::write(target.join(&deep).join("file"), b"x").unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("keep"), b"keep").unwrap();
    symlink(outside.path(), target.join("old/d/link")).unwrap();

    roots.target.remove_tree(&path(b"old")).unwrap();
    assert!(names(target).is_empty());
    assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"keep");
}

#[test]
fn archived_entries_are_restored_without_replacing_anything() {
    let roots = roots();
    let target = roots.target_directory.path();
    fs::create_dir_all(target.join(".foldover/archive/run/docs")).unwrap();
    fs::write(target.join(".foldover/archive/run/docs/note"), b"old").unwrap();
    let archived = path(b".foldover/archive/run/docs/note");
    let restored = path(b"docs/note");

    roots
        .target
        .restore(&archived, &restored, STANDARD)
        .unwrap();
    assert_eq!(fs::read(target.join("docs/note")).unwrap(), b"old");
    assert_ne!(mode_of(&target.join("docs")), 0o700);

    fs::write(target.join(".foldover/archive/run/docs/note"), b"older").unwrap();
    assert!(matches!(
        roots.target.restore(&archived, &restored, STANDARD),
        Err(WriteError::Exists)
    ));
    assert_eq!(fs::read(target.join("docs/note")).unwrap(), b"old");
}

/// Attributes another Mac sends reach a streamed copy; an update replaces
/// them, and a source that keeps none, such as SFTP, leaves them alone.
#[test]
fn native_metadata_travels_with_streamed_copies() {
    let roots = roots();
    let file = path(b"photo.jpg");
    let native = NativeMetadata {
        attributes: vec![(ATTRIBUTE.as_bytes().to_vec(), b"blue".to_vec())],
        flags: None,
    };
    let foreign = ForeignMetadata {
        permissions: 0o644,
        modified: std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000),
    };
    let staged = roots
        .target
        .stage_stream(&file, foreign, Some(&native), &ALL, |sink| sink(b"pixels"))
        .unwrap();
    roots
        .target
        .place(staged.into_staged(), &file, Placement::Create, STANDARD)
        .unwrap();
    let placed = roots.target.metadata(&file).unwrap();
    let read = roots
        .target
        .native_metadata(&file, &placed, ALL.metadata())
        .unwrap();
    assert_eq!(read.attributes, native.attributes);

    // An SFTP-like source keeps no attributes: they stay.
    roots
        .target
        .update_foreign_metadata(&file, &placed, foreign, None, &ALL)
        .unwrap();
    let placed = roots.target.metadata(&file).unwrap();
    let kept = roots
        .target
        .native_metadata(&file, &placed, ALL.metadata())
        .unwrap();
    assert_eq!(kept.attributes, native.attributes);

    // Another Mac without the attribute removes it.
    roots
        .target
        .update_foreign_metadata(
            &file,
            &placed,
            foreign,
            Some(&NativeMetadata::default()),
            &ALL,
        )
        .unwrap();
    let placed = roots.target.metadata(&file).unwrap();
    let cleared = roots
        .target
        .native_metadata(&file, &placed, ALL.metadata())
        .unwrap();
    assert!(cleared.attributes.is_empty());

    // A pair that ignores attributes neither reads nor writes them.
    assert!(
        roots
            .target
            .native_metadata(&file, &placed, NOTHING.metadata())
            .unwrap()
            .attributes
            .is_empty()
    );
}

/// Device matrix: HFS+, exFAT and FAT answer `ENOTSUP` to exclusive
/// renames and swaps, so every copy to them failed. Placing, archiving
/// and private files keep their guarantees there without the flags.
#[test]
fn volumes_without_exclusive_renames_or_swaps_place_and_archive_alike() {
    rename::without_rename_flags(|| {
        let roots = roots();
        let target = roots.target_directory.path();
        let source = roots._source_directory.path();
        let file = path(b"file");

        // A new file lands, and never over one that appeared meanwhile.
        fs::write(source.join("file"), b"new").unwrap();
        let observed = roots.source.metadata(&file).unwrap();
        let stage = || {
            roots
                .target
                .stage_file(&file, &roots.source, &observed, &ALL, &mut proceed)
                .unwrap()
                .into_staged()
        };
        roots
            .target
            .place(stage(), &file, Placement::Create, STANDARD)
            .unwrap();
        assert_eq!(fs::read(target.join("file")).unwrap(), b"new");
        assert!(matches!(
            roots
                .target
                .place(stage(), &file, Placement::Create, STANDARD),
            Err(WriteError::Exists)
        ));
        assert_eq!(names(target), [b"file".to_vec()]);

        // A replacement displaces the old entry for archiving.
        fs::write(target.join("file"), b"old").unwrap();
        let old = roots.target.metadata(&file).unwrap();
        let displaced = roots
            .target
            .place(stage(), &file, Placement::Replace, STANDARD)
            .unwrap()
            .unwrap();
        assert_eq!(fs::read(target.join("file")).unwrap(), b"new");
        assert_eq!(displaced.metadata().unwrap().identity(), old.identity());
        let archived = path(b".foldover/archive/run/file");
        roots
            .target
            .archive_displaced(displaced, &archived, STANDARD)
            .unwrap();
        assert_eq!(
            fs::read(target.join(".foldover/archive/run/file")).unwrap(),
            b"old"
        );
        assert_eq!(names(target), [b".foldover".to_vec(), b"file".to_vec()]);

        // Replacing what is gone changes nothing.
        fs::remove_file(target.join("file")).unwrap();
        assert!(matches!(
            roots
                .target
                .place(stage(), &file, Placement::Replace, STANDARD),
            Err(WriteError::Changed)
        ));
        assert_eq!(names(target), [b".foldover".to_vec()]);

        // Archives never overwrite, and neither do private files.
        fs::write(target.join("file"), b"again").unwrap();
        let again = roots.target.metadata(&file).unwrap();
        assert!(matches!(
            roots.target.archive(&file, &again, &archived, STANDARD),
            Err(WriteError::Exists)
        ));
        assert_eq!(fs::read(target.join("file")).unwrap(), b"again");
        let id = path(b".foldover/id");
        roots
            .target
            .create_private_file(&id, b"one", STANDARD)
            .unwrap();
        assert!(matches!(
            roots.target.create_private_file(&id, b"two", STANDARD),
            Err(WriteError::Exists)
        ));
        assert_eq!(
            roots.target.read_private_file(&id, 64).unwrap().unwrap(),
            b"one"
        );
        assert_eq!(
            names(&target.join(".foldover")),
            [b"archive".to_vec(), b"id".to_vec()]
        );
    });
}

/// Without exclusive renames, a folder moves only where nothing is.
#[test]
fn folders_move_exclusively_without_rename_flags() {
    rename::without_rename_flags(|| {
        let roots = roots();
        let target = roots.target_directory.path();
        fs::create_dir_all(target.join("docs/inner")).unwrap();
        fs::write(target.join("docs/inner/note"), b"n").unwrap();
        let docs = path(b"docs");
        let observed = roots.target.metadata(&docs).unwrap();
        let archived = path(b".foldover/archive/run/docs");
        roots
            .target
            .archive(&docs, &observed, &archived, STANDARD)
            .unwrap();
        assert_eq!(
            fs::read(target.join(".foldover/archive/run/docs/inner/note")).unwrap(),
            b"n"
        );
        fs::create_dir(target.join("docs")).unwrap();
        assert!(matches!(
            roots.target.restore(&archived, &docs, STANDARD),
            Err(WriteError::Exists)
        ));
        assert!(
            target
                .join(".foldover/archive/run/docs/inner/note")
                .exists()
        );
    });
}
