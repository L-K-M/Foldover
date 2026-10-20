//! One side of a pair: a local root, a folder on an SFTP server, or one a
//! paired Mac shares. The engine works through this type only. Copies between two local roots
//! keep their native path (clones, resource forks, ACLs); every other
//! combination streams data and carries mode and modification time.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::num::NonZeroUsize;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime};

use foldover_types::{
    CaseMatching, Durability, EndpointId, EntryVersion, MetadataPolicy, NameMatching, PairOptions,
    PairRoot, ScanControl, SyncPath, UnicodeMatching,
};

use crate::lease::LockFile;
use crate::link::PeerLink;
use crate::remote::PeerEndpoint;
use crate::sftp::SftpEndpoint;
use crate::{
    ApplyError, ArchivedItem, ArchivedRun, Expected, Filter, LocalEndpoint, Observation,
    ObservedEntry, RootRelation, RunScope, ScanError,
};

/// One directory entry as a quick change check sees it (PLAN.md §4.8):
/// enough to tell that something changed, not what.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListedEntry {
    name: Vec<u8>,
    kind: ListedKind,
    size: u64,
    modified: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListedKind {
    Directory,
    Other,
}

impl ListedEntry {
    fn new(name: Vec<u8>, kind: ListedKind, size: u64, modified: Option<SystemTime>) -> Self {
        Self {
            name,
            kind,
            size,
            modified,
        }
    }

    pub fn name(&self) -> &[u8] {
        &self.name
    }

    pub fn kind(&self) -> ListedKind {
        self.kind
    }

    /// An entry in `directory`, unless `filter` leaves it out of syncs.
    /// A name no sync path can hold is kept: a run will report it.
    pub(crate) fn kept(
        directory: Option<&SyncPath>,
        filter: &Filter,
        name: Vec<u8>,
        kind: ListedKind,
        size: u64,
        modified: Option<SystemTime>,
    ) -> Option<Self> {
        let excluded = crate::local::child_path(directory, &name)
            .is_ok_and(|path| filter.excludes(&path, kind == ListedKind::Directory));
        (!excluded).then(|| Self::new(name, kind, size, modified))
    }
}

/// How far an observation has got, e.g. for "Comparing… 12,345 items".
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObserveStep {
    /// An entry was found.
    Listed,
    /// An entry's version is known, read or reused.
    Checked,
}

pub enum Endpoint {
    Local(LocalEndpoint),
    Sftp(SftpEndpoint),
    Peer(PeerEndpoint),
    /// A folder in memory with chosen quirks, for simulations.
    #[cfg(feature = "simulation")]
    Memory(crate::memory::MemoryEndpoint),
}

/// What opening a root may need beyond its bytes: the running peer link,
/// for folders on paired Macs. Opening those blocks on the link. It does
/// not keep the link running: the agent owns it, and the link's host
/// holds the engine, which holds this.
#[derive(Clone, Default)]
pub struct Reach {
    link: Option<Weak<PeerLink>>,
}

impl Reach {
    /// Folders on paired Macs, through `link`.
    pub fn through(link: &Arc<PeerLink>) -> Self {
        Self {
            link: Some(Arc::downgrade(link)),
        }
    }

    /// The running link, if Mac-to-Mac is on.
    pub fn link(&self) -> Option<Arc<PeerLink>> {
        self.link.as_ref().and_then(Weak::upgrade)
    }
}

impl Endpoint {
    /// Open a root and read its first entry, so refused access shows here.
    pub fn open(root: &PairRoot, reach: &Reach) -> Result<Self, ScanError> {
        match root {
            PairRoot::Local(root) => {
                let endpoint = LocalEndpoint::open(Path::new(OsStr::from_bytes(root.as_bytes())))?;
                endpoint.probe_access()?;
                Ok(Self::Local(endpoint))
            }
            PairRoot::Sftp(root) => {
                let endpoint = SftpEndpoint::open(root)?;
                endpoint.probe_access()?;
                Ok(Self::Sftp(endpoint))
            }
            PairRoot::Peer(root) => {
                let link = reach.link().ok_or_else(|| {
                    ScanError::MacUnreachable("Mac-to-Mac is off; turn it on in Settings".into())
                })?;
                Ok(Self::Peer(PeerEndpoint::open(link, root)?))
            }
        }
    }

