//! A blocking SFTP version 3 client over `ssh -s sftp`. OpenSSH handles
//! keys, the agent, `~/.ssh/config` and known hosts; this module speaks
//! the file protocol on the subprocess's standard streams.
//!
//! ```text
//! Session ──stdin──▶ ssh -s host sftp ──▶ sftp-server
//!         ◀─stdout──
//! ```

mod wire;

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::io::{self, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

pub use wire::{Attributes, FileKind};
use wire::{Reader, Writer};

const SSH: &str = "/usr/bin/ssh";
const PROTOCOL_VERSION: u32 = 3;
/// Bytes per read or write request; every server accepts 32 KiB.
const CHUNK: usize = 32 * 1024;
/// Requests in flight while streaming, so throughput does not wait on
/// every round trip.
const WINDOW: usize = 16;
/// The largest response accepted from a broken or hostile server.
const MAX_PACKET: usize = 1024 * 1024;
/// How much of ssh's error output is kept for messages.
const STDERR_LIMIT: usize = 4 * 1024;

const POSIX_RENAME: &[u8] = b"posix-rename@openssh.com";
const STATVFS: &[u8] = b"statvfs@openssh.com";
const FSYNC: &[u8] = b"fsync@openssh.com";

/// Where to connect: a host from `~/.ssh/config` or DNS, with an optional
/// user and port.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SshTarget {
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
}

/// An open file or directory on the server.
#[derive(Debug)]
pub struct Handle(Vec<u8>);

/// What the server supports beyond version 3.
#[derive(Clone, Copy, Debug, Default)]
struct Extensions {
    posix_rename: bool,
    statvfs: bool,
    fsync: bool,
}

pub struct Session {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    errors: Arc<Mutex<Vec<u8>>>,
    next_id: u32,
    extensions: Extensions,
}

