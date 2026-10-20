//! Against OpenSSH's `sftp-server` on standard streams: no SSH or keys
//! needed. Skipped where the server is not installed. Scripted protocol
//! tests run against a fake server on a socketpair instead.

use std::ffi::OsStr;
use std::fs;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

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

    // A resumed copy reads only the rest.
    let handle = session.open_read(b"big").unwrap();
    let from = data.len() as u64 / 3 + 7;
    let mut rest = Vec::new();
    session
        .read_from(&handle, from, &mut |chunk| {
            rest.extend_from_slice(chunk);
            Ok(())
        })
        .unwrap();
    session.close(handle).unwrap();
    assert!(rest == data[from as usize..]);

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

/// Short deadlines keep stall tests quick; healthy replies arrive in
/// milliseconds.
fn quick() -> Options {
    Options {
        connect_timeout: Duration::from_secs(2),
        io_timeout: Duration::from_secs(2),
    }
}

/// Deadlines for tests where nothing should arrive at all.
fn snappy() -> Options {
    Options {
        connect_timeout: Duration::from_millis(500),
        io_timeout: Duration::from_millis(500),
    }
}

/// A scripted SFTP server on the far end of a socketpair. The stream is
/// blocking; scripts end quietly by returning when `request` sees EOF.
struct Fake {
    stream: UnixStream,
}

impl Fake {
    /// The next request as (kind, id, fields after the id), or `None`
    /// once the session is gone.
    fn request(&mut self) -> Option<(u8, u32, Vec<u8>)> {
        let mut length = [0; 4];
        self.stream.read_exact(&mut length).ok()?;
        let length = u32::from_be_bytes(length) as usize;
        if length > MAX_PACKET {
            return None;
        }
        let mut body = vec![0; length];
        self.stream.read_exact(&mut body).ok()?;
        let mut reader = Reader::new(&body);
        let kind = reader.u8().ok()?;
        let id = reader.u32().ok()?;
        Some((kind, id, body[5..].to_vec()))
    }

    fn send(&mut self, packet: Vec<u8>) {
        let _ = self.stream.write_all(&packet);
    }

    /// The version handshake every session opens with.
    fn handshake(&mut self) {
        let (kind, version, _) = self.request().expect("init packet");
        assert_eq!(kind, wire::INIT);
        assert_eq!(version, PROTOCOL_VERSION);
        self.send(Writer::new(wire::VERSION, PROTOCOL_VERSION).finish());
    }

    fn status(&mut self, id: u32, code: u32) {
        self.send(
            Writer::new(wire::STATUS, id)
                .u32(code)
                .bytes(b"scripted")
                .bytes(b"en")
                .finish(),
        );
    }

    fn data(&mut self, id: u32, data: &[u8]) {
        self.send(Writer::new(wire::DATA, id).bytes(data).finish());
    }

    fn attrs(&mut self, id: u32) {
        self.send(
            Writer::new(wire::ATTRS, id)
                .attributes(&Attributes::default())
                .finish(),
        );
    }

    /// Answer requests until the session drops.
    fn serve(&mut self, mut answer: impl FnMut(&mut Self, u8, u32, &[u8])) {
        while let Some((kind, id, body)) = self.request() {
            answer(self, kind, id, &body);
        }
    }
}

fn file(stream: UnixStream) -> File {
    File::from(OwnedFd::from(stream))
}

/// A session on a socketpair to a scripted server thread.
fn fixture(options: Options, script: impl FnOnce(Fake) + Send + 'static) -> Session {
    let (client, server) = UnixStream::pair().unwrap();
    thread::spawn(move || script(Fake { stream: server }));
    let errors = Arc::new(Mutex::new(Vec::new()));
    let input = client.try_clone().unwrap();
    Session::over(file(input), file(client), errors, options, None).unwrap()
}

/// A failed sink must not leave the window's pending answers to be
/// mistaken for the next request's reply.
#[test]
fn read_failure_keeps_wire_aligned() {
    let mut session = fixture(quick(), move |mut server| {
        server.handshake();
        let mut ids = Vec::new();
        for _ in 0..WINDOW {
            let (kind, id, _) = server.request().expect("read request");
            assert_eq!(kind, wire::READ);
            ids.push(id);
        }
        for (index, id) in ids.iter().enumerate() {
            server.data(*id, &vec![index as u8; CHUNK]);
        }
        server.serve(|server, kind, id, _| {
            if kind == wire::LSTAT {
                server.attrs(id);
            }
        });
    });
    let handle = Handle(vec![1]);
    let mut fed = 0;
    let error = session
        .read_all(&handle, &mut |chunk| {
            fed += chunk.len();
            if fed > CHUNK {
                return Err(io::Error::other("sink full"));
            }
            Ok(())
        })
        .unwrap_err();
    assert_eq!(error.to_string(), "sink full");
    session.lstat(b"later").unwrap();
}

