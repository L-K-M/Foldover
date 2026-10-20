//! A folder held in memory, for simulations (PLAN.md §5.2, ADR 0038):
//! fast enough for many thousands of seeded histories, and able to act
//! like the file systems Foldover meets. Quirks a folder can have:
//!
//! - names that ignore case, or that it stores decomposed (NFD), or
//!   composed (NFC) as Foldover writes to SFTP servers;
//! - modification times kept only to a coarse resolution, as on FAT;
//! - replacing a file in two steps (old out, new in) instead of one swap,
//!   as on plain SFTP servers;
//! - a size limit, so writes fail with the disk full.
//!
//! A fault can be injected from any write step on: every mutation after
//! it fails until the fault is cleared, as when the disk vanishes or the
//! agent dies mid-operation. Extended attributes, file identities and
//! change notifications are not emulated.
//!
//! Writes follow the local endpoint's order (ADR 0019), one fault point
//! per step:
//!
//! ```text
//! atomic replace:   new in, old to a temporary ─▶ temporary to the archive
//! two-step replace: new to a temporary ─▶ old to the archive ─▶ temporary in
//! ```

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use foldover_fs::WriteError;
use foldover_types::{
    CaseMatching, Digest, Durability, EndpointId, EntryVersion, MetadataPolicy, NameMatching,
    Propagation, ScanControl, SyncPath, UnicodeMatching,
};
use unicode_normalization::UnicodeNormalization;

use crate::lease::LockFile;
use crate::local::archive::{ARCHIVE_ROOT, archive_time};
use crate::local::filter::{IGNORE_FILE, parse_ignore_file};
use crate::local::version::{Observed, Payload, entry_version, metadata_version};
use crate::local::{Archiving, Fingerprint, TEMPORARY_PREFIX, protected, temporary};
use crate::{
    ApplyError, Expected, Filter, ListedEntry, ListedKind, NodeKind, Observation, ObserveStep,
    ObservedEntry, RunScope, ScanError,
};

const IDENTITY_FILE: &[u8] = b".foldover/id";
const DEFAULT_FILE_MODE: u32 = 0o644;
const DEFAULT_DIRECTORY_MODE: u32 = 0o755;
const PERMISSION_BITS: u32 = 0o7777;
const EXECUTE_BITS: u32 = 0o111;
/// Where every folder's clock starts: 2026-01-01T00:00:00Z.
const EPOCH_SECONDS: u64 = 1_767_225_600;
/// How far the clock moves per write step.
const STEP: Duration = Duration::from_millis(1);

/// How a folder's file system behaves.
#[derive(Clone, Copy, Debug)]
pub struct Quirks {
    pub names: NameMatching,
    pub storage: NameStorage,
    /// The finest modification time kept; `Duration::ZERO` keeps all.
    pub time_resolution: Duration,
    pub replace: Replace,
    /// Bytes of file data the folder holds at most.
    pub capacity: Option<u64>,
}

impl Default for Quirks {
    fn default() -> Self {
        Self {
            names: NameMatching::EXACT,
            storage: NameStorage::AsGiven,
            time_resolution: Duration::ZERO,
            replace: Replace::Atomic,
            capacity: None,
        }
    }
}

/// How a folder stores the names it is given.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NameStorage {
    AsGiven,
    /// Unicode decomposed, as HFS+ does.
    Decomposed,
    /// Unicode composed, as Foldover writes to SFTP servers.
    Composed,
}

/// How a folder replaces an existing file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Replace {
    /// One swap; the old file is never missing.
    Atomic,
    /// The old file leaves before the new one arrives.
    TwoStep,
}

/// One folder in memory. Clones share the folder, so a simulation edits
/// what the endpoint syncs.
#[derive(Clone, Debug)]
pub struct MemoryEndpoint {
    folder: Arc<Mutex<Folder>>,
}

#[derive(Debug)]
struct Folder {
    /// Entries by folded path, each with the path as stored.
    entries: BTreeMap<Vec<u8>, Stored>,
    quirks: Quirks,
    clock: SystemTime,
    steps: u64,
    /// Steps from this count on fail.
    fault: Option<u64>,
    temporaries: u64,
}

#[derive(Clone, Debug)]
struct Stored {
    path: Vec<u8>,
    node: Node,
}

#[derive(Clone, Debug)]
enum Node {
    File {
        data: Arc<Vec<u8>>,
        mode: u32,
        modified: SystemTime,
    },
    Directory {
        mode: u32,
        modified: SystemTime,
    },
    Symlink {
        target: Vec<u8>,
        modified: SystemTime,
    },
}

