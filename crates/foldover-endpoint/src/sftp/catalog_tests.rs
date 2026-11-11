//! Regression tests for the private SFTP catalog primitives and for
//! `CatalogFiles` over an SFTP root. A scripted SFTP v3 server, spawned from
//! Python over a real directory, stands in for OpenSSH so the failure paths
//! (refused rename, lost response, disconnect) are deterministic.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use foldover_types::ScanControl;

use super::*;
use crate::{CatalogFiles, Endpoint, HeldLease, LeaseHolder, LeaseTiming};

/// A scripted SFTP v3 server over the directory `ROOT`. `CONTROL` is re-read
/// before every request:
///
/// - `ok` (the default): normal operation;
/// - `fail-rename`: every RENAME is refused with `SSH_FX_FAILURE`;
/// - `hangup-on-rename`: the connection closes when a RENAME arrives;
/// - `hangup`: the connection closes when the first MKDIR arrives.
const FAKE_SERVER: &str = r##"
#!/usr/bin/env python3
import os
import stat
import struct
import sys

ROOT = sys.argv[1]
CONTROL = sys.argv[2] if len(sys.argv) > 2 else None


def read_exact(stream, count):
    data = bytearray()
    while len(data) < count:
        part = stream.read(count - len(data))
        if not part:
            raise EOFError
        data.extend(part)
    return bytes(data)


def read_packet():
    (length,) = struct.unpack(">I", read_exact(sys.stdin.buffer, 4))
    return read_exact(sys.stdin.buffer, length)


def string(data):
    return struct.pack(">I", len(data)) + data


def write_packet(body):
    sys.stdout.buffer.write(struct.pack(">I", len(body)) + body)
    sys.stdout.buffer.flush()


def reply(kind, ident, payload=b""):
    write_packet(bytes([kind]) + struct.pack(">I", ident) + payload)


def status(ident, code, message=""):
    reply(101, ident, struct.pack(">I", code) + string(message.encode()) + string(b"en"))


def attributes(st):
    mode = st.st_mode
    if stat.S_ISDIR(mode):
        bits = 0o040000
    elif stat.S_ISLNK(mode):
        bits = 0o120000
    elif stat.S_ISREG(mode):
        bits = 0o100000
    else:
        bits = 0o060000
    flags = 1 | 4 | 8
    return (struct.pack(">I", flags)
            + struct.pack(">Q", st.st_size)
            + struct.pack(">I", bits | (mode & 0o7777))
            + struct.pack(">II", int(st.st_atime), int(st.st_mtime)))


def send_attrs(ident, st):
    reply(105, ident, attributes(st))


def send_name(ident, entries):
    payload = struct.pack(">I", len(entries))
    for name, st in entries:
        payload += string(name) + string(b"-") + attributes(st)
    reply(104, ident, payload)


def take_string(data):
    (length,) = struct.unpack(">I", data[:4])
    return data[4:4 + length], data[4 + length:]


def scripted():
    if not CONTROL:
        return "ok"
    try:
        with open(CONTROL, "r") as handle:
            return handle.read().strip()
    except OSError:
        return "ok"


handles = {}
directories = {}
counter = [0]


def new_handle(value):
    counter[0] += 1
    ident = str(counter[0]).encode()
    handles[ident] = value
    return ident