/// A failed producer must drain the writes already in flight.
#[test]
fn write_failure_keeps_wire_aligned() {
    let mut session = fixture(quick(), move |mut server| {
        server.handshake();
        server.serve(|server, kind, id, _| match kind {
            wire::WRITE => server.status(id, wire::STATUS_OK),
            wire::LSTAT => server.attrs(id),
            _ => {}
        });
    });
    let handle = Handle(vec![1]);
    let error = session
        .write_chunks(&handle, |sink| {
            for _ in 0..3 {
                sink(&vec![7u8; CHUNK])?;
            }
            Err(io::Error::other("producer broke"))
        })
        .unwrap_err();
    assert_eq!(error.to_string(), "producer broke");
    session.lstat(b"later").unwrap();
}

/// A refused write leaves the rest of the window unanswered; the
/// session must still stay aligned.
#[test]
fn failed_write_keeps_wire_aligned() {
    let mut session = fixture(quick(), move |mut server| {
        server.handshake();
        server.serve(|server, kind, id, _| match kind {
            wire::WRITE => server.status(id, wire::STATUS_PERMISSION_DENIED),
            wire::LSTAT => server.attrs(id),
            _ => {}
        });
    });
    let handle = Handle(vec![1]);
    let error = session
        .write_chunks(&handle, |sink| {
            for _ in 0..WINDOW + 5 {
                sink(&vec![9u8; CHUNK])?;
            }
            Ok(())
        })
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    session.lstat(b"later").unwrap();
}

/// Replies may arrive out of request order; each must still be matched
/// to its request.
#[test]
fn reordered_replies_still_match_their_requests() {
    let content: Vec<u8> = (0..WINDOW * CHUNK).map(|index| index as u8).collect();
    let scripted = content.clone();
    let mut session = fixture(quick(), move |mut server| {
        server.handshake();
        let mut ids = Vec::new();
        for _ in 0..WINDOW {
            let (kind, id, _) = server.request().expect("read request");
            assert_eq!(kind, wire::READ);
            ids.push(id);
        }
        // Answer the window in reverse: the first request is answered last.
        for (index, id) in ids.iter().enumerate().rev() {
            let chunk = &scripted[index * CHUNK..(index + 1) * CHUNK];
            server.data(*id, chunk);
        }
        server.serve(|server, kind, id, _| {
            if kind == wire::READ {
                server.status(id, wire::STATUS_EOF);
            }
        });
    });
    let handle = Handle(vec![1]);
    let mut read = Vec::new();
    session
        .read_all(&handle, &mut |chunk| {
            read.extend_from_slice(chunk);
            Ok(())
        })
        .unwrap();
    assert_eq!(read, content);
}

/// A server that answers reads with empty DATA packets forever must be
/// stopped, not looped on.
#[test]
fn empty_data_is_not_progress() {
    let mut session = fixture(quick(), move |mut server| {
        server.handshake();
        server.serve(|server, kind, id, _| match kind {
            wire::READ => server.data(id, b""),
            wire::LSTAT => server.attrs(id),
            _ => {}
        });
    });
    let handle = Handle(vec![1]);
    let error = session.read_all(&handle, &mut |_| Ok(())).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    session.lstat(b"later").unwrap();
}

/// Replies for other in-flight requests may arrive slowly, each inside
/// the packet deadline; the wait for the requested answer still shares
/// one budget instead of resetting it per routed packet.
#[test]
fn slowly_reordered_replies_share_one_deadline() {
    let options = Options {
        connect_timeout: Duration::from_secs(2),
        io_timeout: Duration::from_millis(600),
    };
    let mut session = fixture(options, move |mut server| {
        server.handshake();
        let mut ids = Vec::new();
        for _ in 0..WINDOW {
            let (kind, id, _) = server.request().expect("read request");
            assert_eq!(kind, wire::READ);
            ids.push(id);
        }
        // Each gap is under the packet deadline, but together they
        // exceed the wait for the first request's answer.
        for id in &ids[1..4] {
            thread::sleep(Duration::from_millis(450));
            server.data(*id, &vec![1u8; CHUNK]);
        }
        for index in [0usize].into_iter().chain(4..WINDOW) {
            server.data(ids[index], &vec![index as u8; CHUNK]);
        }
        server.serve(|server, kind, id, _| {
            if kind == wire::READ {
                server.status(id, wire::STATUS_EOF);
            }
        });
    });
    let handle = Handle(vec![1]);
    let error = session.read_all(&handle, &mut |_| Ok(())).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(
        session.lstat(b"later").unwrap_err().kind(),
        io::ErrorKind::ConnectionAborted
    );
}