    pub fn identity(&self) -> Result<Option<EndpointId>, ScanError> {
        match self {
            Self::Local(endpoint) => endpoint.identity(),
            Self::Sftp(endpoint) => endpoint.identity(),
            Self::Peer(endpoint) => endpoint.identity(),
            #[cfg(feature = "simulation")]
            Self::Memory(endpoint) => endpoint.identity(),
        }
    }

    /// Whether this folder can hold Foldover's records in `.foldover`.
    /// Another Mac's folder always can: its agent keeps them.
    pub fn probe_records(&self) -> Result<(), ApplyError> {
        match self {
            Self::Local(endpoint) => endpoint.probe_records(),
            Self::Sftp(endpoint) => endpoint.probe_records(),
            Self::Peer(_) => Ok(()),
            #[cfg(feature = "simulation")]
            Self::Memory(_) => Ok(()),
        }
    }

    pub fn ensure_identity(&self, durability: Durability) -> Result<EndpointId, ApplyError> {
        match self {
            Self::Local(endpoint) => endpoint.ensure_identity(durability),
            Self::Sftp(endpoint) => endpoint.ensure_identity(durability),
            Self::Peer(endpoint) => endpoint.ensure_identity(durability),
            #[cfg(feature = "simulation")]
            Self::Memory(endpoint) => endpoint.ensure_identity(durability),
        }
    }

    pub fn ignore_rules(&self) -> Result<Vec<String>, ScanError> {
        match self {
            Self::Local(endpoint) => endpoint.ignore_rules(),
            Self::Sftp(endpoint) => endpoint.ignore_rules(),
            Self::Peer(endpoint) => endpoint.ignore_rules(),
            #[cfg(feature = "simulation")]
            Self::Memory(endpoint) => endpoint.ignore_rules(),
        }
    }

    /// Observe the root, reporting each entry as it is found and again
    /// once its version is known.
    pub fn observe_from(
        &self,
        policy: MetadataPolicy,
        filter: &Filter,
        previous: &BTreeMap<SyncPath, ObservedEntry>,
        control: &mut impl FnMut() -> ScanControl,
        progress: &mut impl FnMut(ObserveStep),
    ) -> Result<Observation, ScanError> {
        match self {
            Self::Local(endpoint) => {
                endpoint.observe_reporting(policy, filter, previous, control, progress)
            }
            Self::Sftp(endpoint) => {
                endpoint.observe_reporting(policy, filter, previous, control, progress)
            }
            // The other Mac keeps its own previous observation.
            Self::Peer(endpoint) => endpoint.observe_reporting(policy, filter, control, progress),
            #[cfg(feature = "simulation")]
            Self::Memory(endpoint) => endpoint.observe(policy, filter, control, progress),
        }
    }

    /// How this folder's filesystem compares names. SFTP servers match
    /// bytes, as Linux filesystems do, but Foldover writes new names there
    /// composed and finds an entry in any spelling (PLAN.md §4.10): case
    /// counts there, normalization does not.
    pub fn name_matching(&self) -> NameMatching {
        match self {
            Self::Local(endpoint) => endpoint.name_matching(),
            Self::Sftp(_) => NameMatching::new(CaseMatching::Exact, UnicodeMatching::Normalized),
            Self::Peer(endpoint) => endpoint.name_matching(),
            #[cfg(feature = "simulation")]
            Self::Memory(endpoint) => endpoint.name_matching(),
        }
    }

    pub fn available_space(&self) -> Result<u64, ScanError> {
        match self {
            Self::Local(endpoint) => endpoint.available_space(),
            Self::Sftp(endpoint) => endpoint.available_space(),
            Self::Peer(endpoint) => endpoint.available_space(),
            #[cfg(feature = "simulation")]
            Self::Memory(endpoint) => Ok(endpoint.available_space()),
        }
    }