def main():
    try:
        first = read_packet()
    except EOFError:
        return
    if not first or first[0] != 1:
        return
    write_packet(bytes([2]) + struct.pack(">I", 3)
                 + string(b"posix-rename@openssh.com") + string(b""))

    while True:
        try:
            body = read_packet()
        except EOFError:
            return
        if len(body) < 5:
            return
        kind = body[0]
        (ident,) = struct.unpack(">I", body[1:5])
        rest = body[5:]
        mode = scripted()

        try:
            if kind == 7:  # LSTAT
                path, _ = take_string(rest)
                try:
                    st = os.lstat(path)
                except FileNotFoundError:
                    status(ident, 2, "no such file")
                except NotADirectoryError:
                    status(ident, 4, "not a directory")
                else:
                    send_attrs(ident, st)

            elif kind == 14:  # MKDIR
                if mode == "hangup":
                    return
                path, _ = take_string(rest)
                try:
                    os.mkdir(path)
                except FileExistsError:
                    status(ident, 4, "exists")
                except NotADirectoryError:
                    status(ident, 4, "not a directory")
                except FileNotFoundError:
                    status(ident, 2, "no such file")
                else:
                    status(ident, 0)

            elif kind == 3:  # OPEN
                path, rest = take_string(rest)
                (flags,) = struct.unpack(">I", rest[:4])
                exclusive = bool(flags & 0x20)
                create = bool(flags & 0x08)
                write = bool(flags & 0x02)
                if exclusive and os.path.lexists(path):
                    status(ident, 4, "exists")
                    continue
                try:
                    if create:
                        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                    elif write:
                        fd = os.open(path, os.O_WRONLY)
                    else:
                        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
                except FileNotFoundError:
                    status(ident, 2, "no such file")
                except IsADirectoryError:
                    status(ident, 4, "is a directory")
                except PermissionError:
                    status(ident, 3, "denied")
                except OSError:
                    status(ident, 4, "cannot open")
                else:
                    reply(102, ident, string(new_handle(fd)))

            elif kind == 5:  # READ
                handle, rest = take_string(rest)
                (offset,) = struct.unpack(">Q", rest[:8])
                (length,) = struct.unpack(">I", rest[8:12])
                fd = handles.get(handle)
                if fd is None:
                    status(ident, 4, "bad handle")
                    continue
                try:
                    data = os.pread(fd, length, offset)
                except OSError:
                    status(ident, 4, "read failed")
                    continue
                if not data:
                    status(ident, 1, "eof")
                else:
                    reply(103, ident, string(data))

            elif kind == 6:  # WRITE
                handle, rest = take_string(rest)
                (offset,) = struct.unpack(">Q", rest[:8])
                data, _ = take_string(rest[8:])
                fd = handles.get(handle)
                if fd is None:
                    status(ident, 4, "bad handle")
                    continue
                try:
                    os.pwrite(fd, data, offset)
                except OSError:
                    status(ident, 4, "write failed")
                else:
                    status(ident, 0)

            elif kind == 4:  # CLOSE
                handle, _ = take_string(rest)
                fd = handles.pop(handle, None)
                directories.pop(handle, None)
                if fd is not None:
                    try:
                        os.close(fd)
                    except OSError:
                        pass
                status(ident, 0)

            elif kind == 18:  # RENAME
                if mode == "hangup-on-rename":
                    return
                if mode == "fail-rename":
                    status(ident, 4, "scripted rename failure")
                    continue
                source, rest = take_string(rest)
                target, _ = take_string(rest)
                try:
                    if os.path.lexists(target):
                        status(ident, 4, "exists")
                    else:
                        os.rename(source, target)
                        status(ident, 0)
                except FileNotFoundError:
                    status(ident, 2, "no such file")
                except OSError:
                    status(ident, 4, "rename failed")

            elif kind == 13:  # REMOVE
                path, _ = take_string(rest)
                try:
                    os.unlink(path)
                except FileNotFoundError:
                    status(ident, 2, "no such file")
                except IsADirectoryError:
                    status(ident, 4, "is a directory")
                except OSError:
                    status(ident, 4, "remove failed")
                else:
                    status(ident, 0)

            elif kind == 11:  # OPENDIR
                path, _ = take_string(rest)
                try:
                    entries = sorted(os.listdir(path))
                except FileNotFoundError:
                    status(ident, 2, "no such file")
                except NotADirectoryError:
                    status(ident, 4, "not a directory")
                except OSError:
                    status(ident, 4, "cannot list")
                else:
                    handle = new_handle(None)
                    directories[handle] = (path, entries, 0)
                    reply(102, ident, string(handle))

            elif kind == 12:  # READDIR
                handle, _ = take_string(rest)
                entry = directories.get(handle)
                if entry is None:
                    status(ident, 4, "bad handle")
                    continue
                path, entries, index = entry
                if index >= len(entries):
                    status(ident, 1, "eof")
                    continue
                batch = []
                for name in entries[index:index + 64]:
                    try:
                        st = os.lstat(os.path.join(path, name))
                    except OSError:
                        continue
                    batch.append((os.fsencode(name), st))
                directories[handle] = (path, entries, index + 64)
                send_name(ident, batch)

            elif kind == 16:  # REALPATH
                path, _ = take_string(rest)
                try:
                    st = os.lstat(path)
                except OSError:
                    st = os.lstat(ROOT)
                send_name(ident, [(os.path.realpath(path).encode(), st)])

            elif kind == 19:  # READLINK
                path, _ = take_string(rest)
                try:
                    target = os.readlink(path)
                    st = os.lstat(path)
                except OSError:
                    status(ident, 4, "not a link")
                else:
                    send_name(ident, [(target.encode(), st)])

            elif kind == 20:  # SYMLINK
                target, rest = take_string(rest)
                link, _ = take_string(rest)
                try:
                    os.symlink(target, link)
                except OSError:
                    status(ident, 4, "symlink failed")
                else:
                    status(ident, 0)

            elif kind == 15:  # RMDIR
                path, _ = take_string(rest)
                try:
                    os.rmdir(path)
                except FileNotFoundError:
                    status(ident, 2, "no such file")
                except OSError:
                    status(ident, 4, "rmdir failed")
                else:
                    status(ident, 0)

            elif kind in (9, 10):  # SETSTAT, FSETSTAT
                status(ident, 0)

            elif kind == 200:  # EXTENDED
                name, rest = take_string(rest)
                if name == b"posix-rename@openssh.com":
                    source, rest = take_string(rest)
                    target, _ = take_string(rest)
                    try:
                        os.replace(source, target)
                    except FileNotFoundError:
                        status(ident, 2, "no such file")
                    except OSError:
                        status(ident, 4, "rename failed")
                    else:
                        status(ident, 0)
                elif name == b"fsync@openssh.com":
                    status(ident, 0)
                else:
                    status(ident, 8, "unsupported")

            else:
                status(ident, 8, "unsupported")
        except (BrokenPipeError, EOFError):
            return


