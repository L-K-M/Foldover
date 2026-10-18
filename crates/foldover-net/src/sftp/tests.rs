//! Against OpenSSH's `sftp-server` on standard streams: no SSH or keys
//! needed. Skipped where the server is not installed.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;

use super::*;

/// `FOLDOVER_SFTP_SERVER`, or where macOS and Linux distributions put it.
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

fn session(directory: &Path) -> Option<Session> {
    let Some(server) = server() else {
        eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
        return None;
    };
    Some(Session::spawn(&server, &[OsStr::new("-d"), directory.as_os_str()]).unwrap())
}

#[test]
fn files_stream_both_ways_and_renames_never_replace_by_default() {
    let directory = tempfile::tempdir().unwrap();
    let Some(mut session) = session(directory.path()) else {
        return;
    };
    // Larger than a window of chunks, and not a multiple of one.
    let data: Vec<u8> = (0..(WINDOW * CHUNK * 2 + 1234))
        .map(|index| index as u8)
        .collect();
    fs::write(directory.path().join("big"), &data).unwrap();
    fs::create_dir(directory.path().join("dir")).unwrap();
    symlink("big", directory.path().join("link")).unwrap();

    let mut names: Vec<_> = session
        .read_dir(b".")
        .unwrap()
        .into_iter()
        .map(|(name, attributes)| (name, attributes.kind()))
        .collect();
    names.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        names,
        [
            (b"big".to_vec(), FileKind::File),
            (b"dir".to_vec(), FileKind::Directory),
            (b"link".to_vec(), FileKind::Symlink),
        ]
    );
    assert_eq!(session.lstat(b"big").unwrap().size, Some(data.len() as u64));
    assert_eq!(session.read_link(b"link").unwrap(), b"big");

    let handle = session.open_read(b"big").unwrap();
    let mut read = Vec::new();
    session
        .read_all(&handle, &mut |chunk| {
            read.extend_from_slice(chunk);
            Ok(())
        })
        .unwrap();
    session.close(handle).unwrap();
    assert!(read == data);

    let handle = session.create_new(b"copy", 0o600).unwrap();
    assert_eq!(
        session.write_all(&handle, &mut data.as_slice()).unwrap(),
        data.len() as u64
    );
    let modified = 1_700_000_000;
    session
        .set_handle_attributes(
            &handle,
            &Attributes {
                times: Some((modified, modified)),
                ..Attributes::default()
            },
        )
        .unwrap();
    let synced = session.sync(&handle);
    assert!(synced.is_ok() || synced.unwrap_err().kind() == io::ErrorKind::Unsupported);
    session.close(handle).unwrap();
    assert!(fs::read(directory.path().join("copy")).unwrap() == data);
    let copied = session.lstat(b"copy").unwrap();
    assert_eq!(copied.modified(), Some(modified));
    assert_eq!(copied.permissions.map(|mode| mode & 0o777), Some(0o600));
    assert!(session.create_new(b"copy", 0o600).is_err());

    assert!(session.rename(b"copy", b"big").is_err());
    session.rename_replacing(b"copy", b"big").unwrap();
    assert!(fs::read(directory.path().join("big")).unwrap() == data);

    session.make_directory(b"dir/new", 0o700).unwrap();
    session.symlink(b"../big", b"dir/new/up").unwrap();
    assert_eq!(
        fs::read_link(directory.path().join("dir/new/up")).unwrap(),
        PathBuf::from("../big")
    );
    assert!(session.remove_directory(b"dir/new").is_err());
    session.remove_file(b"dir/new/up").unwrap();
    session.remove_directory(b"dir/new").unwrap();
    assert_eq!(
        session.lstat(b"missing").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert!(session.available_space(b".").unwrap() > 0);
    assert!(session.real_path(b".").unwrap().starts_with(b"/"));

    fs::set_permissions(
        directory.path().join("dir"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    let denied = session.read_dir(b"dir");
    fs::set_permissions(
        directory.path().join("dir"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    if fs::read_dir(directory.path().join("dir")).is_ok() && denied.is_ok() {
        return; // Superusers read anything.
    }
    assert_eq!(denied.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
}

#[test]
fn connection_failures_say_why() {
    let error = Session::connect(&SshTarget {
        host: "-oProxyCommand=evil".into(),
        user: None,
        port: None,
    })
    .err()
    .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}