impl Session {
    /// Connect with the user's OpenSSH configuration. Never prompts: keys
    /// and agents work, passwords do not.
    pub fn connect(target: &SshTarget) -> io::Result<Self> {
        let valid = |text: &str| {
            !text.is_empty() && !text.starts_with('-') && !text.contains(char::is_whitespace)
        };
        if !valid(&target.host) || target.user.as_deref().is_some_and(|user| !valid(user)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid SSH host or user",
            ));
        }
        let mut command = Command::new(SSH);
        command.args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=4",
        ]);
        if let Some(port) = target.port {
            command.arg("-p").arg(port.to_string());
        }
        if let Some(user) = &target.user {
            command.arg("-l").arg(user);
        }
        command.args(["-s", "--", &target.host, "sftp"]);
        Self::start(command)
    }

    /// Talk to an SFTP server program directly, e.g. OpenSSH's
    /// `sftp-server`, for tests.
    pub fn spawn(program: &Path, args: &[&OsStr]) -> io::Result<Self> {
        let mut command = Command::new(program);
        command.args(args);
        Self::start(command)
    }

    fn start(mut command: Command) -> io::Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let input = child.stdin.take().expect("stdin is piped");
        let output = BufReader::new(child.stdout.take().expect("stdout is piped"));
        let mut stderr = child.stderr.take().expect("stderr is piped");
        let errors = Arc::new(Mutex::new(Vec::new()));
        let collected = Arc::clone(&errors);
        thread::spawn(move || {
            let mut buffer = [0; 1024];
            while let Ok(count @ 1..) = stderr.read(&mut buffer) {
                let mut errors = collected.lock().unwrap_or_else(PoisonError::into_inner);
                errors.extend_from_slice(&buffer[..count]);
                let excess = errors.len().saturating_sub(STDERR_LIMIT);
                errors.drain(..excess);
            }
        });
        let mut session = Self {
            child,
            input,
            output,
            errors,
            next_id: 0,
            extensions: Extensions::default(),
        };
        session
            .handshake()
            .map_err(|error| session.explain(error))?;
        Ok(session)
    }

    fn handshake(&mut self) -> io::Result<()> {
        self.send(Writer::init(PROTOCOL_VERSION).finish())?;
        let body = self.receive()?;
        let mut reader = Reader::new(&body);
        if reader.u8()? != wire::VERSION || reader.u32()? != PROTOCOL_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "The server does not speak SFTP version 3",
            ));
        }
        while !reader.is_empty() {
            let name = reader.bytes()?;
            reader.bytes()?;
            match name {
                POSIX_RENAME => self.extensions.posix_rename = true,
                STATVFS => self.extensions.statvfs = true,
                FSYNC => self.extensions.fsync = true,
                _ => {}
            }
        }
        Ok(())
    }

    /// Add what ssh printed, e.g. "Permission denied (publickey)", to a
    /// connection failure.
    fn explain(&self, error: io::Error) -> io::Error {
        let errors = self.errors.lock().unwrap_or_else(PoisonError::into_inner);
        let printed = String::from_utf8_lossy(&errors);
        let printed = printed.trim();
        if printed.is_empty() {
            return error;
        }
        io::Error::new(error.kind(), format!("{error}: {printed}"))
    }

    pub fn lstat(&mut self, path: &[u8]) -> io::Result<Attributes> {
        let body = self.request(|id| Writer::new(wire::LSTAT, id).bytes(path))?;
        let mut reader = expect(&body, wire::ATTRS)?;
        reader.attributes()
    }

    /// Every entry of a directory except `.` and `..`.
    pub fn read_dir(&mut self, path: &[u8]) -> io::Result<Vec<(Vec<u8>, Attributes)>> {
        let handle = self.handle(|id| Writer::new(wire::OPENDIR, id).bytes(path))?;
        let mut entries = Vec::new();
        let result = loop {
            let body = match self.request(|id| Writer::new(wire::READDIR, id).bytes(&handle.0)) {
                Ok(body) => body,
                Err(error) => break Err(error),
            };
            if is_eof(&body) {
                break Ok(());
            }
            let mut reader = match expect(&body, wire::NAME) {
                Ok(reader) => reader,
                Err(error) => break Err(error),
            };
            let parsed = (|| -> io::Result<()> {
                for _ in 0..reader.u32()? {
                    let name = reader.bytes()?.to_vec();
                    reader.bytes()?;
                    let attributes = reader.attributes()?;
                    if name != b"." && name != b".." {
                        entries.push((name, attributes));
                    }
                }
                Ok(())
            })();
            if let Err(error) = parsed {
                break Err(error);
            }
        };
        let closed = self.close(handle);
        result?;
        closed?;
        Ok(entries)
    }

    pub fn open_read(&mut self, path: &[u8]) -> io::Result<Handle> {
        self.handle(|id| {
            Writer::new(wire::OPEN, id)
                .bytes(path)
                .u32(wire::OPEN_READ)
                .attributes(&Attributes::default())
        })
    }

    /// Create a file that must not exist yet, with `permissions`.
    pub fn create_new(&mut self, path: &[u8], permissions: u32) -> io::Result<Handle> {
        let attributes = Attributes {
            permissions: Some(permissions),
            ..Attributes::default()
        };
        self.handle(|id| {
            Writer::new(wire::OPEN, id)
                .bytes(path)
                .u32(wire::OPEN_WRITE | wire::OPEN_CREATE | wire::OPEN_EXCLUSIVE)
                .attributes(&attributes)
        })
    }

    /// Stream a whole file to `sink`, keeping several reads in flight.
    pub fn read_all(
        &mut self,
        handle: &Handle,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut offset = 0u64;
        loop {
            // Request a window of chunks, then take the answers in order.
            let mut pending = VecDeque::new();
            for index in 0..WINDOW {
                let at = offset + (index * CHUNK) as u64;
                let id = self.id();
                self.send(
                    Writer::new(wire::READ, id)
                        .bytes(&handle.0)
                        .u64(at)
                        .u32(CHUNK as u32)
                        .finish(),
                )?;
                pending.push_back(id);
            }
            let mut finished = false;
            let mut short = false;
            while let Some(id) = pending.pop_front() {
                let body = self.receive_for(id)?;
                if finished || short {
                    continue;
                }
                if is_eof(&body) {
                    finished = true;
                    continue;
                }
                let mut reader = expect(&body, wire::DATA)?;
                let data = reader.bytes()?;
                sink(data)?;
                offset += data.len() as u64;
                // Later answers would leave a gap; ask again from here.
                short = data.len() < CHUNK;
            }
            if finished {
                return Ok(());
            }
        }
    }

    /// Write everything `source` yields from the start of the file,
    /// keeping several writes in flight.
    pub fn write_all(&mut self, handle: &Handle, source: &mut dyn Read) -> io::Result<u64> {
        let mut offset = 0u64;
        let mut pending = VecDeque::new();
        let mut buffer = vec![0; CHUNK];
        loop {
            let count = read_full(source, &mut buffer)?;
            if count > 0 {
                let id = self.id();
                self.send(
                    Writer::new(wire::WRITE, id)
                        .bytes(&handle.0)
                        .u64(offset)
                        .bytes(&buffer[..count])
                        .finish(),
                )?;
                pending.push_back(id);
                offset += count as u64;
            }
            if pending.len() >= WINDOW || (count == 0 && !pending.is_empty()) {
                let id = pending.pop_front().expect("a write is pending");
                status(&self.receive_for(id)?)?;
            }
            if count == 0 && pending.is_empty() {
                return Ok(offset);
            }
        }
    }

    pub fn set_handle_attributes(
        &mut self,
        handle: &Handle,
        attributes: &Attributes,
    ) -> io::Result<()> {
        let body = self.request(|id| {
            Writer::new(wire::FSETSTAT, id)
                .bytes(&handle.0)
                .attributes(attributes)
        })?;
        status(&body)
    }

    pub fn set_attributes(&mut self, path: &[u8], attributes: &Attributes) -> io::Result<()> {
        let body = self.request(|id| {
            Writer::new(wire::SETSTAT, id)
                .bytes(path)
                .attributes(attributes)
        })?;
        status(&body)
    }

    /// Flush the file to the server's disk; unsupported servers say so.
    pub fn sync(&mut self, handle: &Handle) -> io::Result<()> {
        if !self.extensions.fsync {
            return Err(unsupported("fsync"));
        }
        let body = self.request(|id| {
            Writer::new(wire::EXTENDED, id)
                .bytes(FSYNC)
                .bytes(&handle.0)
        })?;
        status(&body)
    }

    pub fn close(&mut self, handle: Handle) -> io::Result<()> {
        let body = self.request(|id| Writer::new(wire::CLOSE, id).bytes(&handle.0))?;
        status(&body)
    }

    pub fn make_directory(&mut self, path: &[u8], permissions: u32) -> io::Result<()> {
        let attributes = Attributes {
            permissions: Some(permissions),
            ..Attributes::default()
        };
        let body = self.request(|id| {
            Writer::new(wire::MKDIR, id)
                .bytes(path)
                .attributes(&attributes)
        })?;
        status(&body)
    }

    pub fn remove_directory(&mut self, path: &[u8]) -> io::Result<()> {
        let body = self.request(|id| Writer::new(wire::RMDIR, id).bytes(path))?;
        status(&body)
    }

    pub fn remove_file(&mut self, path: &[u8]) -> io::Result<()> {
        let body = self.request(|id| Writer::new(wire::REMOVE, id).bytes(path))?;
        status(&body)
    }

    /// Rename without replacing: version 3 fails when `to` exists.
    pub fn rename(&mut self, from: &[u8], to: &[u8]) -> io::Result<()> {
        let body = self.request(|id| Writer::new(wire::RENAME, id).bytes(from).bytes(to))?;
        status(&body)
    }

    /// Rename over an existing entry atomically, where the server can.
    pub fn rename_replacing(&mut self, from: &[u8], to: &[u8]) -> io::Result<()> {
        if !self.extensions.posix_rename {
            return Err(unsupported("posix-rename"));
        }
        let body = self.request(|id| {
            Writer::new(wire::EXTENDED, id)
                .bytes(POSIX_RENAME)
                .bytes(from)
                .bytes(to)
        })?;
        status(&body)
    }

    pub fn read_link(&mut self, path: &[u8]) -> io::Result<Vec<u8>> {
        let body = self.request(|id| Writer::new(wire::READLINK, id).bytes(path))?;
        let mut reader = expect(&body, wire::NAME)?;
        if reader.u32()? != 1 {
            return Err(wire::malformed());
        }
        Ok(reader.bytes()?.to_vec())
    }

    /// Create `link` pointing at `target`. OpenSSH takes the two in the
    /// reverse of the draft's order; other servers are not supported.
    pub fn symlink(&mut self, target: &[u8], link: &[u8]) -> io::Result<()> {
        let body = self.request(|id| Writer::new(wire::SYMLINK, id).bytes(target).bytes(link))?;
        status(&body)
    }

    /// The server's absolute form of `path`, e.g. for `.`.
    pub fn real_path(&mut self, path: &[u8]) -> io::Result<Vec<u8>> {
        let body = self.request(|id| Writer::new(wire::REALPATH, id).bytes(path))?;
        let mut reader = expect(&body, wire::NAME)?;
        if reader.u32()? != 1 {
            return Err(wire::malformed());
        }
        Ok(reader.bytes()?.to_vec())
    }

    /// Bytes an unprivileged writer can still use below `path`.
    pub fn available_space(&mut self, path: &[u8]) -> io::Result<u64> {
        if !self.extensions.statvfs {
            return Err(unsupported("statvfs"));
        }
        let body = self.request(|id| Writer::new(wire::EXTENDED, id).bytes(STATVFS).bytes(path))?;
        let mut reader = expect(&body, wire::EXTENDED_REPLY)?;
        let _block_size = reader.u64()?;
        let fragment_size = reader.u64()?;
        let _blocks = reader.u64()?;
        let _free = reader.u64()?;
        let available = reader.u64()?;
        Ok(available.saturating_mul(fragment_size))
    }

    fn handle(&mut self, build: impl FnOnce(u32) -> Writer) -> io::Result<Handle> {
        let body = self.request(build)?;
        let mut reader = expect(&body, wire::HANDLE)?;
        Ok(Handle(reader.bytes()?.to_vec()))
    }

    fn id(&mut self) -> u32 {
        self.next_id = self.next_id.wrapping_add(1);
        self.next_id
    }

    /// One request and its answer's body.
    fn request(&mut self, build: impl FnOnce(u32) -> Writer) -> io::Result<Vec<u8>> {
        let id = self.id();
        self.send(build(id).finish())?;
        self.receive_for(id)
    }

    fn send(&mut self, packet: Vec<u8>) -> io::Result<()> {
        self.input.write_all(&packet)?;
        self.input.flush()
    }

    /// The next answer, which must belong to request `id`. Answers come
    /// in request order from OpenSSH; others are refused.
    fn receive_for(&mut self, id: u32) -> io::Result<Vec<u8>> {
        let body = self.receive()?;
        let mut reader = Reader::new(&body);
        reader.u8()?;
        if reader.u32()? != id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SFTP answer out of order",
            ));
        }
        Ok(body)
    }

    fn receive(&mut self) -> io::Result<Vec<u8>> {
        let mut length = [0; 4];
        self.output
            .read_exact(&mut length)
            .map_err(|error| match error.kind() {
                io::ErrorKind::UnexpectedEof => self.explain(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "The SFTP connection closed",
                )),
                _ => error,
            })?;
        let length = u32::from_be_bytes(length) as usize;
        if length == 0 || length > MAX_PACKET {
            return Err(wire::malformed());
        }
        let mut body = vec![0; length];
        self.output.read_exact(&mut body)?;
        Ok(body)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The body after its type and id, if it has the expected type; a status
