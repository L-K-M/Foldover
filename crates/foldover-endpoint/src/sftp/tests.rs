//! Against OpenSSH's `sftp-server` on standard streams, like the driver's
//! tests. Skipped where the server is not installed.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use foldover_net::sftp::Session;
use foldover_types::{Durability, PairOptions};

use super::*;
use crate::{Endpoint, Expected, Filter, LocalEndpoint};

fn server() -> Option<PathBuf> {
    std::env::var_os("FOLDOVER_SFTP_SERVER")
        .map(PathBuf::from)
        .into_iter()
        .chain(
            [
                "/usr/libexec/sftp-server",
                "/usr/lib/openssh/sftp-server",
                "/usr/lib/ssh/sftp-server",
            ]
            .map(PathBuf::from),
        )
        .find(|path| path.exists())
}

/// An SFTP endpoint over a local `sftp-server`, rooted at `directory`.
fn remote(directory: &Path) -> Option<Endpoint> {
    let Some(server) = server() else {
        eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
        return None;
    };
    let session = Session::spawn(&server, &[OsStr::new("-d"), OsStr::new("/")]).unwrap();
    let root = directory.as_os_str().as_encoded_bytes();
    Some(Endpoint::Sftp(
        SftpEndpoint::with_session(session, root).unwrap(),
    ))
}

fn proceed() -> ScanControl {
    ScanControl::Continue
}

fn path(bytes: &[u8]) -> SyncPath {
    SyncPath::try_from(bytes).unwrap()
}

#[test]
fn copies_both_ways_keep_content_and_archive_what_they_replace() {
    let local_directory = tempfile::tempdir().unwrap();
    let remote_directory = tempfile::tempdir().unwrap();
    let Some(remote) = remote(remote_directory.path()) else {
        return;
    };
    let local = Endpoint::Local(LocalEndpoint::open(local_directory.path()).unwrap());
    let options = PairOptions::default();
    let policy = options.metadata();
    let run = RunScope::new(UNIX_EPOCH, options.clone());

    fs::create_dir(local_directory.path().join("docs")).unwrap();
    fs::write(local_directory.path().join("docs/a.txt"), b"local data").unwrap();
    std::os::unix::fs::symlink("a.txt", local_directory.path().join("docs/link")).unwrap();
    fs::write(
        remote_directory.path().join("from-server"),
        vec![7; 200_000],
    )
    .unwrap();

    let observe = |endpoint: &Endpoint| {
        endpoint
            .observe_from(
                policy,
                &Filter::none(),
                &std::collections::BTreeMap::new(),
                &mut proceed,
            )
            .unwrap()
    };
    let here = observe(&local);
    let there = observe(&remote);
    assert_eq!(
        there.entries().keys().collect::<Vec<_>>(),
        [&path(b"from-server")]
    );

    // Local to server: a folder, a file and a link.
    for name in [b"docs".as_slice(), b"docs/a.txt", b"docs/link"] {
        let name = path(name);
        let stored = remote
            .copy_from(
                &local,
                &name,
                &here.entries()[&name],
                Expected::Absent,
                &run,
                &mut proceed,
            )
            .unwrap();
        assert_eq!(stored.content(), here.entries()[&name].version().content());
    }
    assert_eq!(
        fs::read(remote_directory.path().join("docs/a.txt")).unwrap(),
        b"local data"
    );
    assert_eq!(
        fs::read_link(remote_directory.path().join("docs/link")).unwrap(),
        PathBuf::from("a.txt")
    );

    // Server to local.
    let name = path(b"from-server");
    local
        .copy_from(
            &remote,
            &name,
            &there.entries()[&name],
            Expected::Absent,
            &run,
            &mut proceed,
        )
        .unwrap();
    assert_eq!(
        fs::read(local_directory.path().join("from-server")).unwrap(),
        vec![7; 200_000]
    );

    // A replacement on the server archives the previous version there.
    fs::write(local_directory.path().join("docs/a.txt"), b"newer").unwrap();
    let here = observe(&local);
    let there = observe(&remote);
    let name = path(b"docs/a.txt");
    remote
        .copy_from(
            &local,
            &name,
            &here.entries()[&name],
            Expected::Present(&there.entries()[&name]),
            &run,
            &mut proceed,
        )
        .unwrap();
    assert_eq!(
        fs::read(remote_directory.path().join("docs/a.txt")).unwrap(),
        b"newer"
    );
    let archived = remote_directory
        .path()
        .join(".foldover/archive")
        .join(run.name())
        .join("docs/a.txt");
    assert_eq!(fs::read(archived).unwrap(), b"local data");

    // A file that changed since the plan is not overwritten.
    fs::write(
        remote_directory.path().join("docs/a.txt"),
        b"edited meanwhile",
    )
    .unwrap();
    let error = remote
        .copy_from(
            &local,
            &name,
            &here.entries()[&name],
            Expected::Present(&there.entries()[&name]),
            &run,
            &mut proceed,
        )
        .unwrap_err();
    assert!(
        matches!(error, ApplyError::DestinationChanged(_)),
        "{error:?}"
    );
    assert_eq!(
        fs::read(remote_directory.path().join("docs/a.txt")).unwrap(),
        b"edited meanwhile"
    );
}

#[test]
fn identities_removals_and_pruning_follow_the_local_rules() {
    let directory = tempfile::tempdir().unwrap();
    let Some(remote) = remote(directory.path()) else {
        return;
    };
    assert_eq!(remote.identity().unwrap(), None);
    let identity = remote.ensure_identity(Durability::Full).unwrap();
    assert_eq!(remote.identity().unwrap(), Some(identity));
    assert_eq!(
        remote.ensure_identity(Durability::Standard).unwrap(),
        identity
    );

    fs::write(directory.path().join(".foldoverignore"), b"*.tmp\n").unwrap();
    assert_eq!(remote.ignore_rules().unwrap(), ["*.tmp"]);
    fs::write(directory.path().join("gone"), b"bye").unwrap();
    fs::create_dir(directory.path().join("empty")).unwrap();
    let options = PairOptions::default();
    let run = RunScope::new(UNIX_EPOCH, options.clone());
    let observed = remote
        .observe_from(
            options.metadata(),
            &Filter::none(),
            &std::collections::BTreeMap::new(),
            &mut proceed,
        )
        .unwrap();
    for name in [b"gone".as_slice(), b"empty"] {
        let name = path(name);
        remote
            .remove(&name, &observed.entries()[&name], &run, &mut proceed)
            .unwrap();
    }
    assert!(!directory.path().join("gone").exists());
    assert!(!directory.path().join("empty").exists());
    assert_eq!(remote.archived_runs().unwrap()[0].name(), run.name());
    let placed = remote
        .put_back(run.name(), &path(b"gone"), Durability::Standard)
        .unwrap();
    assert_eq!(placed, path(b"gone"));
    assert_eq!(fs::read(directory.path().join("gone")).unwrap(), b"bye");

    let pruned = remote
        .prune_archive(Duration::from_secs(1), SystemTime::now())
        .unwrap();
    assert_eq!(pruned, 1);
    assert!(remote.archived_runs().unwrap().is_empty());
}
