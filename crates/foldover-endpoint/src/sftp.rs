//! A folder on an SFTP server, with the same rules as a local root:
//! writes are staged under a temporary name, nothing is replaced or
//! removed without moving it to `.foldover/archive`, and versions come
//! from the data itself.
//!
//! What differs from local roots:
//! - metadata is the mode and the modification time in whole seconds;
//!   there are no extended attributes, flags or ACLs;
//! - a replacement is two renames (current version to the archive, new
//!   version into place), so the path is briefly empty;
//! - an unchanged stat is size, time, mode and owner, without an inode;
//! - `Durability::Full` is refused, not downgraded: namespace changes
//!   have no protocol flush.

mod sessions;
mod spelling;
mod transfer;

pub(crate) use transfer::{copy, update_metadata};

use sessions::{Borrowed, Connect, Sessions};

use std::io;
use std::num::NonZeroUsize;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use foldover_net::sftp::{Attributes, FileKind, Handle, Session, SshTarget};
use foldover_types::{
    Digest, Durability, EndpointId, EntryKind, EntryVersion, MetadataPolicy, ScanControl, SftpRoot,
    SyncPath,
};

use crate::lease::{LEASE_LIMIT, LockFile};
use crate::local::Archiving;
use crate::local::archive::{ARCHIVE_ROOT, archive_time, date_name, sibling_name};
use crate::local::filter::{IGNORE_FILE, IGNORE_FILE_LIMIT, parse_ignore_file};
use crate::local::version::{Observed, Payload, entry_version, metadata_version};
use crate::local::{Fingerprint, TEMPORARY_PREFIX, child_path, protected, temporary};
use crate::{
    ApplyError, ArchivedItem, ArchivedRun, Filter, ListedEntry, ListedKind, NodeKind, Observation,
    ObserveStep, ObservedEntry, RunScope, ScanError,
};

/// Foldover's own folders and files.
const PRIVATE_DIRECTORY: u32 = 0o700;
const PRIVATE_FILE: u32 = 0o600;
/// Folders and files in the synced tree when modes are not synced; the
/// server's umask narrows them.
const DEFAULT_DIRECTORY: u32 = 0o777;
const DEFAULT_FILE: u32 = 0o666;
const PERMISSION_BITS: u32 = 0o777;
const EXECUTE_BITS: u32 = 0o111;
const IDENTITY_FILE: &[u8] = b".foldover/id";
const IDENTITY_FILE_LIMIT: usize = 64;
const INTERNAL_DIRECTORY: &[u8] = b".foldover";
/// Names tried when putting an item back beside a newer one.
const PUT_BACK_ATTEMPTS: u32 = 100;
const RESTORED: &str = "restored";
const SSH_PORT: u16 = 22;

/// Copies a server takes at once when Foldover can open more sessions
/// to it (PLAN.md §4.9).
const PARALLEL_COPIES: NonZeroUsize = NonZeroUsize::new(4).expect("four is not zero");
/// Sessions per server: one per copy worker, and the run's own thread.
const SESSIONS: usize = PARALLEL_COPIES.get() + 1;

pub struct SftpEndpoint {
    sessions: Sessions,
    /// Leases beat over their own session, opened on first use: a transfer
    /// holds a session while it calls back into the run, and a beat must
    /// neither wait for the transfer nor deadlock with it.
    lease: Mutex<Option<Box<Session>>>,
    /// How to open more sessions; without one, as for a session made
    /// elsewhere, everything shares that one.
    connect: Option<Connect>,
    /// `host:port`, to tell whether two roots can overlap; unknown for
    /// sessions made elsewhere.
    server: Option<String>,
    /// The root's absolute path on the server, without a trailing slash
    /// unless it is `/`.
    root: Vec<u8>,
}

impl SftpEndpoint {
    pub fn open(location: &SftpRoot) -> Result<Self, ScanError> {
        let target = SshTarget {
            host: location.host().to_owned(),
            user: location.user().map(str::to_owned),
            port: location.port(),
        };
        let session = Session::connect(&target)
            .map_err(|source| ScanError::Unreachable(source.to_string()))?;
        let mut endpoint = Self::with_session(session, location.path())?;
        endpoint.connect = Some(Box::new(move || Session::connect(&target)));
        endpoint.server = Some(format!(
            "{}:{}",
            location.host(),
            location.port().unwrap_or(SSH_PORT)
        ));
        Ok(endpoint)
    }

