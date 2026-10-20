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

/// A held SFTP endpoint over a local `sftp-server`, or `None` without one.
fn sftp(directory: &Path) -> Option<SftpEndpoint> {
    match remote(directory)? {
        Endpoint::Sftp(endpoint) => Some(endpoint),
        _ => unreachable!(),
    }
}

/// The real `sftp-server` behind a filter that drops `fsync@openssh.com`
/// from the version handshake, so the session behaves like a server
/// without the extension while every other operation stays real.
const NO_FSYNC_PROXY: &str = r#"
import os, struct, subprocess, sys, threading

child = subprocess.Popen(
    sys.argv[1:], stdin=subprocess.PIPE, stdout=subprocess.PIPE
)


def requests():
    while True:
        chunk = os.read(0, 65536)
        if not chunk:
            break
        child.stdin.write(chunk)
        child.stdin.flush()
    child.stdin.close()


threading.Thread(target=requests, daemon=True).start()

while True:
    head = child.stdout.read(4)
    if len(head) < 4:
        break
    (length,) = struct.unpack(">I", head)
    body = child.stdout.read(length)
    if len(body) < length:
        break
    if body[0] == 2 and len(body) >= 5:  # SSH_FXP_VERSION
        kept = b""
        rest = body[5:]
        while rest:
            (size,) = struct.unpack(">I", rest[:4])
            name = rest[4 : 4 + size]
            rest = rest[4 + size :]
            (size,) = struct.unpack(">I", rest[:4])
            data = rest[4 : 4 + size]
            rest = rest[4 + size :]
            if name != b"fsync@openssh.com":
                kept += struct.pack(">I", len(name)) + name
                kept += struct.pack(">I", len(data)) + data
        body = body[:5] + kept
        head = struct.pack(">I", len(body))
    sys.stdout.buffer.write(head + body)
    sys.stdout.buffer.flush()

child.wait()
"#;

/// An endpoint whose server speaks SFTP v3 but not the fsync extension,
/// or `None` without `python3` and an `sftp-server`.
fn without_fsync(directory: &Path) -> Option<SftpEndpoint> {
    let python = std::process::Command::new("python3")
        .arg("--version")
        .status()
        .ok()
        .filter(|status| status.success())
        .map(|_| PathBuf::from("python3"));
    let (Some(python), Some(server)) = (python, server()) else {
        eprintln!("python3 or sftp-server missing; no-fsync fixture skipped");
        return None;
    };
    let arguments = [
        OsStr::new("-c"),
        OsStr::new(NO_FSYNC_PROXY),
        server.as_os_str(),
        OsStr::new("-d"),
        OsStr::new("/"),
    ];
    let root = directory.as_os_str().as_encoded_bytes();
    Some(SftpEndpoint::spawn(&python, &arguments, root).unwrap())
}

fn proceed() -> ScanControl {
    ScanControl::Continue
}

fn path(bytes: &[u8]) -> SyncPath {
    SyncPath::try_from(bytes).unwrap()
}

#[test]
fn roots_relate_only_on_one_server_and_hosts_fold_case() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("inner")).unwrap();
    let Some(mut outer) = sftp(directory.path()) else {
        return;
    };
    let Some(mut inner) = sftp(&directory.path().join("inner")) else {
        return;
    };
    // `open` stores `host:port` as the locator spelled it; DNS names are
    // case-insensitive, so `NAS` and `nas` are the same server.
    outer.server = Some("NAS:22".into());
    inner.server = Some("nas:22".into());
    assert_eq!(outer.relation_to(&inner), crate::RootRelation::Contains);
    assert_eq!(inner.relation_to(&outer), crate::RootRelation::Within);
    assert_eq!(inner.relation_to(&inner), crate::RootRelation::Same);

    // Another port or an unknown server never overlaps.
    inner.server = Some("nas:2222".into());
    assert_eq!(outer.relation_to(&inner), crate::RootRelation::Disjoint);
    inner.server = None;
    assert_eq!(outer.relation_to(&inner), crate::RootRelation::Disjoint);
    inner.server = Some("nas:22".into());
    outer.server = Some("other:22".into());
    assert_eq!(outer.relation_to(&inner), crate::RootRelation::Disjoint);

    // Synthetic locators test raw comparison on case-insensitive hosts too.
    outer.server = Some("nas:22".into());
    outer.root.extend_from_slice(b"/Inner");
    assert_eq!(inner.relation_to(&outer), crate::RootRelation::Disjoint);
}