    /// Where a local root's network volume is mounted from; SFTP roots
    /// have nothing to mount.
    pub fn network_source(&self) -> Result<Option<String>, ScanError> {
        match self {
            Self::Local(endpoint) => endpoint.network_source(),
            Self::Sftp(_) | Self::Peer(_) => Ok(None),
            #[cfg(feature = "simulation")]
            Self::Memory(_) => Ok(None),
        }
    }

    /// One directory's entries the filter keeps, without recursion or
    /// Foldover's own names; `None` once the directory is gone. A paired
    /// Mac reports its own changes, so its folders are never listed this
    /// way.
    pub fn list(
        &self,
        directory: Option<&SyncPath>,
        filter: &Filter,
    ) -> Result<Option<Vec<ListedEntry>>, ScanError> {
        match self {
            Self::Local(endpoint) => endpoint.list(directory, filter),
            Self::Sftp(endpoint) => endpoint.list(directory, filter),
            #[cfg(feature = "simulation")]
            Self::Memory(endpoint) => endpoint.list(directory, filter),
            Self::Peer(_) => Err(ScanError::Io {
                path: directory.cloned(),
                source: std::io::ErrorKind::Unsupported.into(),
            }),
        }
    }

    pub fn recover_leftover(&self, leftover: &SyncPath, run: &RunScope) -> Result<(), ApplyError> {
        match self {
            Self::Local(endpoint) => endpoint.recover_leftover(leftover, run),
            Self::Sftp(endpoint) => endpoint.recover_leftover(leftover, run),
            Self::Peer(endpoint) => endpoint.recover_leftover(leftover, run),
            #[cfg(feature = "simulation")]
            Self::Memory(endpoint) => endpoint.recover_leftover(leftover, run),
        }
    }