    /// How two roots on servers nest; unknown servers never overlap.
    /// Hostnames fold case like DNS; roots still compare as raw bytes,
    /// since the server's filesystem may be case-sensitive.
    pub fn relation_to(&self, other: &Self) -> crate::RootRelation {
        let same_server = match (&self.server, &other.server) {
            (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
            _ => false,
        };
        if !same_server {
            return crate::RootRelation::Disjoint;
        }
        let below = |root: &[u8], other: &[u8]| {
            root.strip_prefix(other)
                .is_some_and(|rest| rest.starts_with(b"/") || other == b"/")
        };
        if self.root == other.root {
            crate::RootRelation::Same
        } else if below(&other.root, &self.root) {
            crate::RootRelation::Contains
        } else if below(&self.root, &other.root) {
            crate::RootRelation::Within
        } else {
            crate::RootRelation::Disjoint
        }
    }

    /// Talk to an SFTP server program on standard streams instead of over
    /// SSH, e.g. OpenSSH's `sftp-server` in tests.
    pub fn spawn(
        program: &std::path::Path,
        arguments: &[&std::ffi::OsStr],
        path: &[u8],
    ) -> Result<Self, ScanError> {
        let session = Session::spawn(program, arguments).map_err(io_error)?;
        let mut endpoint = Self::with_session(session, path)?;
        let (program, arguments) = (
            program.to_owned(),
            arguments
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect::<Vec<_>>(),
        );
        endpoint.connect = Some(Box::new(move || {
            let arguments: Vec<&std::ffi::OsStr> = arguments
                .iter()
                .map(std::ffi::OsString::as_os_str)
                .collect();
            Session::spawn(&program, &arguments)
        }));
        Ok(endpoint)
    }

    /// Use an existing session, e.g. to a local `sftp-server` in tests.
    /// `path` is absolute, or `~` and `~/…` below the login's home.
    pub fn with_session(mut session: Session, path: &[u8]) -> Result<Self, ScanError> {
        let mut root = match path.strip_prefix(b"~") {
            Some(below) => [session.real_path(b".").map_err(io_error)?, below.to_vec()].concat(),
            None => path.to_vec(),
        };
        while root.len() > 1 && root.ends_with(b"/") {
            root.pop();
        }
        match session.lstat(&root) {
            Ok(attributes) if attributes.kind() == FileKind::Directory => {}
            Ok(_) => {
                return Err(io_error(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "The SFTP path is not a folder",
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ScanError::RootMissing);
            }
            Err(error) => return Err(io_error(error)),
        }
        Ok(Self {
            sessions: Sessions::new(session, SESSIONS),
            lease: Mutex::new(None),
            connect: None,
            server: None,
            root,
        })
    }

    /// A session to use alone until it drops. Hold one at a time: a
    /// handle opened on it is valid only there.
    fn session(&self) -> Borrowed<'_> {
        self.sessions.borrow(self.connect.as_ref())
    }

    /// How many copies the server takes at once: one without a way to
    /// open more sessions.
    pub fn workers(&self) -> NonZeroUsize {
        match self.connect {
            Some(_) => PARALLEL_COPIES,
            None => NonZeroUsize::MIN,
        }
    }

    /// Run `request` over the lease session, opening it first if needed;
    /// a failed request drops it, so the next one reconnects.
    fn on_lease_session<T>(
        &self,
        request: impl FnOnce(&mut Session) -> io::Result<T>,
    ) -> io::Result<T> {
        let Some(connect) = &self.connect else {
            return request(&mut self.session());
        };
        let mut lease = self.lease.lock().unwrap_or_else(PoisonError::into_inner);
        if lease.is_none() {
            *lease = Some(Box::new(connect()?));
        }
        let result = request(lease.as_mut().expect("opened above"));
        if result.is_err() {
            *lease = None;
        }
        result
    }

    /// The root's absolute path on the server, `~` resolved.
    pub fn path(&self) -> &[u8] {
        &self.root
    }

    /// The server path of `path` below the root.
    fn remote(&self, path: &[u8]) -> Vec<u8> {
        if self.root == b"/" {
            return [b"/".as_slice(), path].concat();
        }
        [self.root.as_slice(), b"/", path].concat()
    }

    /// Read the root's listing: permissions refuse listings, not opening.
    pub fn probe_access(&self) -> Result<(), ScanError> {
        self.session()
            .read_dir(&self.root)
            .map(|_| ())
            .map_err(io_error)
    }

    /// One directory's entries for a quick change check; `None` once the
    /// directory is gone.
    pub fn list(
        &self,
        directory: Option<&SyncPath>,
        filter: &Filter,
    ) -> Result<Option<Vec<ListedEntry>>, ScanError> {
        let entries = match directory {
            None => self.session().read_dir(&self.root),
            Some(path) => self.on_stored(path.as_bytes(), |remote| self.session().read_dir(remote)),
        };
        let entries = match entries {
            Ok(entries) => entries,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(source) => return Err(at(directory, source)),
        };
        let listed = entries
            .into_iter()
            .filter(|(name, _)| !protected(name))
            .filter_map(|(name, attributes)| {
                let kind = match attributes.kind() {
                    FileKind::Directory => ListedKind::Directory,
                    _ => ListedKind::Other,
                };
                ListedEntry::kept(
                    directory,
                    filter,
                    name,
                    kind,
                    attributes.size.unwrap_or(0),
                    attributes
                        .modified()
                        .map(|seconds| UNIX_EPOCH + Duration::from_secs(u64::from(seconds))),
                )
            })
            .collect();
        Ok(Some(listed))
    }

    /// The identity this root carries, or `None` before its first sync.
    pub fn identity(&self) -> Result<Option<EndpointId>, ScanError> {
        let Some(content) = self.read_small(IDENTITY_FILE, IDENTITY_FILE_LIMIT)? else {
            return Ok(None);
        };
        let text = content.strip_suffix(b"\n").unwrap_or(&content);
        EndpointId::from_hex(text)
            .map(Some)
            .ok_or(ScanError::InvalidIdentity)
    }

    /// Whether this folder can hold Foldover's records, as a Backup
    /// target without them need not (PLAN.md §3.6). A folder with an
    /// identity holds them; otherwise a probe file is uploaded to
    /// `.foldover` and removed, with `.foldover` itself if it was new.
    pub fn probe_records(&self) -> Result<(), ApplyError> {
        if self.identity()?.is_some() {
            return Ok(());
        }
        let internal = SyncPath::try_from(INTERNAL_DIRECTORY).expect("the internal path is valid");
        let existed = self.attributes(&internal)?.is_some();
        self.make_directories(INTERNAL_DIRECTORY, PRIVATE_DIRECTORY)
            .map_err(|source| write_error(&internal, source))?;
        let mut probe = INTERNAL_DIRECTORY.to_vec();
        probe.extend_from_slice(format!("/probe-{}", uuid::Uuid::new_v4().simple()).as_bytes());
        let uploaded = self.upload(&probe, PRIVATE_FILE, Durability::Standard, |sink| sink(b""));
        let mut session = self.session();
        if let Ok(temporary) = &uploaded {
            let _ = session.remove_file(temporary);
        }
        if !existed {
            let _ = session.remove_directory(&self.remote(INTERNAL_DIRECTORY));
        }
        uploaded
            .map(|_| ())
            .map_err(|source| write_error(&internal, source))
    }

    /// The root's identity, writing a new one if it has none.
    pub fn ensure_identity(&self, durability: Durability) -> Result<EndpointId, ApplyError> {
        let path = SyncPath::try_from(IDENTITY_FILE).expect("the identity path is valid");
        check_durability(durability, &path)?;
        if let Some(identity) = self.identity()? {
            return Ok(identity);
        }
        let identity = EndpointId::from_bytes(*uuid::Uuid::new_v4().as_bytes());
        self.make_directories(INTERNAL_DIRECTORY, PRIVATE_DIRECTORY)
            .map_err(|source| write_error(&path, source))?;
        let content = format!("{identity}\n");
        let temporary = self
            .upload(IDENTITY_FILE, PRIVATE_FILE, durability, |sink| {
                sink(content.as_bytes())
            })
            .map_err(|source| write_error(&path, source))?;
        let placed = self
            .session()
            .rename(&temporary, &self.remote(IDENTITY_FILE));
        if placed.is_err() {
            let _ = self.session().remove_file(&temporary);
            // Another writer was first; its identity stands.
            return self.identity()?.ok_or(ApplyError::DestinationChanged(path));
        }
        Ok(identity)
    }

    pub(crate) fn read_lock_file(&self, file: LockFile) -> Result<Option<Vec<u8>>, ScanError> {
        let remote = self.remote(file.path());
        self.on_lease_session(|session| read_small_on(session, &remote, LEASE_LIMIT))
            .map_err(io_error)
    }

    /// Write `file` unless it exists; whether it was written. A plain SFTP
    /// rename refuses an existing target, which makes the create exclusive.
    pub(crate) fn create_lock_file(
        &self,
        file: LockFile,
        content: &[u8],
    ) -> Result<bool, ApplyError> {
        let remote = self.remote(file.path());
        self.on_lease_session(|session| {
            let temporary = self.stage_lock(session, file, content)?;
            let Err(error) = session.rename(&temporary, &remote) else {
                return Ok(true);
            };
            let _ = session.remove_file(&temporary);
            match read_small_on(session, &remote, LEASE_LIMIT)? {
                Some(_) => Ok(false),
                None => Err(error),
            }
        })
        .map_err(|source| write_error(&lock_path(file), source))
    }

    pub(crate) fn replace_lock_file(
        &self,
        file: LockFile,
        content: &[u8],
    ) -> Result<(), ApplyError> {
        let remote = self.remote(file.path());
        self.on_lease_session(|session| {
            let temporary = self.stage_lock(session, file, content)?;
            session.rename_replacing(&temporary, &remote)
        })
        .map_err(|source| write_error(&lock_path(file), source))
    }

    pub(crate) fn remove_lock_file(&self, file: LockFile) -> Result<(), ApplyError> {
        let remote = self.remote(file.path());
        self.on_lease_session(|session| match session.remove_file(&remote) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            removed => removed,
        })
        .map_err(|source| write_error(&lock_path(file), source))
    }

