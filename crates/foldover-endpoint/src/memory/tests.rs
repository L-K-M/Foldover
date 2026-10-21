use std::collections::BTreeMap;

use foldover_types::{CaseMatching, PairOptions};

use super::*;
use crate::Endpoint;

fn path(text: &str) -> SyncPath {
    SyncPath::try_from(text.as_bytes().to_vec()).unwrap()
}

fn run() -> RunScope {
    RunScope::new(
        UNIX_EPOCH + Duration::from_secs(EPOCH_SECONDS),
        PairOptions::default(),
    )
}

fn observe(endpoint: &Endpoint) -> Observation {
    endpoint
        .observe_from(
            MetadataPolicy::default(),
            &Filter::none(),
            &BTreeMap::new(),
            &mut || ScanControl::Continue,
            &mut |_| {},
        )
        .unwrap()
}

/// Copy `name` from `from` to `to`, whatever `to` holds there now.
fn copy(from: &Endpoint, to: &Endpoint, name: &str) -> Result<EntryVersion, ApplyError> {
    let source = observe(from);
    let target = observe(to);
    let path = path(name);
    let current = match target.entries().get(&path) {
        Some(entry) => Expected::Present(entry),
        None => Expected::Absent,
    };
    to.copy_from(
        from,
        &path,
        &source.entries()[&path],
        current,
        &run(),
        &mut || ScanControl::Continue,
    )
}

fn names(folder: &MemoryEndpoint) -> Vec<String> {
    folder
        .user_files()
        .into_keys()
        .map(|name| String::from_utf8(name).unwrap())
        .collect()
}

#[test]
fn a_case_insensitive_folder_keeps_one_file_under_its_first_name() {
    let folder = MemoryEndpoint::new(Quirks {
        names: NameMatching::new(CaseMatching::Insensitive, UnicodeMatching::Exact),
        ..Quirks::default()
    });
    folder.put_file(b"Docs/Report.txt", b"one").unwrap();
    folder.put_file(b"docs/report.txt", b"two").unwrap();
    // Writing opened the file there: both kept their names.
    assert_eq!(names(&folder), ["Docs/Report.txt"]);
    assert_eq!(folder.user_files()[b"Docs/Report.txt".as_slice()], b"two");
}

#[test]
fn a_decomposing_folder_stores_names_decomposed() {
    let folder = MemoryEndpoint::new(Quirks {
        names: NameMatching::new(CaseMatching::Exact, UnicodeMatching::Normalized),
        storage: NameStorage::Decomposed,
        ..Quirks::default()
    });
    folder.put_file("caf\u{e9}.txt".as_bytes(), b"x").unwrap();
    assert_eq!(names(&folder), ["cafe\u{301}.txt"]);
    // The composed name still finds it.
    folder.put_file("caf\u{e9}.txt".as_bytes(), b"y").unwrap();
    assert_eq!(folder.user_files().len(), 1);
}

#[test]
fn coarse_times_round_down() {
    let folder = MemoryEndpoint::new(Quirks {
        time_resolution: Duration::from_secs(2),
        ..Quirks::default()
    });
    folder.put_file(b"a", b"a").unwrap();
    let modified = folder.folder().get(b"a").unwrap().node.modified();
    let since = modified.duration_since(UNIX_EPOCH).unwrap();
    assert_eq!(since.subsec_nanos(), 0);
    assert_eq!(since.as_secs() % 2, 0);
}

#[test]
fn a_replaced_file_is_archived_and_a_copy_reports_the_stored_version() {
    let (left, right) = (
        MemoryEndpoint::new(Quirks::default()),
        MemoryEndpoint::new(Quirks::default()),
    );
    left.put_file(b"a", b"new").unwrap();
    right.put_file(b"a", b"old").unwrap();
    let (from, to) = (Endpoint::Memory(left), Endpoint::Memory(right.clone()));
    let stored = copy(&from, &to, "a").unwrap();
    assert_eq!(stored, observe(&to).entries()[&path("a")].version());
    assert_eq!(right.user_files()[b"a".as_slice()], b"new");
    assert_eq!(right.archived_files(), [b"old".to_vec()]);
}

#[test]
fn a_two_step_replace_cut_short_leaves_the_old_archived_and_the_new_aside() {
    let left = MemoryEndpoint::new(Quirks::default());
    let right = MemoryEndpoint::new(Quirks {
        replace: Replace::TwoStep,
        ..Quirks::default()
    });
    left.put_file(b"a", b"new").unwrap();
    right.put_file(b"a", b"old").unwrap();
    // Staging and archiving succeed; moving the new file in does not.
    right.inject_fault(2);
    let (from, to) = (Endpoint::Memory(left), Endpoint::Memory(right.clone()));
    assert!(copy(&from, &to, "a").is_err());

    right.clear_fault();
    let observed = observe(&to);
    assert!(observed.entries().is_empty());
    assert_eq!(observed.leftovers().len(), 1);
    assert_eq!(right.archived_files(), [b"old".to_vec()]);
    to.recover_leftover(&observed.leftovers()[0], &run())
        .unwrap();
    let mut archived = right.archived_files();
    archived.sort();
    assert_eq!(archived, [b"new".to_vec(), b"old".to_vec()]);
}