impl Node {
    fn kind(&self) -> NodeKind {
        match self {
            Self::File { .. } => NodeKind::File,
            Self::Directory { .. } => NodeKind::Directory,
            Self::Symlink { .. } => NodeKind::Symlink,
        }
    }

    fn size(&self) -> u64 {
        match self {
            Self::File { data, .. } => data.len() as u64,
            Self::Directory { .. } | Self::Symlink { .. } => 0,
        }
    }

    fn mode(&self) -> u32 {
        match self {
            Self::File { mode, .. } | Self::Directory { mode, .. } => *mode,
            Self::Symlink { .. } => 0o777,
        }
    }

    fn modified(&self) -> SystemTime {
        match self {
            Self::File { modified, .. }
            | Self::Directory { modified, .. }
            | Self::Symlink { modified, .. } => *modified,
        }
    }

    fn version(&self, policy: MetadataPolicy) -> EntryVersion {
        let observed = Observed {
            kind: self.kind(),
            permissions: self.mode(),
            modified: self.modified(),
            user_flags: None,
            attributes: Some(Vec::new()),
            access_control: None,
        };
        match self {
            Self::File { data, .. } => entry_version(
                Payload::File(Digest::from_bytes(*blake3::hash(data).as_bytes())),
                &observed,
                policy,
            ),
            Self::Directory { .. } => entry_version(Payload::Directory, &observed, policy),
            Self::Symlink { target, .. } => {
                entry_version(Payload::Symlink(target), &observed, policy)
            }
        }
    }
}

impl MemoryEndpoint {
    pub fn new(quirks: Quirks) -> Self {
        Self {
            folder: Arc::new(Mutex::new(Folder {
                entries: BTreeMap::new(),
                quirks,
                clock: UNIX_EPOCH + Duration::from_secs(EPOCH_SECONDS),
                steps: 0,
                fault: None,
                temporaries: 0,
            })),
        }
    }

    fn folder(&self) -> MutexGuard<'_, Folder> {
        self.folder.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Fail every write step from the `after`th from now on.
    pub fn inject_fault(&self, after: u64) {
        let mut folder = self.folder();
        folder.fault = Some(folder.steps + after);
    }

    pub fn clear_fault(&self) {
        self.folder().fault = None;
    }

    /// Change the size limit, e.g. to free space after a disk-full test.
    pub fn set_capacity(&self, capacity: Option<u64>) {
        self.folder().quirks.capacity = capacity;
    }

    /// Write a file as a person would, creating its folders. Writing to a
    /// file that exists under another spelling the folder takes for the
    /// same name opens that file, which keeps its name.
    pub fn put_file(&self, path: &[u8], data: &[u8]) -> io::Result<()> {
        let mut folder = self.folder();
        folder.tick();
        let (parent, _) = split(path);
        if let Some(parent) = parent {
            folder.create_parents(parent)?;
        }
        let modified = folder.rounded(folder.clock);
        let node = Node::File {
            data: Arc::new(data.to_vec()),
            mode: DEFAULT_FILE_MODE,
            modified,
        };
        let key = folder.key(path);
        // Writing into an existing file keeps its mode, as a real one does.
        if let Some(stored) = folder.entries.get_mut(&key)
            && let Node::File { mode, .. } = stored.node
        {
            stored.node = match node {
                Node::File { data, modified, .. } => Node::File {
                    data,
                    mode,
                    modified,
                },
                other => other,
            };
            return Ok(());
        }
        folder.put(path, node)
    }

    /// Make a symbolic link as a person would, creating its folders.
    pub fn put_link(&self, path: &[u8], target: &[u8]) -> io::Result<()> {
        let mut folder = self.folder();
        folder.tick();
        let (parent, _) = split(path);
        if let Some(parent) = parent {
            folder.create_parents(parent)?;
        }
        let modified = folder.rounded(folder.clock);
        folder.put(
            path,
            Node::Symlink {
                target: target.to_vec(),
                modified,
            },
        )
    }

    /// Delete a file or link as a person would; absent paths are fine.
    pub fn delete_file(&self, path: &[u8]) {
        let mut folder = self.folder();
        folder.tick();
        let key = folder.key(path);
        if matches!(
            folder.entries.get(&key).map(|stored| &stored.node),
            Some(Node::File { .. } | Node::Symlink { .. })
        ) {
            folder.entries.remove(&key);
        }
    }