    /// Move the claim over the lock.
    pub(crate) fn promote_claim(&self) -> Result<(), ApplyError> {
        let (claim, lock) = (
            self.remote(LockFile::Claim.path()),
            self.remote(LockFile::Lock.path()),
        );
        self.on_lease_session(|session| session.rename_replacing(&claim, &lock))
            .map_err(|source| write_error(&lock_path(LockFile::Lock), source))
    }

    /// `content` in a fresh temporary beside `file`, over `session`.
    fn stage_lock(
        &self,
        session: &mut Session,
        file: LockFile,
        content: &[u8],
    ) -> io::Result<Vec<u8>> {
        self.make_directories_on(session, INTERNAL_DIRECTORY, PRIVATE_DIRECTORY)?;
        let temporary = self.beside(file.path());
        write_new(session, temporary, PRIVATE_FILE, |sink| sink(content))
    }

    pub fn ignore_rules(&self) -> Result<Vec<String>, ScanError> {
        match self.read_small(IGNORE_FILE, IGNORE_FILE_LIMIT as usize)? {
            Some(content) => parse_ignore_file(content),
            None => Ok(Vec::new()),
        }
    }

    /// Bytes an unprivileged writer can still use; unknown when the server
    /// cannot tell, which skips the free-space gate.
    pub fn available_space(&self) -> Result<u64, ScanError> {
        match self.session().available_space(&self.root) {
            Ok(available) => Ok(available),
            Err(error) if error.kind() == io::ErrorKind::Unsupported => Ok(u64::MAX),
            Err(error) => Err(io_error(error)),
        }
    }