/// Draining replies owed after an error shares one deadline too: a
/// server trickling them under the per-packet gap must not stretch the
/// cleanup to a whole window of timeouts. The operation's own error is
/// still the one reported; the session fails closed.
#[test]
fn a_slow_pending_drain_is_bounded() {
    let options = Options {
        connect_timeout: Duration::from_secs(2),
        io_timeout: Duration::from_millis(600),
    };
    let mut session = fixture(options, move |mut server| {
        server.handshake();
        let mut ids = Vec::new();
        for _ in 0..WINDOW {
            let (kind, id, _) = server.request().expect("read request");
            assert_eq!(kind, wire::READ);
            ids.push(id);
        }
        // Most of the window at once, then the last few each just
        // inside the packet deadline.
        for id in &ids[..WINDOW - 3] {
            server.data(*id, &vec![1u8; CHUNK]);
        }
        for id in &ids[WINDOW - 3..] {
            thread::sleep(Duration::from_millis(450));
            server.data(*id, &vec![1u8; CHUNK]);
        }
        server.serve(|server, kind, id, _| {
            if kind == wire::LSTAT {
                server.attrs(id);
            }
        });
    });
    let handle = Handle(vec![1]);
    let error = session
        .read_all(&handle, &mut |_| Err(io::Error::other("sink full")))
        .unwrap_err();
    assert_eq!(error.to_string(), "sink full");
    assert_eq!(
        session.lstat(b"later").unwrap_err().kind(),
        io::ErrorKind::ConnectionAborted
    );
}

/// A reply for a request nobody sent means the wire is desynchronized;
/// the session must fail closed instead of trusting later answers.
#[test]
fn an_answer_for_no_request_fails_closed() {
    let mut session = fixture(quick(), move |mut server| {
        server.handshake();
        let Some((_, id, _)) = server.request() else {
            return;
        };
        server.attrs(id.wrapping_add(1000));
        while server.request().is_some() {}
    });
    assert_eq!(
        session.lstat(b"x").unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        session.lstat(b"x").unwrap_err().kind(),
        io::ErrorKind::ConnectionAborted
    );
}