/// answer becomes the error it reports.
fn expect(body: &[u8], kind: u8) -> io::Result<Reader<'_>> {
    let mut reader = Reader::new(body);
    let found = reader.u8()?;
    reader.u32()?;
    if found == kind {
        return Ok(reader);
    }
    if found == wire::STATUS {
        status(body)?;
    }
    Err(wire::malformed())
}

fn is_eof(body: &[u8]) -> bool {
    let mut reader = Reader::new(body);
    matches!(reader.u8(), Ok(wire::STATUS))
        && reader.u32().is_ok()
        && matches!(reader.u32(), Ok(wire::STATUS_EOF))
}

/// A status answer as a result.
fn status(body: &[u8]) -> io::Result<()> {
    let mut reader = Reader::new(body);
    if reader.u8()? != wire::STATUS {
        return Err(wire::malformed());
    }
    reader.u32()?;
    let code = reader.u32()?;
    let message = reader
        .bytes()
        .map(|text| String::from_utf8_lossy(text).into_owned())
        .unwrap_or_default();
    let kind = match code {
        wire::STATUS_OK => return Ok(()),
        wire::STATUS_NO_SUCH_FILE => io::ErrorKind::NotFound,
        wire::STATUS_PERMISSION_DENIED => io::ErrorKind::PermissionDenied,
        wire::STATUS_OP_UNSUPPORTED => io::ErrorKind::Unsupported,
        wire::STATUS_EOF => io::ErrorKind::UnexpectedEof,
        _ => io::ErrorKind::Other,
    };
    Err(io::Error::new(kind, format!("SFTP: {message}")))
}

fn unsupported(extension: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("The SFTP server lacks {extension}"),
    )
}

/// Fill `buffer` unless the source ends first.
fn read_full(source: &mut dyn Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match source.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests;
