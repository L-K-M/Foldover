//! A blocking SFTP version 3 client over `ssh -s sftp`. OpenSSH handles
//! keys, the agent, `~/.ssh/config` and known hosts; this module speaks
//! the file protocol on the subprocess's standard streams.
//!
//! ```text
//! Session ──stdin──▶ ssh -s host sftp ──▶ sftp-server
//!         ◀─stdout──
//! ```

mod wire;

use std::collections::{HashSet, VecDeque};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::os::fd::OwnedFd;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};

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
/// The fallback deadline for connecting and the version handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
/// The fallback deadline for one packet exchange.
const IO_TIMEOUT: Duration = Duration::from_secs(60);
/// Replies received ahead of their request's turn, bounded by the
/// window: a server with more in flight is desynchronized.
const MAX_STASHED: usize = WINDOW;

/// Deadlines guarding every wait on the server; the defaults suit
/// interactive SSH servers on real networks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Options {
    /// Spawning, connecting and the version handshake may take this long.
    pub connect_timeout: Duration,
    /// Sending or receiving a single packet may take this long.
    pub io_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            connect_timeout: CONNECT_TIMEOUT,
            io_timeout: IO_TIMEOUT,
        }
    }
}

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

/// Where a producer pushes file data, chunk by chunk.
pub type Chunks<'a> = &'a mut dyn FnMut(&[u8]) -> io::Result<()>;

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
    child: Option<Child>,
    input: File,
    output: BufReader<File>,
    errors: Arc<Mutex<Vec<u8>>>,
    next_id: u32,
    extensions: Extensions,
    options: Options,
    /// Requests sent whose replies have not been consumed.
    inflight: HashSet<u32>,
    /// Replies that arrived ahead of their request's turn.
    stashed: VecDeque<(u32, Vec<u8>)>,
    /// The wire failed or desynchronized; further requests are refused.
    failed: bool,
}

impl Session {
    /// Connect with the user's OpenSSH configuration. Never prompts: keys
    /// and agents work, passwords do not.
    pub fn connect(target: &SshTarget) -> io::Result<Self> {
        Self::connect_with(target, Options::default())
    }

    /// Connect with explicit deadlines.
    pub fn connect_with(target: &SshTarget, options: Options) -> io::Result<Self> {
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
        // A second bound inside ssh itself, so the TCP connect cannot
        // outlive the deadline this process enforces on the handshake.
        let seconds = options
            .connect_timeout
            .as_secs()
            .clamp(1, u64::from(u32::MAX));
        command.arg("-o").arg(format!("ConnectTimeout={seconds}"));
        if let Some(port) = target.port {
            command.arg("-p").arg(port.to_string());
        }
        if let Some(user) = &target.user {
            command.arg("-l").arg(user);
        }
        command.args(["-s", "--", &target.host, "sftp"]);
        Self::start(command, options)
    }

    /// Talk to an SFTP server program directly, e.g. OpenSSH's
    /// `sftp-server`, for tests.
    pub fn spawn(program: &Path, args: &[&OsStr]) -> io::Result<Self> {
        Self::spawn_with(program, args, Options::default())
    }

    /// Spawn with explicit deadlines.
    pub fn spawn_with(program: &Path, args: &[&OsStr], options: Options) -> io::Result<Self> {
        let mut command = Command::new(program);
        command.args(args);
        Self::start(command, options)
    }