#[test]
fn archived_runs_are_only_directories_named_like_runs() {
    let directory = tempfile::tempdir().unwrap();
    let Some(remote) = sftp(directory.path()) else {
        return;
    };
    let archive = directory.path().join(".foldover/archive");
    let run = crate::local::archive::archive_name(UNIX_EPOCH + Duration::from_secs(1_700_000_000));
    let file = crate::local::archive::archive_name(UNIX_EPOCH + Duration::from_secs(1_700_000_001));
    let link = crate::local::archive::archive_name(UNIX_EPOCH + Duration::from_secs(1_700_000_002));
    fs::create_dir_all(archive.join(&run)).unwrap();
    fs::write(archive.join(&file), b"not a run").unwrap();
    std::os::unix::fs::symlink(&run, archive.join(&link)).unwrap();
    fs::create_dir(archive.join("kept by the user")).unwrap();

    let runs: Vec<String> = remote
        .archived_runs()
        .unwrap()
        .iter()
        .map(|run| run.name().to_owned())
        .collect();
    assert_eq!(runs, [run]);
}

/// Review: each entry was listed and checked back to back, so the
/// comparison read nearly complete from the first file.
#[test]
fn the_whole_tree_is_listed_before_any_entry_is_checked() {
    let directory = tempfile::tempdir().unwrap();
    let Some(remote) = sftp(directory.path()) else {
        return;
    };
    fs::create_dir(directory.path().join("folder")).unwrap();
    for name in ["a", "b", "folder/c", "folder/d"] {
        fs::write(directory.path().join(name), name).unwrap();
    }
    let mut steps = Vec::new();
    remote
        .observe_reporting(
            PairOptions::default().metadata(),
            &Filter::none(),
            &std::collections::BTreeMap::new(),
            &mut proceed,
            &mut |step| steps.push(step),
        )
        .unwrap();
    let listed = vec![ObserveStep::Listed; 5];
    let checked = vec![ObserveStep::Checked; 5];
    assert_eq!(steps, [listed, checked].concat());
}

#[test]
fn oversized_sidecars_are_excluded_before_classification() {
    let directory = tempfile::tempdir().unwrap();
    let Some(remote) = sftp(directory.path()) else {
        return;
    };
    let filter = Filter::none().with_size_limit(Some(8));
    let options = PairOptions::default();
    fs::write(directory.path().join("._heavy"), vec![0; 100]).unwrap();
    fs::write(directory.path().join("plain"), b"content").unwrap();
    let observed = remote
        .observe_from(
            options.metadata(),
            &filter,
            &std::collections::BTreeMap::new(),
            &mut proceed,
        )
        .unwrap();
    assert_eq!(observed.oversized(), &[path(b"._heavy")]);
    assert!(observed.entries().contains_key(&path(b"plain")));

    // A sidecar inside the limits still stops the scan.
    fs::write(directory.path().join("._tiny"), b"1234").unwrap();
    assert!(matches!(
        remote.observe_from(
            options.metadata(),
            &filter,
            &std::collections::BTreeMap::new(),
            &mut proceed,
        ),
        Err(ScanError::UnclassifiedSidecar(sidecar)) if sidecar == path(b"._tiny")
    ));
}

#[test]
fn unchanged_sftp_attributes_cannot_hide_a_content_change() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("note");
    let settled = UNIX_EPOCH + Duration::from_secs(1);
    let set_time = || {
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(settled))
            .unwrap();
    };
    fs::write(&file, b"before").unwrap();
    set_time();
    let Some(remote) = sftp(directory.path()) else {
        return;
    };
    let options = PairOptions::default();
    let first = remote
        .observe_from(
            options.metadata(),
            &Filter::none(),
            &Default::default(),
            &mut proceed,
        )
        .unwrap();
    let before = remote.session().lstat(&remote.remote(b"note")).unwrap();

    // Preserving size and mtime is normal for remote copy/edit tools.
    fs::write(&file, b"after!").unwrap();
    set_time();
    let after = remote.session().lstat(&remote.remote(b"note")).unwrap();
    assert_eq!(stat_key(&before), stat_key(&after));
    let second = remote
        .observe_from(
            options.metadata(),
            &Filter::none(),
            first.entries(),
            &mut proceed,
        )
        .unwrap();
    let key = path(b"note");
    assert_ne!(
        first.versions()[&key].content(),
        second.versions()[&key].content()
    );
    let run = RunScope::new(UNIX_EPOCH, options);
    assert!(matches!(
        remote.remove(&key, &first.entries()[&key], &run, &mut proceed),
        Err(ApplyError::DestinationChanged(_))
    ));
    assert_eq!(fs::read(&file).unwrap(), b"after!");
    assert!(!directory.path().join(".foldover/archive").exists());
}