main()
"##;

fn pair() -> PairId {
    crate::test_pair()
}

fn state_directory(root: &Path) -> PathBuf {
    root.join(".foldover/state").join(format!("{}", pair()))
}

/// Spawn a fake SFTP server rooted at `root`, scripting `control`.
fn endpoint(root: &Path, control: &Path) -> SftpEndpoint {
    let script = root.join("fake-sftp.py");
    fs::write(&script, FAKE_SERVER).unwrap();
    SftpEndpoint::spawn(
        Path::new("python3"),
        &[script.as_os_str(), root.as_os_str(), control.as_os_str()],
        root.as_os_str().as_encoded_bytes(),
    )
    .unwrap()
}

fn catalog(root: &Path, control: &Path) -> SftpEndpoint {
    endpoint(root, control)
        .catalog_endpoint(pair(), Durability::Standard)
        .unwrap()
}

#[test]
fn sftp_catalog_roundtrip_read_list_remove() {
    let root = tempfile::tempdir().unwrap();
    let control = root.path().join("control");
    fs::write(&control, "ok").unwrap();
    let catalog = catalog(root.path(), &control);

    assert_eq!(catalog.catalog_read("missing", 64).unwrap(), None);
    catalog
        .catalog_write("alpha", b"one", Durability::Standard)
        .unwrap();
    catalog
        .catalog_write("beta", b"two", Durability::Standard)
        .unwrap();
    assert_eq!(
        catalog.catalog_read("alpha", 64).unwrap().as_deref(),
        Some(b"one".as_slice())
    );
    assert_eq!(
        catalog.catalog_read("beta", 64).unwrap().as_deref(),
        Some(b"two".as_slice())
    );

    let mut names = catalog.catalog_list().unwrap();
    names.sort();
    assert_eq!(names, ["alpha", "beta", "format"]);

    catalog.catalog_remove("alpha").unwrap();
    assert_eq!(catalog.catalog_read("alpha", 64).unwrap(), None);
    // Removing what is already gone is not an error.
    catalog.catalog_remove("alpha").unwrap();
    let mut names = catalog.catalog_list().unwrap();
    names.sort();
    assert_eq!(names, ["beta", "format"]);
}