    fn start(mut command: Command, options: Options) -> io::Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let input = File::from(OwnedFd::from(child.stdin.take().expect("stdin is piped")));
        let output = File::from(OwnedFd::from(child.stdout.take().expect("stdout is piped")));
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
        Self::over(input, output, errors, options, Some(child))
    }

    /// A session on open streams; `child`, when any, is killed on drop.
    fn over(
        input: File,
        output: File,
        errors: Arc<Mutex<Vec<u8>>>,
        options: Options,
        mut child: Option<Child>,
    ) -> io::Result<Self> {
        if let Err(error) = nonblocking(&input).and_then(|()| nonblocking(&output)) {
            if let Some(child) = &mut child {
                let _ = child.kill();
                let _ = child.wait();
            }
            return Err(error);
        }
        let mut session = Self {
            child,
            input,
            output: BufReader::new(output),
            errors,
            next_id: 0,
            extensions: Extensions::default(),
            options,
            inflight: HashSet::new(),
            stashed: VecDeque::new(),
            failed: false,
        };
        let started = Instant::now();
        session
            .handshake(started, options.connect_timeout)
            .map_err(|error| session.explain(error))?;
        Ok(session)
    }

    fn handshake(&mut self, started: Instant, within: Duration) -> io::Result<()> {
        self.send_until(&Writer::init(PROTOCOL_VERSION).finish(), started, within)?;
        let body = self.receive_until(started, within)?;
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
    /// On any error the replies already owed are drained so the wire
    /// stays aligned; if that drain fails the session is marked
    /// unusable rather than left desynchronized.
    pub fn read_all(
        &mut self,
        handle: &Handle,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        self.read_from(handle, 0, sink)
    }

    /// Read from `offset` to the end, as `read_all` does from the start.
    pub fn read_from(
        &mut self,
        handle: &Handle,
        from: u64,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        self.check_alive()?;
        let mut offset = from;
        loop {
            // Request a window of chunks, then take the answers in order.
            let mut pending = VecDeque::new();
            for index in 0..WINDOW {
                let at = offset + (index * CHUNK) as u64;
                let id = self.id();
                if let Err(error) = self.send_for(
                    id,
                    Writer::new(wire::READ, id)
                        .bytes(&handle.0)
                        .u64(at)
                        .u32(CHUNK as u32)
                        .finish(),
                ) {
                    let _ = self.drain();
                    return Err(error);
                }
                pending.push_back(id);
            }
            let mut finished = false;
            let mut short = false;
            while let Some(id) = pending.pop_front() {
                let body = match self.receive_for(id) {
                    Ok(body) => body,
                    Err(error) => {
                        let _ = self.drain();
                        return Err(error);
                    }
                };
                if finished || short {
                    continue;
                }
                if is_eof(&body) {
                    finished = true;
                    continue;
                }
                let chunk = (|| -> io::Result<&[u8]> {
                    let mut reader = expect(&body, wire::DATA)?;
                    let data = reader.bytes()?;
                    if data.is_empty() {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "SFTP read made no progress",
                        ));
                    }
                    Ok(data)
                })();
                let data = match chunk {
                    Ok(data) => data,
                    Err(error) => {
                        let _ = self.drain();
                        return Err(error);
                    }
                };
                if let Err(error) = sink(data) {
                    let _ = self.drain();
                    return Err(error);
                }
                offset += data.len() as u64;
                // Later answers would leave a gap; ask again from here.
                short = data.len() < CHUNK;
            }
            if finished {
                return Ok(());
            }
        }
    }

    /// Write everything `source` yields from the start of the file.
    pub fn write_all(&mut self, handle: &Handle, source: &mut dyn Read) -> io::Result<u64> {
        self.write_chunks(handle, |sink| {
            let mut buffer = vec![0; CHUNK];
            loop {
                let count = read_full(source, &mut buffer)?;
                if count == 0 {
                    return Ok(());
                }
                sink(&buffer[..count])?;
            }
        })
    }

    /// Write the chunks `produce` pushes from the start of the file,
    /// keeping several writes in flight. Returns the bytes written.
    /// On any error the replies already owed are drained, like in
    /// `read_all`.
    pub fn write_chunks(
        &mut self,
        handle: &Handle,
        produce: impl FnOnce(Chunks<'_>) -> io::Result<()>,
    ) -> io::Result<u64> {
        self.check_alive()?;
        let mut offset = 0u64;
        let mut pending = VecDeque::new();
        let mut buffer = Vec::with_capacity(CHUNK);
        let produced = produce(&mut |mut chunk: &[u8]| {
            while !chunk.is_empty() {
                let taken = chunk.len().min(CHUNK - buffer.len());
                buffer.extend_from_slice(&chunk[..taken]);
                chunk = &chunk[taken..];
                if buffer.len() == CHUNK {
                    self.queue_write(&mut pending, handle, &buffer, &mut offset)?;
                    buffer.clear();
                }
            }
            Ok(())
        });
        let finished = produced.and_then(|()| {
            if !buffer.is_empty() {
                self.queue_write(&mut pending, handle, &buffer, &mut offset)?;
            }
            while let Some(id) = pending.pop_front() {
                status(&self.receive_for(id)?)?;
            }
            Ok(offset)
        });
        if finished.is_err() {
            let _ = self.drain();
        }
        finished
    }

    /// Send one WRITE, collecting the oldest reply once the window is
    /// full.
    fn queue_write(
        &mut self,
        pending: &mut VecDeque<u32>,
        handle: &Handle,
        data: &[u8],
        offset: &mut u64,
    ) -> io::Result<()> {
        let id = self.id();
        self.send_for(
            id,
            Writer::new(wire::WRITE, id)
                .bytes(&handle.0)
                .u64(*offset)
                .bytes(data)
                .finish(),
        )?;
        pending.push_back(id);
        *offset += data.len() as u64;
        if pending.len() >= WINDOW {
            let first = pending.pop_front().expect("a write is pending");
            status(&self.receive_for(first)?)?;
        }
        Ok(())
    }

    /// Copy `length` bytes from offset 0 of `source` into `target`, one
    /// bounded request at a time so a single session cannot deadlock
    /// against itself. `observe` sees each chunk, e.g. for hashing or a
    /// cancel check. Returns the bytes copied, always `length` on
    /// success; an early end of the source, an empty or oversized DATA
    /// answer, or an observer error aborts the copy honestly.
    pub fn copy_data(
        &mut self,
        source: &Handle,
        target: &Handle,
        length: u64,
        observe: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<u64> {
        self.check_alive()?;
        let mut copied = 0u64;
        while copied < length {
            let wanted =
                u32::try_from((length - copied).min(CHUNK as u64)).expect("a chunk fits u32");
            let body = self.request(|id| {
                Writer::new(wire::READ, id)
                    .bytes(&source.0)
                    .u64(copied)
                    .u32(wanted)
            })?;
            if is_eof(&body) {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("SFTP source ended after {copied} of {length} bytes"),
                ));
            }
            let mut reader = expect(&body, wire::DATA)?;
            let data = reader.bytes()?;
            if data.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "SFTP read made no progress",
                ));
            }
            if data.len() > wanted as usize {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "SFTP server answered more than requested",
                ));
            }
            observe(data)?;
            let answer = self.request(|id| {
                Writer::new(wire::WRITE, id)
                    .bytes(&target.0)
                    .u64(copied)
                    .bytes(data)
            })?;
            status(&answer)?;
            copied += data.len() as u64;
        }
        Ok(copied)
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
        self.send_for(id, build(id).finish())?;
        self.receive_for(id)
    }

    /// Send a request and register it as awaiting a reply.
    fn send_for(&mut self, id: u32, packet: Vec<u8>) -> io::Result<()> {
        self.check_alive()?;
        self.send(packet)?;
        self.inflight.insert(id);
        Ok(())
    }

    fn send(&mut self, packet: Vec<u8>) -> io::Result<()> {
        let started = Instant::now();
        let result = self.send_until(&packet, started, self.options.io_timeout);
        if result.is_err() {
            self.fail();
        }
        result
    }

    /// Write the whole packet within `within` of `started`, waiting on
    /// the pipe instead of blocking when it is full.
    fn send_until(&mut self, packet: &[u8], started: Instant, within: Duration) -> io::Result<()> {
        let mut sent = 0;
        while sent < packet.len() {
            match self.input.write(&packet[sent..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "The SFTP connection accepts no more data",
                    ));
                }
                Ok(count) => sent += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    wait(&self.input, PollFlags::OUT, started, within)?;
                }
                Err(error) => return Err(error),
            }
        }
        self.input.flush()
    }

    /// The answer to request `id`. Answers that arrive ahead of it are
    /// kept for their own requests, up to `MAX_STASHED`; a reply for a
    /// request nobody sent desynchronizes the session. The whole wait
    /// for `id` shares one deadline: each routed packet may arrive
    /// inside the per-packet gap while the answer never does.
    fn receive_for(&mut self, id: u32) -> io::Result<Vec<u8>> {
        if let Some(index) = self.stashed.iter().position(|(reply, _)| *reply == id) {
            let (_, body) = self.stashed.remove(index).expect("found above");
            self.inflight.remove(&id);
            return Ok(body);
        }
        let started = Instant::now();
        loop {
            let body = self.receive_within(started)?;
            let reply = self.reply_id(&body)?;
            if reply == id {
                self.inflight.remove(&id);
                return Ok(body);
            }
            let duplicate = self.stashed.iter().any(|(stashed, _)| *stashed == reply);
            if duplicate || !self.inflight.contains(&reply) || self.stashed.len() >= MAX_STASHED {
                self.fail();
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "SFTP answer out of order",
                ));
            }
            self.stashed.push_back((reply, body));
        }
    }

    /// The request id of a reply packet. A packet too short to carry
    /// one marks the session unusable.
    fn reply_id(&mut self, body: &[u8]) -> io::Result<u32> {
        let mut reader = Reader::new(body);
        match reader.u8().and_then(|_| reader.u32()) {
            Ok(id) => Ok(id),
            Err(error) => {
                self.fail();
                Err(error)
            }
        }
    }

    /// Consume the replies still owed for requests in flight, e.g. after
    /// a sink failed mid-window, so the wire is aligned again. A drain
    /// that cannot complete marks the session unusable.
    fn drain(&mut self) -> io::Result<()> {
        for (reply, _) in self.stashed.drain(..) {
            self.inflight.remove(&reply);
        }
        let started = Instant::now();
        while !self.inflight.is_empty() {
            let body = self.receive_within(started)?;
            let reply = self.reply_id(&body)?;
            if !self.inflight.remove(&reply) {
                self.fail();
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "SFTP answer out of order",
                ));
            }
        }
        Ok(())
    }

    /// Whether this session still answers requests; one that
    /// desynchronized refuses them, so a pool should not lend it out.
    pub fn alive(&self) -> bool {
        !self.failed
    }

    /// A session that once desynchronized must not be trusted again.
    fn check_alive(&self) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "The SFTP session is unusable after an earlier failure",
            ));
        }
        Ok(())
    }

    /// Refuse further requests and stop the child, so nothing keeps
    /// waiting on a desynchronized stream.
    fn fail(&mut self) {
        self.failed = true;
        if let Some(child) = &mut self.child {
            let _ = child.kill();
        }
    }

    /// The next packet within `io_timeout` of `started`, which the
    /// caller keeps across a wait spanning several packets so the wait
    /// is bounded once, not once per packet.
    fn receive_within(&mut self, started: Instant) -> io::Result<Vec<u8>> {
        let result = self.receive_until(started, self.options.io_timeout);
        if result.is_err() {
            self.fail();
        }
        result
    }

    /// The next packet's body within `within` of `started`, covering a
    /// stalled or partial frame as well as a silent one.
    fn receive_until(&mut self, started: Instant, within: Duration) -> io::Result<Vec<u8>> {
        let mut length = [0; 4];
        self.read_exact(&mut length, started, within)
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
        self.read_exact(&mut body, started, within)?;
        Ok(body)
    }

    /// Fill `buffer` within `within` of `started`, waiting on the pipe
    /// instead of blocking when it is empty.
    fn read_exact(
        &mut self,
        buffer: &mut [u8],
        started: Instant,
        within: Duration,
    ) -> io::Result<()> {
        let mut filled = 0;
        while filled < buffer.len() {
            match self.output.read(&mut buffer[filled..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "The SFTP connection closed",
                    ));
                }
                Ok(count) => filled += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    wait(self.output.get_ref(), PollFlags::IN, started, within)?;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
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

/// A failure status the server answered with. The answer is
/// authoritative: the request did not apply. Transport failures may
/// have applied unseen, so callers that track remote state treat the
/// two differently (`error.get_ref()` downcasts to this).
#[derive(Debug)]
pub struct Refused {
    /// The server's SSH_FXP status code.
    pub code: u32,
    /// The server's explanation.
    pub message: String,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "SFTP: {}", self.message)
    }
}

impl std::error::Error for Refused {}

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
    Err(io::Error::new(kind, Refused { code, message }))
}

fn unsupported(extension: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("The SFTP server lacks {extension}"),
    )
}

/// Reads and writes report `WouldBlock` instead of hanging, so `wait`
/// can bound them.
fn nonblocking(file: &File) -> io::Result<()> {
    let flags = fcntl_getfl(file)?;
    fcntl_setfl(file, flags | OFlags::NONBLOCK).map_err(io::Error::from)
}

/// Wait until `file` is ready or `within` has elapsed since `started`.
fn wait(file: &File, events: PollFlags, started: Instant, within: Duration) -> io::Result<()> {
    loop {
        let remaining = within.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Timed out waiting for the SFTP server",
            ));
        }
        let limit = Timespec::try_from(remaining)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid SFTP timeout"))?;
        let mut polled = [PollFd::new(file, events)];
        match poll(&mut polled, Some(&limit)) {
            Ok(0) => {}
            Ok(_) => return Ok(()),
            Err(rustix::io::Errno::INTR) => {}
            Err(error) => return Err(error.into()),
        }
    }
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