#[test]
fn reads_stop_at_the_observed_length_and_refuse_a_changed_file() {
    let directory = tempfile::tempdir().unwrap();
    let Some(remote) = sftp(directory.path()) else {
        return;
    };
    let name = path(b"data");

    // A file that grew since its stat streams nothing past it.
    fs::write(directory.path().join("data"), vec![3; 100]).unwrap();
    let before = remote.attributes(&name).unwrap().unwrap();
    fs::write(directory.path().join("data"), vec![9; 250]).unwrap();
    let mut received = Vec::new();
    let read = remote.read_file(&name, &before, &mut proceed, &mut |chunk| {
        received.extend_from_slice(chunk);
        Ok(())
    });
    assert!(matches!(read, Err(ScanError::Changed(_))), "{read:?}");
    assert!(
        received.len() <= 100,
        "the sink saw {} bytes",
        received.len()
    );

    // A file that shrank does not pass as its old stat either.
    fs::write(directory.path().join("data"), vec![3; 100]).unwrap();
    let before = remote.attributes(&name).unwrap().unwrap();
    fs::write(directory.path().join("data"), vec![3; 40]).unwrap();
    let read = remote.read_file(&name, &before, &mut proceed, &mut |_| Ok(()));
    assert!(matches!(read, Err(ScanError::Changed(_))), "{read:?}");
}

#[test]
fn keep_copy_streams_a_file_beside_the_original_and_cancels_cleanly() {
    let directory = tempfile::tempdir().unwrap();
    let Some(remote) = sftp(directory.path()) else {
        return;
    };
    let options = PairOptions::default();
    let data: Vec<u8> = (0..200_000).map(|index| index as u8).collect();
    fs::write(directory.path().join("report.bin"), &data).unwrap();
    let observed = remote
        .observe_from(
            options.metadata(),
            &Filter::none(),
            &std::collections::BTreeMap::new(),
            &mut proceed,
        )
        .unwrap();
    let name = path(b"report.bin");
    let version = observed.entries()[&name].version();

    let copy = remote
        .keep_copy(&name, version, "Folder B", &options, &mut proceed)
        .unwrap();
    let copied = directory
        .path()
        .join(std::str::from_utf8(copy.as_bytes()).unwrap());
    assert_eq!(fs::read(&copied).unwrap(), data);
    assert_eq!(fs::read(directory.path().join("report.bin")).unwrap(), data);

    // A second copy takes the next free name.
    let second = remote
        .keep_copy(&name, version, "Folder B", &options, &mut proceed)
        .unwrap();
    assert!(second.as_bytes().ends_with(b" 2).bin"), "{second:?}");

    // Cancelling mid-copy removes the staged temporary. The version
    // check reads the file once first, so the control is polled about
    // seven times before the copy's own chunks arrive.
    let mut calls = 0;
    let cancelled = remote.keep_copy(&name, version, "Folder B", &options, &mut || {
        calls += 1;
        if calls > 9 {
            ScanControl::Cancel
        } else {
            ScanControl::Continue
        }
    });
    assert!(
        matches!(cancelled, Err(ApplyError::Scan(ScanError::Cancelled))),
        "{cancelled:?}"
    );
    let left: Vec<_> = fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with(".foldover-tmp-"))
        .collect();
    assert_eq!(left, Vec::<String>::new());

    // A file that no longer matches the version is not copied.
    fs::write(directory.path().join("report.bin"), b"changed").unwrap();
    assert!(matches!(
        remote.keep_copy(&name, version, "Folder B", &options, &mut proceed),
        Err(ApplyError::SourceChanged(_))
    ));
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
                &mut |_| {},
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
    let identity = remote.ensure_identity(Durability::Standard).unwrap();
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
            &mut |_| {},
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

/// `Durability::Full` cannot be promised over SFTP at all: namespace
/// changes have no protocol flush. Every mutating entry point must
/// reject it before touching the server, whatever the server offers.
#[test]
fn full_durability_is_rejected_before_any_write() {
    // A capable server, offering fsync.
    let directory = tempfile::tempdir().unwrap();
    if let Some(remote) = remote(directory.path()) {
        full_durability_is_rejected(&remote, directory.path());
    }
    // One without the extension behaves identically.
    let directory = tempfile::tempdir().unwrap();
    if let Some(endpoint) = without_fsync(directory.path()) {
        full_durability_is_rejected(&Endpoint::Sftp(endpoint), directory.path());
    }
}