/// A server that never answers must fail within the io deadline, not
/// block the calling agent forever.
#[test]
fn silent_server_times_out_instead_of_blocking() {
    let mut session = fixture(snappy(), |mut server| {
        server.handshake();
        while server.request().is_some() {}
    });
    assert_eq!(
        session.lstat(b"x").unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(
        session.lstat(b"x").unwrap_err().kind(),
        io::ErrorKind::ConnectionAborted
    );
}

/// Half a frame then silence is still a stall.
#[test]
fn a_stalled_partial_frame_times_out() {
    let mut session = fixture(snappy(), |mut server| {
        server.handshake();
        if server.request().is_none() {
            return;
        }
        let _ = server.stream.write_all(&[0, 0]);
        while server.request().is_some() {}
    });
    assert_eq!(
        session.lstat(b"x").unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
}

/// A peer that accepts the socket but never speaks must fail the
/// handshake within the connect deadline.
#[test]
fn handshake_deadline_bounds_connect() {
    let (client, mut server) = UnixStream::pair().unwrap();
    thread::spawn(move || {
        let mut buffer = [0; 1024];
        while server.read(&mut buffer).unwrap_or(0) > 0 {}
    });
    let errors = Arc::new(Mutex::new(Vec::new()));
    let input = client.try_clone().unwrap();
    let error = Session::over(file(input), file(client), errors, snappy(), None)
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
}

/// A real copy: each chunk is read, observed and written before the
/// next request, so one session cannot deadlock against itself.
#[test]
fn copy_data_moves_exact_bytes_through_one_session() {
    let content: Vec<u8> = (0..CHUNK * 2 + 123).map(|index| index as u8).collect();
    let written = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&written);
    let scripted = content.clone();
    let length = content.len() as u64;
    let mut session = fixture(quick(), move |mut server| {
        server.handshake();
        server.serve(|server, kind, id, body| match kind {
            wire::READ => {
                let mut fields = Reader::new(body);
                fields.bytes().unwrap();
                let offset = fields.u64().unwrap();
                let wanted = fields.u32().unwrap() as usize;
                let offset = offset as usize;
                if offset >= scripted.len() {
                    server.status(id, wire::STATUS_EOF);
                } else {
                    let end = (offset + wanted).min(scripted.len());
                    server.data(id, &scripted[offset..end]);
                }
            }
            wire::WRITE => {
                let mut fields = Reader::new(body);
                fields.bytes().unwrap();
                let offset = fields.u64().unwrap() as usize;
                let data = fields.bytes().unwrap();
                let mut written = kept.lock().unwrap();
                let needed = offset + data.len();
                if written.len() < needed {
                    written.resize(needed, 0);
                }
                written[offset..needed].copy_from_slice(data);
                server.status(id, wire::STATUS_OK);
            }
            wire::LSTAT => server.attrs(id),
            _ => {}
        });
    });
    let source = Handle(vec![1]);
    let target = Handle(vec![2]);
    let mut observed = Vec::new();
    let copied = session
        .copy_data(&source, &target, length, &mut |chunk| {
            observed.extend_from_slice(chunk);
            Ok(())
        })
        .unwrap();
    assert_eq!(copied, length);
    assert_eq!(observed, content);
    assert_eq!(*written.lock().unwrap(), content);
    session.lstat(b"later").unwrap();
}

/// An early end of the source, a zero-length answer or an oversized
/// answer each abort the copy instead of looping or padding.
#[test]
fn copy_data_reports_broken_answers() {
    // The source ends before the promised length.
    let mut session = fixture(quick(), |mut server| {
        server.handshake();
        server.serve(|server, kind, id, _| {
            if kind == wire::READ {
                server.status(id, wire::STATUS_EOF);
            }
        });
    });
    let source = Handle(vec![1]);
    let target = Handle(vec![2]);
    let error = session
        .copy_data(&source, &target, 10, &mut |_| Ok(()))
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);

    // An empty DATA answer is not progress.
    let mut session = fixture(quick(), |mut server| {
        server.handshake();
        server.serve(|server, kind, id, _| {
            if kind == wire::READ {
                server.data(id, b"");
            }
        });
    });
    let error = session
        .copy_data(&source, &target, 10, &mut |_| Ok(()))
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    // More data than requested is a protocol violation.
    let mut session = fixture(quick(), |mut server| {
        server.handshake();
        server.serve(|server, kind, id, _| {
            if kind == wire::READ {
                server.data(id, &vec![0; CHUNK + 1]);
            }
        });
    });
    let error = session
        .copy_data(&source, &target, 10, &mut |_| Ok(()))
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    // A failing observer aborts the copy but keeps the wire aligned.
    let mut session = fixture(quick(), |mut server| {
        server.handshake();
        server.serve(|server, kind, id, body| match kind {
            wire::READ => {
                let mut fields = Reader::new(body);
                fields.bytes().unwrap();
                fields.u64().unwrap();
                let wanted = fields.u32().unwrap() as usize;
                server.data(id, &vec![3u8; wanted]);
            }
            wire::LSTAT => server.attrs(id),
            _ => {}
        });
    });
    let error = session
        .copy_data(&source, &target, 10, &mut |_| {
            Err(io::Error::other("observer cancelled"))
        })
        .unwrap_err();
    assert_eq!(error.to_string(), "observer cancelled");
    session.lstat(b"later").unwrap();
}

/// The real `sftp-server`, when present, copies through one session.
#[test]
fn copy_data_against_a_real_server() {
    let directory = tempfile::tempdir().unwrap();
    let Some(mut session) = session(directory.path()) else {
        return;
    };
    let data: Vec<u8> = (0..CHUNK * 3 + 77).map(|index| index as u8).collect();
    fs::write(directory.path().join("from"), &data).unwrap();
    let source = session.open_read(b"from").unwrap();
    let target = session.create_new(b"to", 0o600).unwrap();
    let mut observed = Vec::new();
    let copied = session
        .copy_data(&source, &target, data.len() as u64, &mut |chunk| {
            observed.extend_from_slice(chunk);
            Ok(())
        })
        .unwrap();
    assert_eq!(copied, data.len() as u64);
    assert_eq!(observed, data);
    session.close(source).unwrap();
    session.close(target).unwrap();
    assert_eq!(fs::read(directory.path().join("to")).unwrap(), data);
}