    /// Move a file as a person would, unless `to` is taken.
    pub fn move_file(&self, from: &[u8], to: &[u8]) -> io::Result<()> {
        let mut folder = self.folder();
        folder.tick();
        let from_key = folder.key(from);
        let Some(stored) = folder.entries.get(&from_key).cloned() else {
            return Ok(());
        };
        if !matches!(stored.node, Node::File { .. }) || folder.get(to).is_some() {
            return Ok(());
        }
        let (parent, _) = split(to);
        if let Some(parent) = parent {
            folder.create_parents(parent)?;
        }
        // In place first: a failed move leaves the file where it was.
        folder.put(to, stored.node)?;
        folder.entries.remove(&from_key);
        Ok(())
    }

    /// Every file outside Foldover's own names, by stored path.
    pub fn user_files(&self) -> BTreeMap<Vec<u8>, Vec<u8>> {
        self.folder()
            .entries
            .values()
            .filter(|stored| {
                stored
                    .path
                    .split(|byte| *byte == b'/')
                    .all(|name| !protected(name))
            })
            .filter_map(|stored| match &stored.node {
                Node::File { data, .. } => Some((stored.path.clone(), data.to_vec())),
                Node::Directory { .. } | Node::Symlink { .. } => None,
            })
            .collect()
    }

    /// The content of every file in the archive, from any run.
    pub fn archived_files(&self) -> Vec<Vec<u8>> {
        let mut prefix = ARCHIVE_ROOT.to_vec();
        prefix.push(b'/');
        self.folder()
            .entries
            .values()
            .filter(|stored| stored.path.starts_with(&prefix))
            .filter_map(|stored| match &stored.node {
                Node::File { data, .. } => Some(data.to_vec()),
                Node::Directory { .. } | Node::Symlink { .. } => None,
            })
            .collect()
    }

    /// The content of every temporary an interrupted write left behind;
    /// one may hold a displaced original until a run archives it.
    pub fn leftover_files(&self) -> Vec<Vec<u8>> {
        self.folder()
            .entries
            .values()
            .filter(|stored| !internal(&stored.path) && temporary(split(&stored.path).1))
            .filter_map(|stored| match &stored.node {
                Node::File { data, .. } => Some(data.to_vec()),
                Node::Directory { .. } | Node::Symlink { .. } => None,
            })
            .collect()
    }

    pub(crate) fn identity(&self) -> Result<Option<EndpointId>, ScanError> {
        let folder = self.folder();
        let Some(Node::File { data, .. }) = folder.get(IDENTITY_FILE).map(|stored| &stored.node)
        else {
            return Ok(None);
        };
        let text = data.strip_suffix(b"\n").unwrap_or(data);
        EndpointId::from_hex(text)
            .map(Some)
            .ok_or(ScanError::InvalidIdentity)
    }

    pub(crate) fn ensure_identity(
        &self,
        _durability: Durability,
    ) -> Result<EndpointId, ApplyError> {
        if let Some(identity) = self.identity()? {
            return Ok(identity);
        }
        let mut folder = self.folder();
        let identity = EndpointId::from_bytes(*uuid::Uuid::new_v4().as_bytes());
        folder
            .private_file(IDENTITY_FILE, format!("{identity}\n").into_bytes())
            .map_err(|source| write(IDENTITY_FILE, source))?;
        Ok(identity)
    }

    pub(crate) fn ignore_rules(&self) -> Result<Vec<String>, ScanError> {
        match self.folder().get(IGNORE_FILE).map(|stored| &stored.node) {
            Some(Node::File { data, .. }) => parse_ignore_file(data.to_vec()),
            _ => Ok(Vec::new()),
        }
    }

    pub(crate) fn name_matching(&self) -> NameMatching {
        self.folder().quirks.names
    }

    pub(crate) fn available_space(&self) -> u64 {
        let folder = self.folder();
        folder
            .quirks
            .capacity
            .map_or(u64::MAX, |capacity| capacity.saturating_sub(folder.used()))
    }