    /// Reobserve contents and link targets: SFTP stat cannot prove they
    /// stayed unchanged, even when size and modification time match.
    pub fn observe_from(
        &self,
        policy: MetadataPolicy,
        filter: &Filter,
        previous: &std::collections::BTreeMap<SyncPath, ObservedEntry>,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<Observation, ScanError> {
        self.observe_reporting(policy, filter, previous, control, &mut |_| {})
    }

    /// `observe_from`, reporting each entry as it is found and again once
    /// its version is known.
    pub fn observe_reporting(
        &self,
        policy: MetadataPolicy,
        filter: &Filter,
        _previous: &std::collections::BTreeMap<SyncPath, ObservedEntry>,
        control: &mut impl FnMut() -> ScanControl,
        progress: &mut impl FnMut(ObserveStep),
    ) -> Result<Observation, ScanError> {
        let mut observation = Observation::default();
        // Listing the whole tree first, as local roots do, makes checked
        // over listed a fair measure of how far the comparison is.
        let mut listed = Vec::new();
        let mut pending: Vec<Option<SyncPath>> = vec![None];
        while let Some(directory) = pending.pop() {
            if control() == ScanControl::Cancel {
                return Err(ScanError::Cancelled);
            }
            let remote = match &directory {
                None => self.root.clone(),
                Some(path) => self.remote(path.as_bytes()),
            };
            let entries = self
                .session()
                .read_dir(&remote)
                .map_err(|source| at(directory.as_ref(), source))?;
            for (name, attributes) in entries {
                let path = child_path(directory.as_ref(), &name)?;
                if protected(&name) {
                    if temporary(&name) {
                        observation.leftovers.push(path);
                    }
                    continue;
                }
                let kind = attributes.kind();
                if filter.excludes(&path, kind == FileKind::Directory) {
                    continue;
                }
                // Size and age exclusions come before sidecar
                // classification, as in local roots.
                if kind == FileKind::File {
                    if filter.too_large(attributes.size.unwrap_or(0)) {
                        observation.oversized.push(path);
                        continue;
                    }
                    if filter.is_old(modified(&attributes)) {
                        observation.old.push(path.clone());
                    }
                }
                if name.starts_with(b"._") {
                    return Err(ScanError::UnclassifiedSidecar(path));
                }
                if kind == FileKind::Directory {
                    pending.push(Some(path.clone()));
                }
                progress(ObserveStep::Listed);
                listed.push((path, attributes));
            }
        }
        for (path, attributes) in listed {
            let entry = self.observe_entry(&path, &attributes, policy, control)?;
            observation.entries.insert(path, entry);
            progress(ObserveStep::Checked);
        }
        Ok(observation)
    }

    fn observe_entry(
        &self,
        path: &SyncPath,
        attributes: &Attributes,
        policy: MetadataPolicy,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<ObservedEntry, ScanError> {
        let key = stat_key(attributes);
        let observed_at = SystemTime::now();
        let version = self.version(path, attributes, policy, control)?;
        Ok(ObservedEntry {
            version,
            fingerprint: Fingerprint::from_key(key, attributes.size.unwrap_or(0), observed_at),
        })
    }

    /// An entry's version from its attributes, reading its data or link.
    fn version(
        &self,
        path: &SyncPath,
        attributes: &Attributes,
        policy: MetadataPolicy,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<EntryVersion, ScanError> {
        let observed = observed(attributes)?;
        match attributes.kind() {
            FileKind::File => {
                let digest = self.read_file(path, attributes, control, &mut |_| Ok(()))?;
                Ok(entry_version(Payload::File(digest), &observed, policy))
            }
            FileKind::Directory => Ok(entry_version(Payload::Directory, &observed, policy)),
            FileKind::Symlink => {
                let target = self.read_link(path)?;
                Ok(entry_version(Payload::Symlink(&target), &observed, policy))
            }
            FileKind::Other => Err(ScanError::UnsupportedEntry(path.clone())),
        }
    }

    /// Push a file's data to `sink` and return its digest. The file must
    /// keep `expected`'s stat throughout.
    pub(crate) fn read_file(
        &self,
        path: &SyncPath,
        expected: &Attributes,
        control: &mut impl FnMut() -> ScanControl,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<Digest, ScanError> {
        self.read_range(path, expected, 0, control, sink)
    }

    /// Push a file's data from byte `from` on, for a copy that resumes;
    /// the receiver checks the whole file's digest.
    pub(crate) fn read_file_from(
        &self,
        path: &SyncPath,
        expected: &Attributes,
        from: u64,
        control: &mut impl FnMut() -> ScanControl,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<(), ScanError> {
        self.read_range(path, expected, from, control, sink)
            .map(|_| ())
    }

    /// The digest covers the bytes read, from `from` to the end.
    fn read_range(
        &self,
        path: &SyncPath,
        expected: &Attributes,
        from: u64,
        control: &mut impl FnMut() -> ScanControl,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<Digest, ScanError> {
        // The handle is valid only on the session that opened it.
        let mut session = self.session();
        let (handle, remote) = self
            .on_stored_with(&mut session, path.as_bytes(), |session, remote| {
                session
                    .open_read(remote)
                    .map(|handle| (handle, remote.to_vec()))
            })
            .map_err(|source| at(Some(path), source))?;
        let mut hasher = blake3::Hasher::new();
        let mut taken = from;
        let mut cancelled = false;
        let mut overrun = false;
        let read = session.read_from(&handle, from, &mut |chunk| {
            if control() == ScanControl::Cancel {
                cancelled = true;
                return Err(io::Error::other("cancelled"));
            }
            taken += chunk.len() as u64;
            // The stat's size is the read's contract: bytes beyond it
            // are a concurrent edit, not this file's content.
            if expected.size.is_some_and(|size| taken > size) {
                overrun = true;
                return Err(io::Error::other("the file grew while reading"));
            }
            hasher.update(chunk);
            sink(chunk)
        });
        let _ = session.close(handle);
        if cancelled {
            return Err(ScanError::Cancelled);
        }
        if overrun {
            return Err(ScanError::Changed(Some(path.clone())));
        }
        read.map_err(|source| at(Some(path), source))?;
        // An early end is a change too: fewer bytes than the stat said.
        if expected.size.is_some_and(|size| taken != size) {
            return Err(ScanError::Changed(Some(path.clone())));
        }
        let after = session
            .lstat(&remote)
            .map_err(|source| at(Some(path), source))?;
        if stat_key(&after) != stat_key(expected) {
            return Err(ScanError::Changed(Some(path.clone())));
        }
        Ok(Digest::from_bytes(*hasher.finalize().as_bytes()))
    }

    /// The current attributes at `path`, or `None` when nothing is there.
    pub(crate) fn attributes(&self, path: &SyncPath) -> Result<Option<Attributes>, ScanError> {
        Ok(self.lookup(path)?.map(|(_, attributes)| attributes))
    }

    /// Prove `path` still holds `expected` and return its attributes.
    pub(crate) fn verified(
        &self,
        path: &SyncPath,
        expected: &ObservedEntry,
        policy: MetadataPolicy,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<Attributes, ApplyError> {
        self.verified_at(path, expected, policy, control)
            .map(|(_, attributes)| attributes)
    }

    /// `verified`, with where the entry is stored.
    fn verified_at(
        &self,
        path: &SyncPath,
        expected: &ObservedEntry,
        policy: MetadataPolicy,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<(Vec<u8>, Attributes), ApplyError> {
        let changed = || ApplyError::DestinationChanged(path.clone());
        let (stored, attributes) = self.lookup(path)?.ok_or_else(changed)?;
        if self.version(path, &attributes, policy, control)? != expected.version {
            return Err(changed());
        }
        Ok((stored, attributes))
    }

    pub(crate) fn read_link(&self, path: &SyncPath) -> Result<Vec<u8>, ScanError> {
        self.on_stored(path.as_bytes(), |remote| self.session().read_link(remote))
            .map_err(|source| at(Some(path), source))
    }

    /// The version stored at `path` now, reading data only for links and
    /// directories; files take `content` from the copy just made.
    pub(crate) fn placed_version(
        &self,
        path: &SyncPath,
        content: Option<Digest>,
        policy: MetadataPolicy,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<EntryVersion, ApplyError> {
        let attributes = self
            .attributes(path)?
            .ok_or_else(|| ApplyError::DestinationChanged(path.clone()))?;
        Ok(match (attributes.kind(), content) {
            (FileKind::File, Some(digest)) => {
                entry_version(Payload::File(digest), &observed(&attributes)?, policy)
            }
            _ => self.version(path, &attributes, policy, control)?,
        })
    }

    /// Upload into a new temporary beside `path` (a raw path below the
    /// root) and return the temporary's server path.
    pub(crate) fn upload(
        &self,
        path: &[u8],
        permissions: u32,
        durability: Durability,
        produce: impl FnOnce(foldover_net::sftp::Chunks<'_>) -> io::Result<()>,
    ) -> io::Result<Vec<u8>> {
        // Callers refuse Full before staging; stay honest if one does
        // not, before any remote mutation.
        if durability == Durability::Full {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "SFTP cannot guarantee full durability",
            ));
        }
        let temporary = self.beside(path);
        write_new(&mut self.session(), temporary, permissions, produce)
    }

    /// The version a staged file will have, from its attributes and the
    /// digest of the data written.
    pub(crate) fn staged_version(
        &self,
        remote: &[u8],
        digest: Digest,
        policy: MetadataPolicy,
    ) -> io::Result<EntryVersion> {
        let attributes = self.session().lstat(remote)?;
        let observed =
            observed(&attributes).map_err(|error| io::Error::other(error.to_string()))?;
        Ok(entry_version(Payload::File(digest), &observed, policy))
    }

    /// Remove an unplaced temporary; a leftover only costs space.
    pub(crate) fn discard(&self, remote: &[u8]) {
        let _ = self.session().remove_file(remote);
    }

    /// Give a staged or existing entry the synced part of `metadata`.
    pub(crate) fn set_metadata(
        &self,
        remote: &[u8],
        kind: EntryKind,
        metadata: crate::ForeignMetadata,
        policy: MetadataPolicy,
    ) -> io::Result<()> {
        let mut session = self.session();
        let current = session.lstat(remote)?;
        let mode = current.permissions.unwrap_or(0) & PERMISSION_BITS;
        let wanted = match policy.permissions() {
            foldover_types::Propagation::Sync => metadata.permissions & PERMISSION_BITS,
            foldover_types::Propagation::Ignore if kind == EntryKind::File => {
                if metadata.permissions & EXECUTE_BITS != 0 {
                    mode | ((mode & 0o444) >> 2)
                } else {
                    mode & !EXECUTE_BITS
                }
            }
            foldover_types::Propagation::Ignore => mode,
        };
        let times = (policy.modification_times() == foldover_types::Propagation::Sync
            && kind == EntryKind::File)
            .then(|| {
                let seconds = whole_seconds(metadata.modified);
                (seconds, seconds)
            });
        if wanted == mode && times.is_none() {
            return Ok(());
        }
        session.set_attributes(
            remote,
            &Attributes {
                permissions: (wanted != mode).then_some(wanted),
                times,
                ..Attributes::default()
            },
        )
    }

    /// Create a folder at `path` with `permissions`, named composed.
    pub(crate) fn make_directory(&self, path: &SyncPath, permissions: u32) -> io::Result<()> {
        let remote = self.fresh(path.as_bytes());
        self.session().make_directory(&remote, permissions)
    }

    /// Create a link beside `path` and return its server path.
    pub(crate) fn stage_link(&self, path: &[u8], target: &[u8]) -> io::Result<Vec<u8>> {
        let temporary = self.beside(path);
        self.session().symlink(target, &temporary)?;
        Ok(temporary)
    }

    /// Move a staged temporary to `path`. Creating never replaces, and
    /// names the entry composed beside the temporary; a replacement first
    /// sets the current entry aside and takes its spelling, as its folder
    /// lists it.
    pub(crate) fn place(
        &self,
        temporary: &[u8],
        path: &SyncPath,
        replacing: Option<&Attributes>,
        run: &RunScope,
    ) -> Result<(), ApplyError> {
        let target = match replacing {
            Some(expected) => {
                let current = self.lookup(path)?;
                let Some((stored, _)) =
                    current.filter(|(_, current)| stat_key(current) == stat_key(expected))
                else {
                    let _ = self.session().remove_file(temporary);
                    return Err(ApplyError::DestinationChanged(path.clone()));
                };
                let target = self.listed(&stored);
                if let Err(error) = self.set_aside(&stored, path, run) {
                    let _ = self.session().remove_file(temporary);
                    return Err(error);
                }
                target
            }
            None => {
                let folder = &temporary[..temporary
                    .iter()
                    .rposition(|byte| *byte == b'/')
                    .expect("temporaries are inside the root")];
                let name = path.components().last().expect("paths have a name");
                [folder, b"/".as_slice(), &spelling::compose(name)].concat()
            }
        };
        let placed = self.session().rename(temporary, &target);
        if let Err(source) = placed {
            let _ = self.session().remove_file(temporary);
            return Err(match self.attributes(path) {
                Ok(Some(_)) => ApplyError::DestinationChanged(path.clone()),
                _ => write_error(path, source),
            });
        }
        Ok(())
    }

    /// Move `path`, stored at `stored`, to the run's archive, creating its
    /// folders there, or delete it where the run keeps no archive. Only
    /// files and links come here: `remove` takes folders when empty, and
    /// `place` never replaces a folder, since a file meeting one is a type
    /// conflict.
    fn set_aside(&self, stored: &[u8], path: &SyncPath, run: &RunScope) -> Result<(), ApplyError> {
        if run.archiving() == Archiving::Off {
            return self
                .session()
                .remove_file(stored)
                .map_err(|source| write_error(path, source));
        }
        let archived = run.archive_path(path)?;
        if let Some(parent) = archived.parent() {
            self.make_directories(parent.as_bytes(), PRIVATE_DIRECTORY)
                .map_err(|source| write_error(path, source))?;
        }
        self.session()
            .rename(stored, &self.remote(archived.as_bytes()))
            .map_err(|source| write_error(path, source))
    }

    /// Move the file or link at `from`, still `current`, to `to`, named
    /// composed as new names are and creating missing folders. The
    /// server's rename never replaces.
    pub fn move_entry(
        &self,
        from: &SyncPath,
        current: &ObservedEntry,
        to: &SyncPath,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<(), ApplyError> {
        check_durability(run.options().durability(), from)?;
        let (stored, attributes) = self
            .verified_at(from, current, run.options().metadata(), control)
            .map_err(|error| match error {
                ApplyError::Scan(ScanError::Changed(_)) => {
                    ApplyError::DestinationChanged(from.clone())
                }
                error => error,
            })?;
        if attributes.kind() == FileKind::Directory {
            return Err(ApplyError::Unsupported(from.clone()));
        }
        // Missing folders come composed, inside those that remain in the
        // spelling they have.
        if let Some(parent) = to.parent()
            && self.attributes(&parent)?.is_none()
        {
            let folders = self.creatable(parent.as_bytes());
            self.make_directories(&folders, DEFAULT_DIRECTORY)
                .map_err(|source| write_error(to, source))?;
        }
        // Making folders took round trips: the entry must still be the one
        // verified, or the move would take a replacement.
        let still = self
            .lookup(from)?
            .is_some_and(|(_, now)| stat_key(&now) == stat_key(&attributes));
        if !still {
            return Err(ApplyError::DestinationChanged(from.clone()));
        }
        // Found before the session is held: finding it may use it.
        let target = self.fresh(to.as_bytes());
        let moved = self.session().rename(&stored, &target);
        if let Err(source) = moved {
            return Err(match self.attributes(to) {
                Ok(Some(_)) => ApplyError::DestinationChanged(to.clone()),
                _ => write_error(from, source),
            });
        }
        Ok(())
    }

    /// Archive the entry at `path` for this run, or delete it where the
    /// run keeps no archive; folders are removed only when empty.
    pub fn remove(
        &self,
        path: &SyncPath,
        current: &ObservedEntry,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<(), ApplyError> {
        check_durability(run.options().durability(), path)?;
        let (remote, attributes) = self
            .verified_at(path, current, run.options().metadata(), control)
            .map_err(|error| match error {
                ApplyError::Scan(ScanError::Changed(_)) => {
                    ApplyError::DestinationChanged(path.clone())
                }
                error => error,
            })?;
        if attributes.kind() != FileKind::Directory {
            return self.set_aside(&remote, path, run);
        }
        let mut session = self.session();
        match session.remove_directory(&remote) {
            Ok(()) => Ok(()),
            Err(source) => match session.read_dir(&remote) {
                Ok(entries) if !entries.is_empty() => Err(ApplyError::NotEmpty(path.clone())),
                _ => Err(write_error(path, source)),
            },
        }
    }

    /// Archive a temporary an interrupted write left behind, or delete it
    /// where the run keeps no archive.
    pub fn recover_leftover(&self, leftover: &SyncPath, run: &RunScope) -> Result<(), ApplyError> {
        check_durability(run.options().durability(), leftover)?;
        self.set_aside(&self.remote(leftover.as_bytes()), leftover, run)
    }

    /// Give `path` new synced metadata; the content stays.
    pub(crate) fn update_metadata(
        &self,
        path: &SyncPath,
        kind: EntryKind,
        metadata: crate::ForeignMetadata,
        policy: MetadataPolicy,
    ) -> Result<(), ApplyError> {
        self.on_stored(path.as_bytes(), |remote| {
            self.set_metadata(remote, kind, metadata, policy)
        })
        .map_err(|source| write_error(path, source))
    }

    /// The metadata a copy to another system carries.
    pub(crate) fn foreign_metadata(attributes: &Attributes) -> crate::ForeignMetadata {
        crate::ForeignMetadata {
            permissions: attributes.permissions.unwrap_or(0) & PERMISSION_BITS,
            modified: modified(attributes),
        }
    }

    /// The version of an entry whose content is unchanged, from its
    /// current attributes.
    pub(crate) fn metadata_version(
        &self,
        path: &SyncPath,
        content: foldover_types::EntryContent,
        policy: MetadataPolicy,
    ) -> Result<EntryVersion, ApplyError> {
        let attributes = self
            .attributes(path)?
            .ok_or_else(|| ApplyError::DestinationChanged(path.clone()))?;
        Ok(metadata_version(content, &observed(&attributes)?, policy))
    }

    /// Permanently delete archived runs older than `retention`.
    pub fn prune_archive(&self, retention: Duration, now: SystemTime) -> Result<u64, ApplyError> {
        let archive = self.remote(ARCHIVE_ROOT);
        let runs = match self.session().read_dir(&archive) {
            Ok(runs) => runs,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(io_error(error).into()),
        };
        let mut pruned = 0;
        for (name, attributes) in runs {
            let expired = archive_time(&name).is_some_and(|started| {
                now.duration_since(started).is_ok_and(|age| age > retention)
            });
            if !expired || attributes.kind() != FileKind::Directory {
                continue;
            }
            self.remove_tree(&[archive.as_slice(), b"/", &name].concat())
                .map_err(|source| ApplyError::Scan(io_error(source)))?;
            pruned += 1;
        }
        Ok(pruned)
    }

    fn remove_tree(&self, remote: &[u8]) -> io::Result<()> {
        let entries = self.session().read_dir(remote)?;
        for (name, attributes) in entries {
            let child = [remote, b"/", &name].concat();
            if attributes.kind() == FileKind::Directory {
                self.remove_tree(&child)?;
            } else {
                self.session().remove_file(&child)?;
            }
        }
        self.session().remove_directory(remote)
    }

    /// Archived runs, newest first.
    pub fn archived_runs(&self) -> Result<Vec<ArchivedRun>, ScanError> {
        let entries = match self.session().read_dir(&self.remote(ARCHIVE_ROOT)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_error(error)),
        };
        let mut runs: Vec<ArchivedRun> = entries
            .into_iter()
            .filter_map(|(name, attributes)| {
                // Only directories are runs; a file or link carrying a
                // run's timestamp name is unrelated content.
                if attributes.kind() != FileKind::Directory {
                    return None;
                }
                let started = archive_time(&name)?;
                Some(ArchivedRun::new(String::from_utf8(name).ok()?, started))
            })
            .collect();
        runs.sort_by_key(|run| std::cmp::Reverse(run.started()));
        Ok(runs)
    }

    /// Files and links `run` archived, in path order after `after`.
    pub fn archived_items(
        &self,
        run: &str,
        after: Option<&SyncPath>,
        limit: usize,
    ) -> Result<(Vec<ArchivedItem>, bool), ScanError> {
        let folder = crate::local::archive::run_folder(run).ok_or(ScanError::UnknownArchiveRun)?;
        let mut items = Vec::new();
        let mut pending: Vec<Option<SyncPath>> = vec![None];
        while let Some(relative) = pending.pop() {
            let directory = match &relative {
                None => folder.clone(),
                Some(relative) => crate::local::archive::join(&folder, relative.as_bytes())?,
            };
            let entries = self
                .session()
                .read_dir(&self.remote(directory.as_bytes()))
                .map_err(|source| at(Some(&directory), source))?;
            for (name, attributes) in entries {
                let path = child_path(relative.as_ref(), &name)?;
                match attributes.kind() {
                    FileKind::Directory => pending.push(Some(path)),
                    FileKind::File | FileKind::Symlink
                        if after.is_none_or(|after| &path > after) =>
                    {
                        items.push(ArchivedItem::new(path, attributes.size.unwrap_or(0)));
                    }
                    _ => {}
                }
            }
        }
        items.sort_by(|left, right| left.path().cmp(right.path()));
        let more = items.len() > limit;
        items.truncate(limit);
        Ok((items, more))
    }

    /// Put an archived item back at its path, or beside it as
    /// `name (restored).ext` when that is taken. Returns where it went.
    pub fn put_back(&self, run: &str, path: &SyncPath) -> Result<SyncPath, ApplyError> {
        if path.components().any(protected) {
            return Err(ApplyError::Unsupported(path.clone()));
        }
        let folder = crate::local::archive::run_folder(run).ok_or(ScanError::UnknownArchiveRun)?;
        let archived = crate::local::archive::join(&folder, path.as_bytes())?;
        // Found in whichever spelling it is stored; that is what moves.
        let Some((stored, _)) = self.lookup(&archived)? else {
            return Err(ApplyError::NotArchived(path.clone()));
        };
        // A missing folder comes back composed, as new names do, inside
        // the folders that remain in the spelling they have.
        if let Some(parent) = path.parent()
            && self.attributes(&parent)?.is_none()
        {
            let folders = self.creatable(parent.as_bytes());
            self.make_directories(&folders, DEFAULT_DIRECTORY)
                .map_err(|source| write_error(path, source))?;
        }
        for attempt in 0..PUT_BACK_ATTEMPTS {
            let destination = match attempt {
                0 => path.clone(),
                attempt => sibling_name(path, RESTORED, attempt)?,
            };
            if self.attributes(&destination)?.is_some() {
                continue;
            }
            // Found before the session is held: finding it may use it.
            let target = self.fresh(destination.as_bytes());
            return self
                .session()
                .rename(&stored, &target)
                .map(|()| destination.clone())
                .map_err(|source| write_error(&destination, source));
        }
        Err(ApplyError::Occupied(path.clone()))
    }

    /// Copy the file or link at `path`, still `expected`, to a free name
    /// beside it labelled `from …`, dated by its modification day.
    pub fn keep_copy(
        &self,
        path: &SyncPath,
        expected: EntryVersion,
        from: &str,
        options: &foldover_types::PairOptions,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<SyncPath, ApplyError> {
        check_durability(options.durability(), path)?;
        let changed = || ApplyError::SourceChanged(path.clone());
        let attributes = self.attributes(path)?.ok_or_else(changed)?;
        if self.version(path, &attributes, options.metadata(), control)? != expected {
            return Err(changed());
        }
        let label = format!("from {from}, {}", date_name(modified(&attributes)));
        let mut free = None;
        for attempt in 1..=PUT_BACK_ATTEMPTS {
            let candidate = sibling_name(path, &label, attempt)?;
            if self.attributes(&candidate)?.is_none() {
                free = Some(candidate);
                break;
            }
        }
        let copy = free.ok_or_else(|| ApplyError::Occupied(path.clone()))?;
        let temporary = match attributes.kind() {
            FileKind::File => {
                // SFTP has no server-side copy, but the session can move
                // bounded chunks read-to-write: staging this way never
                // holds the whole file, however large it is.
                self.stage_copy(path, expected, &attributes, &copy, options, control)?
            }
            FileKind::Symlink => {
                let target = self.read_link(path)?;
                self.stage_link(copy.as_bytes(), &target)
                    .map_err(|source| write_error(&copy, source))?
            }
            FileKind::Directory | FileKind::Other => {
                return Err(ApplyError::Unsupported(path.clone()));
            }
        };
        let target = self.fresh(copy.as_bytes());
        let placed = self.session().rename(&temporary, &target);
        if let Err(source) = placed {
            let _ = self.session().remove_file(&temporary);
            return Err(write_error(&copy, source));
        }
        Ok(copy)
    }

    /// Stream the file at `path`, still holding `attributes`, into a
    /// staged temporary beside `copy` and return its server path. One
    /// session guard keeps the copy's reads and writes in request order;
    /// the observer hashes each chunk and can cancel. The temporary is
    /// removed and every handle closed on any failure, and the source is
    /// re-stat'ed before the staged file is allowed to be published.
    fn stage_copy(
        &self,
        path: &SyncPath,
        expected: EntryVersion,
        attributes: &Attributes,
        copy: &SyncPath,
        options: &foldover_types::PairOptions,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<Vec<u8>, ApplyError> {
        // `copy_data` needs the length up front; a file the server
        // cannot size cannot be copied bounded, which is honest rather
        // than an unbounded fallback.
        let size = attributes
            .size
            .ok_or_else(|| ApplyError::Unsupported(path.clone()))?;
        let (source_remote, _) = self
            .lookup(path)?
            .ok_or_else(|| ApplyError::SourceChanged(path.clone()))?;
        let temporary = self.beside(copy.as_bytes());
        let permissions = attributes.permissions.unwrap_or(DEFAULT_FILE) & PERMISSION_BITS;
        let mut hasher = blake3::Hasher::new();
        let mut cancelled = false;
        let copied = {
            let mut session = self.session();
            let source = match session.open_read(&source_remote) {
                Ok(handle) => handle,
                Err(error) => return Err(at(Some(path), error).into()),
            };
            let staged = (|| -> io::Result<(u64, Attributes)> {
                let target = session.create_new(&temporary, permissions)?;
                let copied = session.copy_data(&source, &target, size, &mut |chunk| {
                    if control() == ScanControl::Cancel {
                        cancelled = true;
                        return Err(io::Error::other("cancelled"));
                    }
                    hasher.update(chunk);
                    Ok(())
                });
                let closed = session.close(target);
                // Prove the source still holds its stat after its last
                // byte was read.
                let after = copied.is_ok().then(|| session.lstat(&source_remote));
                let bytes = copied.and_then(|count| closed.map(|()| count))?;
                let after = after.expect("a finished copy re-stats the source")?;
                Ok((bytes, after))
            })();
            let _ = session.close(source);
            staged
        };
        let (bytes, after) = match copied {
            Err(source) => {
                self.discard(&temporary);
                return Err(if cancelled {
                    ScanError::Cancelled.into()
                } else {
                    write_error(copy, source)
                });
            }
            Ok(pair) => pair,
        };
        if bytes != size || stat_key(&after) != stat_key(attributes) {
            self.discard(&temporary);
            return Err(ApplyError::SourceChanged(path.clone()));
        }
        let digest = Digest::from_bytes(*hasher.finalize().as_bytes());
        let stored = self
            .staged_version(&temporary, digest, options.metadata())
            .map_err(|source| {
                self.discard(&temporary);
                write_error(copy, source)
            })?;
        if stored.content() != expected.content() {
            self.discard(&temporary);
            return Err(ApplyError::SourceChanged(path.clone()));
        }
        self.set_metadata(
            &temporary,
            EntryKind::File,
            Self::foreign_metadata(attributes),
            options.metadata(),
        )
        .map_err(|source| {
            self.discard(&temporary);
            write_error(copy, source)
        })?;
        Ok(temporary)
    }

    /// Create every missing folder of `path` (raw, below the root).
    fn make_directories(&self, path: &[u8], permissions: u32) -> io::Result<()> {
        self.make_directories_on(&mut self.session(), path, permissions)
    }

    fn make_directories_on(
        &self,
        session: &mut Session,
        path: &[u8],
        permissions: u32,
    ) -> io::Result<()> {
        let mut prefix = Vec::new();
        for component in path.split(|byte| *byte == b'/') {
            if !prefix.is_empty() {
                prefix.push(b'/');
            }
            prefix.extend_from_slice(component);
            let remote = self.remote(&prefix);
            if let Err(error) = session.make_directory(&remote, permissions) {
                // SFTP reports an existing folder as a generic failure.
                match session.lstat(&remote) {
                    Ok(existing) if existing.kind() == FileKind::Directory => {}
                    _ => return Err(error),
                }
            }
        }
        Ok(())
    }

    /// A small file below the root, or `None` when it is missing.
    fn read_small(&self, path: &[u8], limit: usize) -> Result<Option<Vec<u8>>, ScanError> {
        read_small_on(&mut self.session(), &self.remote(path), limit).map_err(io_error)
    }
}

/// The small file at `remote`, or `None` when it is missing.
fn read_small_on(
    session: &mut Session,
    remote: &[u8],
    limit: usize,
) -> io::Result<Option<Vec<u8>>> {
    let handle: Handle = match session.open_read(remote) {
        Ok(handle) => handle,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut content = Vec::new();
    let read = session.read_all(&handle, &mut |chunk| {
        content.extend_from_slice(chunk);
        if content.len() > limit {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "File too large"));
        }
        Ok(())
    });
    let _ = session.close(handle);
    read?;
    Ok(Some(content))
}

/// `produce`'s data in a new file at `remote`, removed again on failure.
fn write_new(
    session: &mut Session,
    remote: Vec<u8>,
    permissions: u32,
    produce: impl FnOnce(foldover_net::sftp::Chunks<'_>) -> io::Result<()>,
) -> io::Result<Vec<u8>> {
    let handle = session.create_new(&remote, permissions)?;
    let written = session.write_chunks(&handle, produce);
    let closed = session.close(handle);
    if let Err(error) = written.and(closed) {
        let _ = session.remove_file(&remote);
        return Err(error);
    }
    Ok(remote)
}

fn lock_path(file: LockFile) -> SyncPath {
    SyncPath::try_from(file.path()).expect("lease paths are valid")
}

/// Full durability cannot be promised over SFTP: namespace changes have
/// no protocol flush, so a write under it refuses rather than degrades.
pub(crate) fn check_durability(durability: Durability, path: &SyncPath) -> Result<(), ApplyError> {
    if durability == Durability::Full {
        return Err(ApplyError::DurabilityUnsupported(path.clone()));
    }
    Ok(())
}

/// `path`'s parent with a fresh temporary name, e.g. `docs/.foldover-tmp-…`.
/// A new temporary's name.
fn temporary_name() -> Vec<u8> {
    [
        TEMPORARY_PREFIX,
        uuid::Uuid::new_v4().simple().to_string().as_bytes(),
    ]
    .concat()
}

/// Available stat fields, useful as observations but not unchanged-data proof.
fn stat_key(attributes: &Attributes) -> Vec<u8> {
    let mut key = b"sftp".to_vec();
    key.extend_from_slice(&attributes.size.unwrap_or(u64::MAX).to_le_bytes());
    key.extend_from_slice(&attributes.modified().unwrap_or(0).to_le_bytes());
    key.extend_from_slice(&attributes.permissions.unwrap_or(0).to_le_bytes());
    let (user, group) = attributes.owner.unwrap_or_default();
    key.extend_from_slice(&user.to_le_bytes());
    key.extend_from_slice(&group.to_le_bytes());
    key
}

fn modified(attributes: &Attributes) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(u64::from(attributes.modified().unwrap_or(0)))
}

fn whole_seconds(time: SystemTime) -> u32 {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    u32::try_from(seconds).unwrap_or(u32::MAX)
}

/// The fields a version depends on. SFTP reports no extended attributes,
/// flags or ACLs, so none are tracked.
fn observed(attributes: &Attributes) -> Result<Observed<'static>, ScanError> {
    let kind = match attributes.kind() {
        FileKind::File => NodeKind::File,
        FileKind::Directory => NodeKind::Directory,
        FileKind::Symlink => NodeKind::Symlink,
        FileKind::Other => NodeKind::Other,
    };
    Ok(Observed {
        kind,
        permissions: attributes.permissions.unwrap_or(0),
        modified: modified(attributes),
        user_flags: None,
        attributes: Some(Vec::new()),
        access_control: None,
    })
}

fn io_error(source: io::Error) -> ScanError {
    ScanError::Io { path: None, source }
}

fn at(path: Option<&SyncPath>, source: io::Error) -> ScanError {
    ScanError::Io {
        path: path.cloned(),
        source,
    }
}

fn write_error(path: &SyncPath, source: io::Error) -> ApplyError {
    ApplyError::Write {
        path: path.clone(),
        source: source.into(),
    }
}

#[cfg(test)]
mod tests;