#[test]
fn sftp_catalog_create_is_exclusive_and_never_overwrites() {
    let root = tempfile::tempdir().unwrap();
    let control = root.path().join("control");
    fs::write(&control, "ok").unwrap();
    let catalog = catalog(root.path(), &control);
    catalog
        .catalog_write("alpha", b"one", Durability::Standard)
        .unwrap();

    let error = catalog
        .catalog_write("alpha", b"two", Durability::Standard)
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(
        catalog.catalog_read("alpha", 64).unwrap().as_deref(),
        Some(b"one".as_slice())
    );
}

#[test]
fn sftp_catalog_read_is_bounded_and_rejects_invalid_names() {
    let root = tempfile::tempdir().unwrap();
    let control = root.path().join("control");
    fs::write(&control, "ok").unwrap();
    let catalog = catalog(root.path(), &control);
    let bytes = vec![b'x'; 32];
    catalog
        .catalog_write("big", &bytes, Durability::Standard)
        .unwrap();
    assert_eq!(
        catalog.catalog_read("big", 64).unwrap().as_deref(),
        Some(bytes.as_slice())
    );
    let error = catalog.catalog_read("big", 8).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    let long = "x".repeat(129);
    for name in [
        "",
        ".hidden",
        "UPPER",
        "a/b",
        "a\\b",
        "space name",
        long.as_str(),
    ] {
        assert!(
            catalog
                .catalog_write(name, b"x", Durability::Standard)
                .is_err(),
            "write accepted {name:?}"
        );
        assert!(
            catalog.catalog_read(name, 64).is_err(),
            "read accepted {name:?}"
        );
        assert!(
            catalog.catalog_remove(name).is_err(),
            "remove accepted {name:?}"
        );
    }
}

#[test]
fn sftp_catalog_marker_mismatch_refuses_directory() {
    let root = tempfile::tempdir().unwrap();
    let control = root.path().join("control");
    fs::write(&control, "ok").unwrap();
    let state = state_directory(root.path());
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("format"), b"not the marker").unwrap();
    let error = endpoint(root.path(), &control)
        .catalog_endpoint(pair(), Durability::Standard)
        .map(|_| ())
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn sftp_catalog_refuses_symlink_and_directory_injection() {
    let root = tempfile::tempdir().unwrap();
    let control = root.path().join("control");
    fs::write(&control, "ok").unwrap();
    let state = state_directory(root.path());
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("format"), DIRECTORY_MARKER).unwrap();
    let outside = root.path().join("outside");
    fs::write(&outside, b"secret").unwrap();
    std::os::unix::fs::symlink(&outside, state.join("link")).unwrap();
    fs::create_dir(state.join("subdir")).unwrap();
    let catalog = endpoint(root.path(), &control)
        .catalog_endpoint(pair(), Durability::Standard)
        .unwrap();

    assert!(catalog.catalog_read("link", 64).is_err());
    assert!(catalog.catalog_read("subdir", 64).is_err());
    assert_eq!(
        catalog.catalog_remove("link").unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        catalog.catalog_remove("subdir").unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        catalog
            .catalog_write("link", b"x", Durability::Standard)
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(
        catalog
            .catalog_write("subdir", b"x", Durability::Standard)
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    let mut names = catalog.catalog_list().unwrap();
    names.sort();
    assert_eq!(names, ["format"]);
}

#[test]
fn sftp_catalog_refuses_full_durability() {
    let root = tempfile::tempdir().unwrap();
    let control = root.path().join("control");
    fs::write(&control, "ok").unwrap();
    let endpoint = endpoint(root.path(), &control);
    let error = endpoint
        .catalog_endpoint(pair(), Durability::Full)
        .map(|_| ())
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);

    let catalog = endpoint
        .catalog_endpoint(pair(), Durability::Standard)
        .unwrap();
    let error = catalog
        .catalog_write("alpha", b"one", Durability::Full)
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
}