#[test]
fn an_atomic_replace_cut_short_keeps_the_new_file_and_sets_the_old_aside() {
    let (left, right) = (
        MemoryEndpoint::new(Quirks::default()),
        MemoryEndpoint::new(Quirks::default()),
    );
    left.put_file(b"a", b"new").unwrap();
    right.put_file(b"a", b"old").unwrap();
    right.inject_fault(1);
    let (from, to) = (Endpoint::Memory(left), Endpoint::Memory(right.clone()));
    assert!(copy(&from, &to, "a").is_err());
    right.clear_fault();
    let observed = observe(&to);
    assert_eq!(right.user_files()[b"a".as_slice()], b"new");
    assert_eq!(observed.leftovers().len(), 1);
}

#[test]
fn a_full_folder_refuses_writes_until_space_is_freed() {
    let left = MemoryEndpoint::new(Quirks::default());
    let right = MemoryEndpoint::new(Quirks {
        capacity: Some(4),
        ..Quirks::default()
    });
    left.put_file(b"big", b"too large").unwrap();
    let (from, to) = (Endpoint::Memory(left), Endpoint::Memory(right.clone()));
    assert_eq!(to.available_space().unwrap(), 4);
    let error = copy(&from, &to, "big").unwrap_err();
    assert!(
        matches!(&error, ApplyError::Write { source: WriteError::Io(io), .. } if io.kind() == io::ErrorKind::StorageFull),
        "{error:?}"
    );
    right.set_capacity(None);
    copy(&from, &to, "big").unwrap();
}

#[test]
fn identities_and_lock_files_live_in_foldovers_own_folder() {
    let folder = MemoryEndpoint::new(Quirks::default());
    let endpoint = Endpoint::Memory(folder.clone());
    assert_eq!(endpoint.identity().unwrap(), None);
    let identity = endpoint.ensure_identity(Durability::Standard).unwrap();
    assert_eq!(endpoint.identity().unwrap(), Some(identity));

    assert!(endpoint.create_lock_file(LockFile::Lock, b"lease").unwrap());
    assert!(!endpoint.create_lock_file(LockFile::Lock, b"other").unwrap());
    assert!(
        endpoint
            .create_lock_file(LockFile::Claim, b"claim")
            .unwrap()
    );
    endpoint.promote_claim().unwrap();
    assert_eq!(
        endpoint.read_lock_file(LockFile::Lock).unwrap(),
        Some(b"claim".to_vec())
    );
    assert_eq!(endpoint.read_lock_file(LockFile::Claim).unwrap(), None);
    // None of it is the person's.
    assert!(folder.user_files().is_empty());
    assert!(observe(&endpoint).entries().is_empty());
}

#[test]
fn a_listing_shows_one_directory_without_foldovers_names() {
    let folder = MemoryEndpoint::new(Quirks::default());
    folder.put_file(b"a.txt", b"a").unwrap();
    folder.put_file(b"docs/b.txt", b"b").unwrap();
    let endpoint = Endpoint::Memory(folder);
    endpoint.ensure_identity(Durability::Standard).unwrap();
    let names = |directory: Option<&SyncPath>| -> Vec<Vec<u8>> {
        endpoint
            .list(directory, &Filter::none())
            .unwrap()
            .unwrap()
            .iter()
            .map(|entry| entry.name().to_vec())
            .collect()
    };
    assert_eq!(names(None), [b"a.txt".to_vec(), b"docs".to_vec()]);
    assert_eq!(names(Some(&path("docs"))), [b"b.txt".to_vec()]);
    assert!(
        endpoint
            .list(Some(&path("gone")), &Filter::none())
            .unwrap()
            .is_none()
    );
}

/// Review of dataless entries: the in-memory folder reported a filtered
/// evicted folder as unavailable where a disk leaves it out, so
/// simulations kept its base while the real folder forgot it.
#[test]
fn what_the_filter_leaves_out_is_not_reported_evicted() {
    let folder = MemoryEndpoint::new(Quirks::default());
    folder.put_file(b"cache/blob", b"x").unwrap();
    folder.put_file(b"notes.txt", b"n").unwrap();
    folder.evict(b"cache");
    folder.evict(b"notes.txt");
    let observed = Endpoint::Memory(folder)
        .observe_from(
            MetadataPolicy::default(),
            &Filter::new(["cache/"]).unwrap(),
            &BTreeMap::new(),
            &mut || ScanControl::Continue,
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(observed.unavailable(), [path("notes.txt")]);
}