    /// Move the file or link at `from`, still `current`, to `to`, creating
    /// missing folders; nothing at `to` is replaced (PLAN.md §4.7).
    /// Another Mac's folder takes no moves yet: its runs copy instead.
    pub fn move_entry(
        &self,
        from: &SyncPath,
        current: &ObservedEntry,
        to: &SyncPath,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<(), ApplyError> {
        match self {
            Self::Local(endpoint) => endpoint.move_entry(from, current, to, run, control),
            Self::Sftp(endpoint) => endpoint.move_entry(from, current, to, run, control),
            Self::Peer(_) => Err(ApplyError::Unsupported(from.clone())),
            #[cfg(feature = "simulation")]
            Self::Memory(endpoint) => endpoint.move_entry(from, current, to, run),
        }
    }

    /// Whether this folder takes moves (`move_entry`).
    pub fn moves(&self) -> bool {
        !matches!(self, Self::Peer(_))
    }

    /// How many copies into or out of this folder may run at once (PLAN.md
    /// §4.9). A paired Mac's link has few streams to spare beside change
    /// reports and lease beats.
    pub fn workers(&self) -> NonZeroUsize {
        match self {
            Self::Local(endpoint) => endpoint.workers(),
            Self::Sftp(endpoint) => endpoint.workers(),
            Self::Peer(_) => NonZeroUsize::MIN,
            #[cfg(feature = "simulation")]
            Self::Memory(endpoint) => endpoint.workers(),
        }
    }

    pub fn remove(
        &self,
        path: &SyncPath,
        current: &ObservedEntry,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<(), ApplyError> {
        match self {
            Self::Local(endpoint) => endpoint.remove(path, current, run, control),
            Self::Sftp(endpoint) => endpoint.remove(path, current, run, control),
            Self::Peer(endpoint) => endpoint.remove(path, current, run),
            #[cfg(feature = "simulation")]
            Self::Memory(endpoint) => endpoint.remove(path, current, run),
        }
    }

    /// Make `path` hold `source`'s entry; anything replaced is archived.
    pub fn copy_from(
        &self,
        source: &Endpoint,
        path: &SyncPath,
        from: &ObservedEntry,
        current: Expected<'_>,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<EntryVersion, ApplyError> {
        match (self, source) {
            (Self::Local(to), Self::Local(from_endpoint)) => {
                to.copy_from(from_endpoint, path, from, current, run, control)
            }
            #[cfg(feature = "simulation")]
            (Self::Memory(to), Self::Memory(from_endpoint)) => {
                to.copy_from(from_endpoint, path, from, current, run)
            }
            #[cfg(feature = "simulation")]
            (Self::Memory(_), _) | (_, Self::Memory(_)) => {
                Err(ApplyError::Unsupported(path.clone()))
            }
            _ => crate::sftp::copy(self, source, path, from, current, run, control),
        }
    }

    /// Give `path` `source`'s synced metadata, leaving content alone.
    pub fn update_metadata_from(
        &self,
        source: &Endpoint,
        path: &SyncPath,
        from: &ObservedEntry,
        current: &ObservedEntry,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<EntryVersion, ApplyError> {
        match (self, source) {
            (Self::Local(to), Self::Local(from_endpoint)) => {
                to.update_metadata_from(from_endpoint, path, from, current, run, control)
            }
            #[cfg(feature = "simulation")]
            (Self::Memory(to), Self::Memory(from_endpoint)) => {
                to.update_metadata_from(from_endpoint, path, from, current, run)
            }
            #[cfg(feature = "simulation")]
            (Self::Memory(_), _) | (_, Self::Memory(_)) => {
                Err(ApplyError::Unsupported(path.clone()))
            }
            _ => crate::sftp::update_metadata(self, source, path, from, current, run, control),
        }
    }

    pub fn prune_archive(&self, retention: Duration, now: SystemTime) -> Result<u64, ApplyError> {
        match self {
            Self::Local(endpoint) => endpoint.prune_archive(retention, now),
            Self::Sftp(endpoint) => endpoint.prune_archive(retention, now),
            Self::Peer(endpoint) => endpoint.prune_archive(retention, now),
            #[cfg(feature = "simulation")]
            Self::Memory(endpoint) => endpoint.prune_archive(retention, now),
        }
    }

    pub fn archived_runs(&self) -> Result<Vec<ArchivedRun>, ScanError> {
        match self {
            Self::Local(endpoint) => endpoint.archived_runs(),
            Self::Sftp(endpoint) => endpoint.archived_runs(),
            Self::Peer(endpoint) => endpoint.archived_runs(),
            #[cfg(feature = "simulation")]
            Self::Memory(_) => Err(unsupported_in_memory()),
        }
    }

    pub fn archived_items(
        &self,
        run: &str,
        after: Option<&SyncPath>,
        limit: usize,
    ) -> Result<(Vec<ArchivedItem>, bool), ScanError> {
        match self {
            Self::Local(endpoint) => endpoint.archived_items(run, after, limit),
            Self::Sftp(endpoint) => endpoint.archived_items(run, after, limit),
            Self::Peer(endpoint) => endpoint.archived_items(run, after, limit),
            #[cfg(feature = "simulation")]
            Self::Memory(_) => Err(unsupported_in_memory()),
        }
    }

    pub fn put_back(
        &self,
        run: &str,
        path: &SyncPath,
        durability: Durability,
    ) -> Result<SyncPath, ApplyError> {
        match self {
            Self::Local(endpoint) => endpoint.put_back(run, path, durability),
            Self::Sftp(endpoint) => endpoint.put_back(run, path),
            Self::Peer(endpoint) => endpoint.put_back(run, path, durability),
            #[cfg(feature = "simulation")]
            Self::Memory(_) => Err(ApplyError::Unsupported(path.clone())),
        }
    }

    pub fn keep_copy(
        &self,
        path: &SyncPath,
        expected: EntryVersion,
        from: &str,
        options: &PairOptions,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<SyncPath, ApplyError> {
        match self {
            Self::Local(endpoint) => endpoint.keep_copy(path, expected, from, options, control),
            Self::Sftp(endpoint) => endpoint.keep_copy(path, expected, from, options, control),
            Self::Peer(endpoint) => endpoint.keep_copy(path, expected, from, options),
            #[cfg(feature = "simulation")]
            Self::Memory(_) => Err(ApplyError::Unsupported(path.clone())),
        }
    }

    pub(crate) fn read_lock_file(&self, file: LockFile) -> Result<Option<Vec<u8>>, ScanError> {
        match self {
            Self::Local(endpoint) => endpoint.read_lock_file(file),
            Self::Sftp(endpoint) => endpoint.read_lock_file(file),
            Self::Peer(endpoint) => endpoint.read_lock_file(file),
            #[cfg(feature = "simulation")]
            Self::Memory(endpoint) => endpoint.read_lock_file(file),
        }
    }

    /// Write `file` unless it exists; whether it was written.
    pub(crate) fn create_lock_file(
        &self,
        file: LockFile,
        content: &[u8],
    ) -> Result<bool, ApplyError> {
        match self {
            Self::Local(endpoint) => endpoint.create_lock_file(file, content),
            Self::Sftp(endpoint) => endpoint.create_lock_file(file, content),
            Self::Peer(endpoint) => endpoint.create_lock_file(file, content),
            #[cfg(feature = "simulation")]
            Self::Memory(endpoint) => endpoint.create_lock_file(file, content),
        }
    }

    pub(crate) fn replace_lock_file(
        &self,
        file: LockFile,
        content: &[u8],
    ) -> Result<(), ApplyError> {
        match self {
            Self::Local(endpoint) => endpoint.replace_lock_file(file, content),
            Self::Sftp(endpoint) => endpoint.replace_lock_file(file, content),
            Self::Peer(endpoint) => endpoint.replace_lock_file(file, content),
            #[cfg(feature = "simulation")]
            Self::Memory(endpoint) => endpoint.replace_lock_file(file, content),
        }
    }

    pub(crate) fn remove_lock_file(&self, file: LockFile) -> Result<(), ApplyError> {
        match self {
            Self::Local(endpoint) => endpoint.remove_lock_file(file),
            Self::Sftp(endpoint) => endpoint.remove_lock_file(file),
            Self::Peer(endpoint) => endpoint.remove_lock_file(file),
            #[cfg(feature = "simulation")]
            Self::Memory(endpoint) => endpoint.remove_lock_file(file),
        }
    }

    pub(crate) fn promote_claim(&self) -> Result<(), ApplyError> {
        match self {
            Self::Local(endpoint) => endpoint.promote_claim(),
            Self::Sftp(endpoint) => endpoint.promote_claim(),
            Self::Peer(endpoint) => endpoint.promote_claim(),
            #[cfg(feature = "simulation")]
            Self::Memory(endpoint) => endpoint.promote_claim(),
        }
    }

    /// How two roots nest. Roots on different kinds of endpoint, or on
    /// different servers, never overlap.
    pub fn relation_to(&self, other: &Endpoint) -> Result<RootRelation, ScanError> {
        match (self, other) {
            (Self::Local(endpoint), Self::Local(other)) => endpoint.relation_to(other),
            (Self::Sftp(endpoint), Self::Sftp(other)) => Ok(endpoint.relation_to(other)),
            (Self::Peer(endpoint), Self::Peer(other)) => Ok(endpoint.relation_to(other)),
            #[cfg(feature = "simulation")]
            (Self::Memory(endpoint), Self::Memory(other)) if endpoint.same(other) => {
                Ok(RootRelation::Same)
            }
            _ => Ok(RootRelation::Disjoint),
        }
    }
}

/// Archive browsing works on disks, servers and other Macs only.
#[cfg(feature = "simulation")]
fn unsupported_in_memory() -> ScanError {
    ScanError::Io {
        path: None,
        source: std::io::ErrorKind::Unsupported.into(),
    }
}