#[test]
fn sftp_catalog_failed_publication_leaves_no_final_file() {
    let root = tempfile::tempdir().unwrap();
    let control = root.path().join("control");
    fs::write(&control, "ok").unwrap();
    let catalog = catalog(root.path(), &control);
    fs::write(&control, "fail-rename").unwrap();

    let error = catalog
        .catalog_write("alpha", b"one", Durability::Standard)
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Other);

    // The final name was never exposed, and the marker is untouched.
    assert_eq!(catalog.catalog_read("alpha", 64).unwrap(), None);
    let names = catalog.catalog_list().unwrap();
    assert!(!names.iter().any(|name| name == "alpha"));
    assert_eq!(
        catalog.catalog_read("format", 64).unwrap().as_deref(),
        Some(DIRECTORY_MARKER)
    );
}

#[test]
fn sftp_catalog_lost_response_leaves_no_final_file() {
    let root = tempfile::tempdir().unwrap();
    let control = root.path().join("control");
    fs::write(&control, "ok").unwrap();
    let catalog = catalog(root.path(), &control);
    fs::write(&control, "hangup-on-rename").unwrap();

    let error = catalog
        .catalog_write("beta", b"two", Durability::Standard)
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);

    // The next call opens a fresh session; the final name was never exposed.
    fs::write(&control, "ok").unwrap();
    assert_eq!(catalog.catalog_read("beta", 64).unwrap(), None);
    let names = catalog.catalog_list().unwrap();
    assert!(!names.iter().any(|name| name == "beta"));
}

#[test]
fn sftp_catalog_disconnect_during_open_is_an_error() {
    let root = tempfile::tempdir().unwrap();
    let control = root.path().join("control");
    fs::write(&control, "hangup").unwrap();
    let error = endpoint(root.path(), &control)
        .catalog_endpoint(pair(), Durability::Standard)
        .map(|_| ())
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
}

#[test]
fn catalog_files_roundtrip_and_lease_lifetime_over_sftp() {
    let root = tempfile::tempdir().unwrap();
    let control = root.path().join("control");
    fs::write(&control, "ok").unwrap();
    let parent = Arc::new(Endpoint::Sftp(endpoint(root.path(), &control)));
    let holder = LeaseHolder {
        id: "catalog-tests".into(),
        label: "Catalog tests".into(),
    };
    let mut control_fn = || ScanControl::Continue;
    assert_eq!(parent.identity().unwrap(), None);

    let files = CatalogFiles::open(
        Arc::clone(&parent),
        pair(),
        holder,
        Durability::Standard,
        &mut control_fn,
    )
    .unwrap();
    assert_eq!(files.identity(), parent.identity().unwrap().unwrap());

    // The metadata lease lives under the catalog directory, not the content
    // root's `.foldover/lock`.
    let catalog_lock = state_directory(root.path()).join(".foldover/lock");
    assert!(catalog_lock.exists());
    assert!(!root.path().join(".foldover/lock").exists());

    files.write_new("alpha", b"one").unwrap();
    files.write_new("beta", b"two").unwrap();
    assert_eq!(
        files.read("alpha", 64).unwrap().as_deref(),
        Some(b"one".as_slice())
    );
    assert_eq!(files.read("missing", 64).unwrap(), None);
    let mut names = files.list().unwrap();
    names.sort();
    assert_eq!(names, ["alpha", "beta", "format"]);

    // The content root's own lease is a different file: one run can hold it
    // while the catalog lease is alive.
    let content = HeldLease::take(
        &parent,
        LeaseHolder {
            id: "content-tests".into(),
            label: "Content tests".into(),
        },
        LeaseTiming::STANDARD,
        &mut control_fn,
        &mut |_| {},
    )
    .unwrap();
    assert!(root.path().join(".foldover/lock").exists());
    assert_eq!(
        files.read("beta", 64).unwrap().as_deref(),
        Some(b"two".as_slice())
    );
    files.fence().unwrap();
    drop(content);
    assert!(!root.path().join(".foldover/lock").exists());

    files.remove("alpha").unwrap();
    assert_eq!(files.read("alpha", 64).unwrap(), None);

    drop(files);
    assert!(
        !catalog_lock.exists(),
        "the catalog lease is released on drop"
    );
}