fn full_durability_is_rejected(remote: &Endpoint, root: &Path) {
    let policy = MetadataPolicy::default();
    let full = PairOptions::new(policy, Durability::Full);
    let run = RunScope::new(UNIX_EPOCH, full.clone());

    // The identity write refuses before `.foldover` is created.
    let error = remote.ensure_identity(Durability::Full).unwrap_err();
    assert!(
        matches!(error, ApplyError::DurabilityUnsupported(_)),
        "{error:?}"
    );
    assert!(!root.join(".foldover").exists());

    // A local file copied to the server under Full is rejected; the
    // reverse direction is fine, since a local folder can flush.
    let local_directory = tempfile::tempdir().unwrap();
    let local = Endpoint::Local(LocalEndpoint::open(local_directory.path()).unwrap());
    fs::write(local_directory.path().join("report"), vec![5; 40_000]).unwrap();
    fs::write(root.join("original"), b"keep me").unwrap();
    let observe = |endpoint: &Endpoint| {
        endpoint
            .observe_from(
                policy,
                &Filter::none(),
                &std::collections::BTreeMap::new(),
                &mut proceed,
                &mut |_| {},
            )
            .unwrap()
    };
    let here = observe(&local);
    let there = observe(remote);
    let name = path(b"report");
    let error = remote
        .copy_from(
            &local,
            &name,
            &here.entries()[&name],
            Expected::Absent,
            &run,
            &mut proceed,
        )
        .unwrap_err();
    assert!(
        matches!(error, ApplyError::DurabilityUnsupported(_)),
        "{error:?}"
    );
    let name = path(b"original");
    local
        .copy_from(
            remote,
            &name,
            &there.entries()[&name],
            Expected::Absent,
            &run,
            &mut proceed,
        )
        .unwrap();
    assert_eq!(
        fs::read(local_directory.path().join("original")).unwrap(),
        b"keep me"
    );
    fs::remove_file(local_directory.path().join("original")).unwrap();

    // Metadata-only changes, removals, leftover recovery and conflict
    // copies all mutate the server: each refuses under Full.
    let entry = &there.entries()[&name];
    assert!(matches!(
        remote.update_metadata_from(&local, &name, entry, entry, &run, &mut proceed),
        Err(ApplyError::DurabilityUnsupported(_))
    ));
    assert!(matches!(
        remote.remove(&name, entry, &run, &mut proceed),
        Err(ApplyError::DurabilityUnsupported(_))
    ));
    let version = entry.version();
    assert!(matches!(
        remote.keep_copy(&name, version, "Folder B", &full, &mut proceed),
        Err(ApplyError::DurabilityUnsupported(_))
    ));
    let leftover = path(b".foldover-tmp-deadbeef");
    fs::write(root.join(".foldover-tmp-deadbeef"), b"half").unwrap();
    assert!(matches!(
        remote.recover_leftover(&leftover, &run),
        Err(ApplyError::DurabilityUnsupported(_))
    ));

    // Nothing was published, staged or archived; the source is intact.
    let mut names: Vec<String> = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, [".foldover-tmp-deadbeef", "original"], "{names:?}");
    assert_eq!(fs::read(root.join("original")).unwrap(), b"keep me");

    // Standard writes all still work on the same server.
    let standard = PairOptions::new(policy, Durability::Standard);
    let run = RunScope::new(UNIX_EPOCH, standard.clone());
    fs::remove_file(root.join(".foldover-tmp-deadbeef")).unwrap();
    let identity = remote.ensure_identity(Durability::Standard).unwrap();
    assert_eq!(remote.identity().unwrap(), Some(identity));
    remote
        .keep_copy(&name, version, "Folder B", &standard, &mut proceed)
        .unwrap();
    remote
        .copy_from(
            &local,
            &path(b"report"),
            &here.entries()[&path(b"report")],
            Expected::Absent,
            &run,
            &mut proceed,
        )
        .unwrap();
    assert_eq!(fs::read(root.join("report")).unwrap(), vec![5; 40_000]);
    let observed = observe(remote);
    remote
        .remove(&name, &observed.entries()[&name], &run, &mut proceed)
        .unwrap();
    assert!(!root.join("original").exists());
}