    /// Every entry outside Foldover's own names that `filter` keeps, with
    /// temporaries an interrupted write left behind reported apart.
    pub(crate) fn observe(
        &self,
        policy: MetadataPolicy,
        filter: &Filter,
        control: &mut impl FnMut() -> ScanControl,
        progress: &mut impl FnMut(ObserveStep),
    ) -> Result<Observation, ScanError> {
        let folder = self.folder();
        let mut observation = Observation::default();
        let mut skipped: Vec<Vec<u8>> = Vec::new();
        for stored in folder.entries.values() {
            if control() == ScanControl::Cancel {
                return Err(ScanError::Cancelled);
            }
            if internal(&stored.path) {
                continue;
            }
            let path = SyncPath::try_from(stored.path.clone())?;
            if skipped.iter().any(|folder| inside(&stored.path, folder)) {
                continue;
            }
            if temporary(split(&stored.path).1) {
                observation.leftovers.push(path);
                continue;
            }
            let directory = matches!(stored.node, Node::Directory { .. });
            if filter.excludes(&path, directory) {
                if directory {
                    skipped.push(stored.path.clone());
                }
                continue;
            }
            if let Node::File { data, modified, .. } = &stored.node {
                if filter.too_large(data.len() as u64) {
                    observation.oversized.push(path);
                    continue;
                }
                if filter.is_old(*modified) {
                    observation.old.push(path.clone());
                }
            }
            progress(ObserveStep::Listed);
            let entry = ObservedEntry {
                version: stored.node.version(policy),
                // Versions are always recomputed, so a fingerprint never
                // has to prove anything.
                fingerprint: Fingerprint::from_key(Vec::new(), stored.node.size(), UNIX_EPOCH),
            };
            progress(ObserveStep::Checked);
            observation.entries.insert(path, entry);
        }
        Ok(observation)
    }

    /// One directory's entries the filter keeps, for a quick change check;
    /// `None` once it is gone.
    pub(crate) fn list(
        &self,
        directory: Option<&SyncPath>,
        filter: &Filter,
    ) -> Result<Option<Vec<ListedEntry>>, ScanError> {
        let folder = self.folder();
        let parent = match directory {
            None => None,
            Some(directory) => match folder.get(directory.as_bytes()) {
                Some(Stored {
                    node: Node::Directory { .. },
                    path,
                }) => Some(path.clone()),
                _ => return Ok(None),
            },
        };
        let listed = folder
            .entries
            .values()
            .filter(|stored| split(&stored.path).0 == parent.as_deref())
            .filter(|stored| !protected(split(&stored.path).1))
            .filter_map(|stored| {
                let kind = match stored.node {
                    Node::Directory { .. } => ListedKind::Directory,
                    Node::File { .. } | Node::Symlink { .. } => ListedKind::Other,
                };
                ListedEntry::kept(
                    directory,
                    filter,
                    split(&stored.path).1.to_vec(),
                    kind,
                    stored.node.size(),
                    Some(stored.node.modified()),
                )
            })
            .collect();
        Ok(Some(listed))
    }

    /// Prove `path` still holds `expected`; the stored entry if so.
    fn verified(
        &self,
        path: &SyncPath,
        expected: &ObservedEntry,
        policy: MetadataPolicy,
    ) -> Result<Stored, ApplyError> {
        let folder = self.folder();
        let stored = folder
            .get(path.as_bytes())
            .filter(|stored| stored.node.version(policy) == expected.version)
            .ok_or_else(|| ApplyError::DestinationChanged(path.clone()))?;
        Ok(stored.clone())
    }

    pub(crate) fn copy_from(
        &self,
        source: &MemoryEndpoint,
        path: &SyncPath,
        from: &ObservedEntry,
        current: Expected<'_>,
        run: &RunScope,
    ) -> Result<EntryVersion, ApplyError> {
        let options = run.options();
        let policy = options.metadata();
        let source_node = source
            .verified(path, from, policy)
            .map_err(|_| ApplyError::SourceChanged(path.clone()))?
            .node;
        let mut folder = self.folder();
        let destination = folder.get(path.as_bytes()).cloned();
        match (current, &destination) {
            (Expected::Absent, None) => {}
            (Expected::Present(current), Some(stored))
                if stored.node.version(policy) == current.version => {}
            _ => return Err(ApplyError::DestinationChanged(path.clone())),
        }

        let now = folder.clock;
        let modified = match policy.modification_times() {
            Propagation::Sync => source_node.modified(),
            Propagation::Ignore => now,
        };
        let modified = folder.rounded(modified);
        let node = match &source_node {
            Node::Directory { mode, .. } => {
                if destination.is_some() {
                    return Err(ApplyError::Unsupported(path.clone()));
                }
                let mode = match policy.permissions() {
                    Propagation::Sync => mode & PERMISSION_BITS,
                    Propagation::Ignore => DEFAULT_DIRECTORY_MODE,
                };
                folder
                    .step()
                    .and_then(|()| {
                        folder.create(path.as_bytes(), Node::Directory { mode, modified })
                    })
                    .map_err(|source| write(path.as_bytes(), source))?;
                return folder.version_at(path, policy);
            }
            Node::File { data, mode, .. } => {
                let mode = match policy.permissions() {
                    Propagation::Sync => mode & PERMISSION_BITS,
                    Propagation::Ignore if mode & EXECUTE_BITS != 0 => {
                        DEFAULT_FILE_MODE | ((DEFAULT_FILE_MODE & 0o444) >> 2)
                    }
                    Propagation::Ignore => DEFAULT_FILE_MODE,
                };
                folder
                    .fits(data.len() as u64)
                    .map_err(|source| write(path.as_bytes(), source))?;
                Node::File {
                    data: Arc::clone(data),
                    mode,
                    modified,
                }
            }
            Node::Symlink { target, .. } => Node::Symlink {
                target: target.clone(),
                modified,
            },
        };
        let archive = archived(run, path)?;
        folder
            .place(
                path.as_bytes(),
                node,
                destination.is_some(),
                archive.as_ref().map(SyncPath::as_bytes),
            )
            .map_err(|source| write(path.as_bytes(), source))?;
        folder.version_at(path, policy)
    }

