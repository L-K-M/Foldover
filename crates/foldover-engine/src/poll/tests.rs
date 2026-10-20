use std::fs;
use std::path::Path;

use foldover_types::{LocalRoot, PairDraft, PairId, SyncMode};

use super::*;

fn endpoint(path: &Path) -> Endpoint {
    let root = PairRoot::from_bytes(path.as_os_str().as_encoded_bytes().to_vec()).unwrap();
    Endpoint::open(&root, &Reach::default()).unwrap()
}

fn everything() -> Filter {
    Filter::new([]).unwrap()
}

#[test]
fn the_first_check_only_looks_and_a_new_file_is_a_change() {
    let directory = tempfile::tempdir().unwrap();
    let folder = endpoint(directory.path());
    let mut poll = FolderPoll::default();

    fs::write(directory.path().join("a.txt"), b"a").unwrap();
    assert!(!poll.check(&folder, &everything()).unwrap());
    assert!(!poll.check(&folder, &everything()).unwrap());

    fs::write(directory.path().join("b.txt"), b"b").unwrap();
    assert!(poll.check(&folder, &everything()).unwrap());
    assert!(!poll.check(&folder, &everything()).unwrap());
}

#[test]
fn a_directory_that_changed_is_watched_closely_from_then_on() {
    let directory = tempfile::tempdir().unwrap();
    let photos = directory.path().join("photos");
    let docs = directory.path().join("docs");
    fs::create_dir(&photos).unwrap();
    fs::create_dir(&docs).unwrap();
    fs::write(photos.join("a.jpg"), b"a").unwrap();
    fs::write(docs.join("c.txt"), b"c").unwrap();
    let folder = endpoint(directory.path());
    let mut poll = FolderPoll::default();
    assert!(!poll.check(&folder, &everything()).unwrap());

    // A new photo changes the folder's own entry in the root.
    fs::write(photos.join("b.jpg"), b"b").unwrap();
    assert!(poll.check(&folder, &everything()).unwrap());

    // An edit in place shows only inside the folder, now listed.
    fs::write(photos.join("a.jpg"), b"a, edited").unwrap();
    assert!(poll.check(&folder, &everything()).unwrap());

    // Elsewhere an edit in place waits for the verification run.
    fs::write(docs.join("c.txt"), b"c, edited").unwrap();
    assert!(!poll.check(&folder, &everything()).unwrap());
}

#[test]
fn foldovers_own_files_and_filtered_ones_are_not_changes() {
    let directory = tempfile::tempdir().unwrap();
    let folder = endpoint(directory.path());
    let filter = Filter::new(["*.tmp"]).unwrap();
    let mut poll = FolderPoll::default();
    assert!(!poll.check(&folder, &filter).unwrap());

    fs::create_dir(directory.path().join(".foldover")).unwrap();
    fs::write(directory.path().join(".foldover/lock"), b"lease").unwrap();
    fs::write(directory.path().join(".DS_Store"), b"finder").unwrap();
    fs::write(directory.path().join("draft.tmp"), b"scratch").unwrap();
    assert!(!poll.check(&folder, &filter).unwrap());
}

#[test]
fn an_active_directory_that_vanished_is_a_change_once() {
    let directory = tempfile::tempdir().unwrap();
    let folder = endpoint(directory.path());
    let mut poll = FolderPoll::default();
    assert!(!poll.check(&folder, &everything()).unwrap());
    fs::create_dir(directory.path().join("photos")).unwrap();
    assert!(poll.check(&folder, &everything()).unwrap());
    assert_eq!(poll.active.len(), 1);

    fs::remove_dir(directory.path().join("photos")).unwrap();
    assert!(poll.check(&folder, &everything()).unwrap());
    assert!(poll.active.is_empty());
    assert!(!poll.check(&folder, &everything()).unwrap());
}

#[test]
fn pairs_on_local_disks_need_no_polling() {
    let directory = tempfile::tempdir().unwrap();
    let (left, right) = (
        directory.path().join("left"),
        directory.path().join("right"),
    );
    fs::create_dir(&left).unwrap();
    fs::create_dir(&right).unwrap();
    let root =
        |path: &Path| LocalRoot::from_bytes(path.as_os_str().as_encoded_bytes().to_vec()).unwrap();
    let draft = PairDraft::new("Disk".into(), root(&left), root(&right), SyncMode::Sync).unwrap();
    let pair =
        PairConfig::from_stored(PairId::from_bytes([1; PairId::BYTE_LEN]).unwrap(), 1, draft)
            .unwrap();
    let polled = ChangePoll::default().check(&pair, &Reach::default(), PollMode::Compare);
    assert_eq!(polled.unwrap(), Polled::NotNeeded);
}

#[test]
fn an_sftp_folder_is_listed_like_a_local_one() {
    let Some(server) = std::env::var_os("FOLDOVER_SFTP_SERVER")
        .map(std::path::PathBuf::from)
        .into_iter()
        .chain(
            [
                "/usr/libexec/sftp-server",
                "/usr/lib/openssh/sftp-server",
                "/usr/lib/ssh/sftp-server",
            ]
            .map(std::path::PathBuf::from),
        )
        .find(|path| path.exists())
    else {
        eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let endpoint = foldover_endpoint::SftpEndpoint::spawn(
        &server,
        &[std::ffi::OsStr::new("-d"), std::ffi::OsStr::new("/")],
        directory.path().as_os_str().as_encoded_bytes(),
    )
    .unwrap();
    let folder = Endpoint::Sftp(endpoint);
    let mut poll = FolderPoll::default();
    assert!(!poll.check(&folder, &everything()).unwrap());

    fs::create_dir(directory.path().join("photos")).unwrap();
    assert!(poll.check(&folder, &everything()).unwrap());
    fs::write(directory.path().join("photos/a.jpg"), b"a").unwrap();
    assert!(poll.check(&folder, &everything()).unwrap());
    assert!(!poll.check(&folder, &everything()).unwrap());
}