    pub(crate) fn update_metadata_from(
        &self,
        source: &MemoryEndpoint,
        path: &SyncPath,
        from: &ObservedEntry,
        current: &ObservedEntry,
        run: &RunScope,
    ) -> Result<EntryVersion, ApplyError> {
        if from.version.content() != current.version.content() {
            return Err(ApplyError::Unsupported(path.clone()));
        }
        let policy = run.options().metadata();
        let source_node = source
            .verified(path, from, policy)
            .map_err(|_| ApplyError::SourceChanged(path.clone()))?
            .node;
        self.verified(path, current, policy)?;
        let mut folder = self.folder();
        let key = folder.key(path.as_bytes());
        folder
            .step()
            .map_err(|source| write(path.as_bytes(), source))?;
        let rounded = folder.rounded(source_node.modified());
        let stored = folder
            .entries
            .get_mut(&key)
            .ok_or_else(|| ApplyError::DestinationChanged(path.clone()))?;
        match &mut stored.node {
            Node::File { mode, modified, .. } => {
                if policy.permissions() == Propagation::Sync {
                    *mode = source_node.mode() & PERMISSION_BITS;
                } else if source_node.mode() & EXECUTE_BITS != 0 {
                    *mode |= (*mode & 0o444) >> 2;
                } else {
                    *mode &= !EXECUTE_BITS;
                }
                if policy.modification_times() == Propagation::Sync {
                    *modified = rounded;
                }
            }
            Node::Directory { mode, .. } => {
                if policy.permissions() == Propagation::Sync {
                    *mode = source_node.mode() & PERMISSION_BITS;
                }
            }
            Node::Symlink { .. } => return Ok(current.version),
        }
        Ok(metadata_version(
            current.version.content(),
            &observed(&stored.node),
            policy,
        ))
    }

    /// Move the file or link at `from`, still `current`, to `to`, creating
    /// missing folders; nothing at `to` is replaced.
    pub(crate) fn move_entry(
        &self,
        from: &SyncPath,
        current: &ObservedEntry,
        to: &SyncPath,
        run: &RunScope,
    ) -> Result<(), ApplyError> {
        let stored = self
            .verified(from, current, run.options().metadata())
            .map_err(|_| ApplyError::DestinationChanged(from.clone()))?;
        if let Node::Directory { .. } = stored.node {
            return Err(ApplyError::Unsupported(from.clone()));
        }
        let mut folder = self.folder();
        if folder.get(to.as_bytes()).is_some() {
            return Err(ApplyError::DestinationChanged(to.clone()));
        }
        folder
            .step()
            .and_then(|()| folder.relocate(from.as_bytes(), to.as_bytes()))
            .map_err(|source| write(from.as_bytes(), source))
    }

    pub(crate) fn remove(
        &self,
        path: &SyncPath,
        current: &ObservedEntry,
        run: &RunScope,
    ) -> Result<(), ApplyError> {
        let stored = self.verified(path, current, run.options().metadata())?;
        let mut folder = self.folder();
        if let Node::Directory { .. } = stored.node {
            if folder.children(&stored.path).next().is_some() {
                return Err(ApplyError::NotEmpty(path.clone()));
            }
            folder
                .step()
                .map_err(|source| write(path.as_bytes(), source))?;
            let key = folder.key(path.as_bytes());
            folder.entries.remove(&key);
            return Ok(());
        }
        let archive = archived(run, path)?;
        folder
            .step()
            .and_then(|()| {
                folder.set_aside(path.as_bytes(), archive.as_ref().map(SyncPath::as_bytes))
            })
            .map_err(|source| write(path.as_bytes(), source))
    }

    pub(crate) fn recover_leftover(
        &self,
        leftover: &SyncPath,
        run: &RunScope,
    ) -> Result<(), ApplyError> {
        let archive = archived(run, leftover)?;
        let mut folder = self.folder();
        folder
            .step()
            .and_then(|()| {
                folder.set_aside(
                    leftover.as_bytes(),
                    archive.as_ref().map(SyncPath::as_bytes),
                )
            })
            .map_err(|source| write(leftover.as_bytes(), source))
    }

    pub(crate) fn prune_archive(
        &self,
        retention: Duration,
        now: SystemTime,
    ) -> Result<u64, ApplyError> {
        let mut folder = self.folder();
        let mut prefix = ARCHIVE_ROOT.to_vec();
        prefix.push(b'/');
        let expired: Vec<Vec<u8>> = folder
            .entries
            .values()
            .filter_map(|stored| {
                let rest = stored.path.strip_prefix(prefix.as_slice())?;
                (!rest.contains(&b'/')).then_some(stored.path.clone())
            })
            .filter(|run| {
                archive_time(split(run).1)
                    .and_then(|started| now.duration_since(started).ok())
                    .is_some_and(|age| age > retention)
            })
            .collect();
        for run in &expired {
            folder.step().map_err(|source| write(run, source))?;
            folder
                .entries
                .retain(|_, stored| stored.path != *run && !inside(&stored.path, run));
        }
        Ok(expired.len() as u64)
    }

    pub(crate) fn read_lock_file(&self, file: LockFile) -> Result<Option<Vec<u8>>, ScanError> {
        Ok(
            match self.folder().get(file.path()).map(|stored| &stored.node) {
                Some(Node::File { data, .. }) => Some(data.to_vec()),
                _ => None,
            },
        )
    }

    pub(crate) fn create_lock_file(
        &self,
        file: LockFile,
        content: &[u8],
    ) -> Result<bool, ApplyError> {
        let mut folder = self.folder();
        if folder.get(file.path()).is_some() {
            return Ok(false);
        }
        folder
            .private_file(file.path(), content.to_vec())
            .map_err(|source| write(file.path(), source))?;
        Ok(true)
    }

    pub(crate) fn replace_lock_file(
        &self,
        file: LockFile,
        content: &[u8],
    ) -> Result<(), ApplyError> {
        self.folder()
            .private_file(file.path(), content.to_vec())
            .map_err(|source| write(file.path(), source))
    }

    pub(crate) fn remove_lock_file(&self, file: LockFile) -> Result<(), ApplyError> {
        let mut folder = self.folder();
        folder.step().map_err(|source| write(file.path(), source))?;
        let key = folder.key(file.path());
        folder.entries.remove(&key);
        Ok(())
    }

    pub(crate) fn promote_claim(&self) -> Result<(), ApplyError> {
        let (claim, lock) = (LockFile::Claim.path(), LockFile::Lock.path());
        let mut folder = self.folder();
        folder.step().map_err(|source| write(lock, source))?;
        let key = folder.key(claim);
        let stored = folder
            .entries
            .remove(&key)
            .ok_or_else(|| write(claim, io::ErrorKind::NotFound.into()))?;
        folder
            .put(lock, stored.node)
            .map_err(|source| write(lock, source))
    }

    /// Whether both handles are one folder.
    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.folder, &other.folder)
    }
}

impl Folder {
    /// Advance the clock for one change.
    fn tick(&mut self) {
        self.clock += STEP;
    }

    /// One write step: fails once the injected fault is due.
    fn step(&mut self) -> io::Result<()> {
        self.tick();
        self.steps += 1;
        if self.fault.is_some_and(|from| self.steps > from) {
            return Err(io::Error::other("injected fault"));
        }
        Ok(())
    }

    fn rounded(&self, time: SystemTime) -> SystemTime {
        let resolution = self.quirks.time_resolution.as_nanos();
        if resolution == 0 {
            return time;
        }
        let since = time
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let kept = since - since % resolution;
        UNIX_EPOCH + Duration::from_nanos(u64::try_from(kept).unwrap_or(u64::MAX))
    }

    fn used(&self) -> u64 {
        self.entries.values().map(|stored| stored.node.size()).sum()
    }

    fn fits(&self, size: u64) -> io::Result<()> {
        match self.quirks.capacity {
            Some(capacity) if self.used().saturating_add(size) > capacity => {
                Err(io::Error::from(io::ErrorKind::StorageFull))
            }
            _ => Ok(()),
        }
    }

    /// The path as this folder compares it: each name folded the way the
    /// folder ignores differences.
    fn key(&self, path: &[u8]) -> Vec<u8> {
        path.split(|byte| *byte == b'/')
            .map(|name| fold(name, self.quirks.names))
            .collect::<Vec<_>>()
            .join(&b'/')
    }

    fn get(&self, path: &[u8]) -> Option<&Stored> {
        self.entries.get(&self.key(path))
    }

    /// Entries directly inside the stored folder `parent`.
    fn children<'a>(&'a self, parent: &'a [u8]) -> impl Iterator<Item = &'a Stored> + 'a {
        self.entries
            .values()
            .filter(move |stored| split(&stored.path).0 == Some(parent))
    }

    /// `path` as stored: the parent's stored path and the name as this
    /// folder keeps it.
    fn stored_path(&self, path: &[u8]) -> Vec<u8> {
        let (parent, name) = split(path);
        let name = match self.quirks.storage {
            NameStorage::AsGiven => name.to_vec(),
            NameStorage::Decomposed => match std::str::from_utf8(name) {
                Ok(text) => text.nfd().collect::<String>().into_bytes(),
                Err(_) => name.to_vec(),
            },
            NameStorage::Composed => match std::str::from_utf8(name) {
                Ok(text) => text.nfc().collect::<String>().into_bytes(),
                Err(_) => name.to_vec(),
            },
        };
        match parent {
            None => name,
            Some(parent) => {
                let mut stored = self
                    .get(parent)
                    .map_or_else(|| parent.to_vec(), |stored| stored.path.clone());
                stored.push(b'/');
                stored.extend_from_slice(&name);
                stored
            }
        }
    }

    /// Store `node` at `path`, replacing what is there under the same key.
    fn put(&mut self, path: &[u8], node: Node) -> io::Result<()> {
        let (parent, _) = split(path);
        if let Some(parent) = parent
            && !matches!(
                self.get(parent),
                Some(Stored {
                    node: Node::Directory { .. },
                    ..
                })
            )
        {
            return Err(io::ErrorKind::NotFound.into());
        }
        let stored = Stored {
            path: self.stored_path(path),
            node,
        };
        let key = self.key(path);
        self.entries.insert(key, stored);
        Ok(())
    }

    /// Store `node` at `path` unless something is there.
    fn create(&mut self, path: &[u8], node: Node) -> io::Result<()> {
        if self.get(path).is_some() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        self.put(path, node)
    }

    fn create_parents(&mut self, path: &[u8]) -> io::Result<()> {
        let mut prefix = Vec::new();
        for name in path.split(|byte| *byte == b'/') {
            if !prefix.is_empty() {
                prefix.push(b'/');
            }
            prefix.extend_from_slice(name);
            match self.get(&prefix).map(|stored| &stored.node) {
                Some(Node::Directory { .. }) => {}
                Some(_) => return Err(io::ErrorKind::NotADirectory.into()),
                None => {
                    let modified = self.clock;
                    self.put(
                        &prefix,
                        Node::Directory {
                            mode: DEFAULT_DIRECTORY_MODE,
                            modified,
                        },
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Move the file or link at `path` to `archive`, or delete it without
    /// one. Folders never come here: `remove` takes them only when empty.
    fn set_aside(&mut self, path: &[u8], archive: Option<&[u8]>) -> io::Result<()> {
        let Some(archive) = archive else {
            let key = self.key(path);
            return match self.entries.remove(&key) {
                Some(_) => Ok(()),
                None => Err(io::ErrorKind::NotFound.into()),
            };
        };
        self.relocate(path, archive)
    }

    /// Move the entry at `from`, and everything inside it, to `to`,
    /// creating `to`'s folders.
    fn relocate(&mut self, from: &[u8], to: &[u8]) -> io::Result<()> {
        // Where it goes must exist before it leaves where it is.
        if self.get(from).is_none() {
            return Err(io::ErrorKind::NotFound.into());
        }
        if let (Some(parent), _) = split(to) {
            self.create_parents(parent)?;
        }
        let key = self.key(from);
        let stored = self.entries.remove(&key).ok_or(io::ErrorKind::NotFound)?;
        let inner: Vec<Stored> = self
            .entries
            .values()
            .filter(|inner| inside(&inner.path, &stored.path))
            .cloned()
            .collect();
        let old = stored.path.clone();
        self.put(to, stored.node)?;
        let new = self
            .get(to)
            .map(|placed| placed.path.clone())
            .unwrap_or_default();
        for inner in inner {
            let key = self.key(&inner.path);
            self.entries.remove(&key);
            let mut path = new.clone();
            path.extend_from_slice(&inner.path[old.len()..]);
            self.put(&path, inner.node)?;
        }
        Ok(())
    }

    /// A temporary name beside `path`, as interrupted writes leave.
    fn temporary_beside(&mut self, path: &[u8]) -> Vec<u8> {
        self.temporaries += 1;
        let mut name = TEMPORARY_PREFIX.to_vec();
        name.extend_from_slice(format!("{:08x}", self.temporaries).as_bytes());
        match split(path).0 {
            None => name,
            Some(parent) => {
                let mut temporary = parent.to_vec();
                temporary.push(b'/');
                temporary.extend_from_slice(&name);
                temporary
            }
        }
    }

    /// Put `node` at `path`, archiving what it replaces at `archive`, or
    /// deleting it without one.
    fn place(
        &mut self,
        path: &[u8],
        node: Node,
        replaces: bool,
        archive: Option<&[u8]>,
    ) -> io::Result<()> {
        if !replaces {
            if self.quirks.replace == Replace::TwoStep {
                let temporary = self.temporary_beside(path);
                self.step()?;
                self.put(&temporary, node)?;
                self.step()?;
                return self.relocate(&temporary, path);
            }
            self.step()?;
            return self.create(path, node);
        }
        match self.quirks.replace {
            Replace::Atomic => {
                // The swap: the new entry in, the old one aside.
                let displaced = self.temporary_beside(path);
                self.step()?;
                self.relocate(path, &displaced)?;
                self.put(path, node)?;
                self.step()?;
                self.set_aside(&displaced, archive)
            }
            Replace::TwoStep => {
                let temporary = self.temporary_beside(path);
                self.step()?;
                self.put(&temporary, node)?;
                self.step()?;
                self.set_aside(path, archive)?;
                self.step()?;
                self.relocate(&temporary, path)
            }
        }
    }

    /// Write one of Foldover's own small files, whole.
    fn private_file(&mut self, path: &[u8], data: Vec<u8>) -> io::Result<()> {
        self.step()?;
        if let (Some(parent), _) = split(path) {
            self.create_parents(parent)?;
        }
        let modified = self.clock;
        self.put(
            path,
            Node::File {
                data: Arc::new(data),
                mode: 0o600,
                modified,
            },
        )
    }

    fn version_at(
        &self,
        path: &SyncPath,
        policy: MetadataPolicy,
    ) -> Result<EntryVersion, ApplyError> {
        self.get(path.as_bytes())
            .map(|stored| stored.node.version(policy))
            .ok_or_else(|| ApplyError::DestinationChanged(path.clone()))
    }
}

fn observed(node: &Node) -> Observed<'static> {
    Observed {
        kind: node.kind(),
        permissions: node.mode(),
        modified: node.modified(),
        user_flags: None,
        attributes: Some(Vec::new()),
        access_control: None,
    }
}

/// The parent's path, if any, and the last name.
fn split(path: &[u8]) -> (Option<&[u8]>, &[u8]) {
    match path.iter().rposition(|byte| *byte == b'/') {
        Some(slash) => (Some(&path[..slash]), &path[slash + 1..]),
        None => (None, path),
    }
}

/// Whether `path` is below the folder `folder`.
fn inside(path: &[u8], folder: &[u8]) -> bool {
    path.len() > folder.len() && path.starts_with(folder) && path[folder.len()] == b'/'
}

/// Inside Foldover's own folder, or any other protected name.
fn internal(path: &[u8]) -> bool {
    path.split(|byte| *byte == b'/')
        .any(|name| protected(name) && !temporary(name))
}

/// A name as `names` compares it, like the engine's name keys.
fn fold(name: &[u8], names: NameMatching) -> Vec<u8> {
    let insensitive = names.case() == CaseMatching::Insensitive;
    let Ok(text) = std::str::from_utf8(name) else {
        return if insensitive {
            name.to_ascii_lowercase()
        } else {
            name.to_vec()
        };
    };
    let text: String = match names.unicode() {
        UnicodeMatching::Normalized => text.nfc().collect(),
        UnicodeMatching::Exact => text.to_owned(),
    };
    if insensitive {
        text.to_lowercase().into_bytes()
    } else {
        text.into_bytes()
    }
}

fn write(path: &[u8], source: io::Error) -> ApplyError {
    match SyncPath::try_from(path.to_vec()) {
        Ok(path) => ApplyError::Write {
            path,
            source: WriteError::Io(source),
        },
        Err(error) => ApplyError::InvalidPath(error),
    }
}

/// Where the run archives `path` here; None where it keeps no archive.
fn archived(run: &RunScope, path: &SyncPath) -> Result<Option<SyncPath>, ApplyError> {
    match run.archiving() {
        Archiving::Kept => Ok(Some(run.archive_path(path)?)),
        Archiving::Off => Ok(None),
    }
}

#[cfg(test)]
mod tests;
