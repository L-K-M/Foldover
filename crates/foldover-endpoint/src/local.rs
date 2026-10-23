mod apply;
pub(crate) mod archive;
mod conflict;
pub(crate) mod filter;
mod foreign;
pub(crate) use foreign::Carried;
mod identity;
mod lock;
pub(crate) mod version;

use std::collections::BTreeMap;
use std::io;
use std::num::NonZeroUsize;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use foldover_fs::{
    DataResidency, DirectoryEntry, DirectoryScope, EntryMetadata, ExtendedMetadata, FileIdentity,
    FilesystemFeatures, HashError, Medium, MetadataReadError, NodeKind, Root, RootRelation,
};
#[cfg(target_os = "macos")]
use foldover_fs::{DeclaredSupport, FilesystemFeature};
pub use foldover_types::ScanControl;
#[cfg(target_os = "macos")]
use foldover_types::{CaseMatching, UnicodeMatching};
use foldover_types::{
    Digest, EntryVersion, InvalidSyncPath, MetadataPolicy, NameMatching, SyncPath,
};

use version::{Attribute, Observed, Payload, entry_version, metadata_version};

use crate::{ListedEntry, ListedKind, ObserveStep};

pub(crate) use apply::Archiving;
pub use apply::{ApplyError, Expected, RunScope};
pub use archive::{ArchivedItem, ArchivedRun, archive_name};
pub use filter::{Filter, InvalidFilter};

const INTERNAL_DIRECTORY: &[u8] = b".foldover";
pub(crate) const TEMPORARY_PREFIX: &[u8] = b".foldover-tmp-";
const FINDER_STATE: &[u8] = b".DS_Store";
/// Copies a solid-state disk or a share takes at once (PLAN.md §4.9).
const PARALLEL_COPIES: NonZeroUsize = NonZeroUsize::new(4).expect("four is not zero");

/// A held local root. Metadata inventory does not establish volume UUIDs,
/// content equality, metadata fidelity or readiness to execute a sync.
pub struct LocalEndpoint {
    root: Root,
    identity: FileIdentity,
    state: Option<ProtectedState>,
}

/// Where the agent's own state directory sits under this root. A root may
/// hold it — one rooted at the home directory does — but its entries are
/// never scanned, and operations that could reach or displace it fail, so
/// an agent can neither carry its records to a peer nor damage them.
struct ProtectedState {
    /// The state directory's path below the root; empty when the root is
    /// the state directory or inside it, protecting every entry.
    prefix: Vec<u8>,
    /// Held open, so spellings the prefix does not carry — bind mounts,
    /// case-folded names — still compare by directory identity.
    root: Root,
    /// Its directory identity, matching a scanned entry costs nothing.
    identity: FileIdentity,
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("Metadata scan failed at {path:?}: {source}")]
    Io {
        path: Option<SyncPath>,
        #[source]
        source: io::Error,
    },
    #[error("Invalid directory entry: {0}")]
    InvalidPath(#[from] InvalidSyncPath),
    #[error("Scan cancelled; partial results are not complete")]
    Cancelled,
    #[error("Entry changed during metadata scan: {0:?}")]
    Changed(Option<SyncPath>),
    #[error("Nested filesystem requires its own endpoint: {0:?}")]
    NestedFilesystem(SyncPath),
    #[error("Directory cycle during metadata scan: {0:?}")]
    DirectoryCycle(SyncPath),
    #[error("Unsupported entry type: {0:?}")]
    UnsupportedEntry(SyncPath),
    #[error("AppleDouble sidecar needs metadata classification before scanning: {0:?}")]
    UnclassifiedSidecar(SyncPath),
    #[error("Internal endpoint path is protected: {0:?}")]
    ProtectedPath(SyncPath),
    #[error("Dataless entry cannot form a complete scan: {0:?}")]
    Dataless(Option<SyncPath>),
    #[error("The folder's .foldover/id file is not a Foldover identity")]
    InvalidIdentity,
    #[error("The folder's .foldoverignore file is not UTF-8 text")]
    InvalidIgnoreFile,
    #[error("No archived run has that name")]
    UnknownArchiveRun,
    /// Nothing is at the root's path, e.g. its disk or share is not mounted.
    #[error("The folder is missing; connect its disk or share")]
    RootMissing,
    /// An SFTP server could not be reached or refused the login.
    #[error("Cannot reach the SFTP server: {0}")]
    Unreachable(String),
    /// A paired Mac could not be reached, or refused the folder.
    #[error("Cannot reach the other Mac's folder: {0}")]
    MacUnreachable(String),
    /// The other Mac's folder failed in a way this Mac has no kind for.
    #[error("The other Mac's folder failed: {0}")]
    Peer(String),

    #[error("Extended metadata is unavailable at {path:?}: {source}")]
    Metadata {
        path: Option<SyncPath>,
        #[source]
        source: MetadataReadError,
    },
}

impl ScanError {
    /// The system refused access, e.g. macOS privacy protection for
    /// Documents or an external volume, or file permissions.
    pub fn is_access_denied(&self) -> bool {
        matches!(self, Self::Io { source, .. } if source.kind() == io::ErrorKind::PermissionDenied)
    }
}

/// Exact path and observed attributes. A link target is data, never a scan root.
#[derive(Debug)]
pub struct LocalEntry {
    path: SyncPath,
    metadata: EntryMetadata,
    link_target: Option<Vec<u8>>,
}

impl LocalEntry {
    pub fn path(&self) -> &SyncPath {
        &self.path
    }

    pub fn metadata(&self) -> &EntryMetadata {
        &self.metadata
    }

    pub fn link_target(&self) -> Option<&[u8]> {
        self.link_target.as_deref()
    }
}

/// Returned only after traversal finishes. Even a completed metadata inventory
/// needs content/metadata reads, watchers and freshness checks before planning.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScanStats {
    entries: usize,
    skipped: usize,
    leftovers: Vec<SyncPath>,
    oversized: Vec<SyncPath>,
    old: Vec<SyncPath>,
    unavailable: Vec<SyncPath>,
}

impl ScanStats {
    pub fn entries(&self) -> usize {
        self.entries
    }

    pub fn skipped(&self) -> usize {
        self.skipped
    }

    /// Temporaries an interrupted write left behind. One may hold a displaced
    /// original, so they are archived, never deleted.
    pub fn leftovers(&self) -> &[SyncPath] {
        &self.leftovers
    }
}

/// What `._name` files are on a root's volume (PLAN.md §4.10).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Sidecars {
    /// The volume keeps extended attributes itself, so a `._` file is a
    /// copy of another volume's bookkeeping, and ambiguous: it stops the
    /// scan (ADR 0010).
    Unclassified,
    /// The volume has none, as exFAT and FAT: macOS keeps each file's
    /// extended attributes in its `._` sidecar and shows them as the
    /// file's own, moving and removing the sidecar with it. A scan skips
    /// the sidecars; the attributes travel with their files.
    Metadata,
}

impl Sidecars {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn of(native_attributes: foldover_fs::DeclaredSupport) -> Self {
        use foldover_fs::DeclaredSupport;
        match native_attributes {
            DeclaredSupport::Unsupported => Self::Metadata,
            DeclaredSupport::Supported | DeclaredSupport::NotReported => Self::Unclassified,
        }
    }
}

/// How long after its last modification an entry's stat stays untrusted.
/// Coarse timestamps (FAT, exFAT) cannot show an edit within the same tick.
const RACY_WINDOW: Duration = Duration::from_secs(3);
/// A version a stat proved unchanged is read again after half to all of
/// this, so an edit that kept size and modification time is found within
/// a week (PLAN.md §4.8). Each entry's share is spread by its stat, so a
/// large folder is not reread in one run.
const REREAD: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The exact stat a version was derived from. An identical later stat proves
/// the version unchanged without rereading data, outside the racy window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Fingerprint {
    key: Vec<u8>,
    size: u64,
    observed_at: SystemTime,
}

/// Persisted form: seconds and nanoseconds of `observed_at`, the size, then
/// the stat key.
const FINGERPRINT_HEADER_BYTES: usize = 8 + 4 + 8;

impl Fingerprint {
    fn new(metadata: &EntryMetadata, observed_at: SystemTime) -> Self {
        Self {
            key: metadata.stat_key(),
            size: metadata.size(),
            observed_at,
        }
    }

    fn proves_unchanged(&self, current: &EntryMetadata) -> bool {
        self.proves_unchanged_by(&current.stat_key(), current.modified())
    }

    /// Store another system's stat; incomplete keys cannot justify version reuse.
    pub(crate) fn from_key(key: Vec<u8>, size: u64, observed_at: SystemTime) -> Self {
        Self {
            key,
            size,
            observed_at,
        }
    }

    fn proves_unchanged_by(&self, key: &[u8], modified: SystemTime) -> bool {
        // Equal keys include equal mtimes, so `modified` is the current one.
        let settled = self
            .observed_at
            .duration_since(modified)
            .is_ok_and(|age| age >= RACY_WINDOW);
        settled && key == self.key && self.read_within(self.reread_after(), SystemTime::now())
    }

    /// How long after it was read this version must be read again:
    /// between half of `REREAD` and all of it, fixed by the stat.
    fn reread_after(&self) -> Duration {
        let hash = blake3::hash(&self.key);
        let spread = u64::from_le_bytes(hash.as_bytes()[..8].try_into().expect("8 bytes"));
        let half = (REREAD.as_secs() / 2).max(1);
        Duration::from_secs(half + spread % half)
    }

    /// Whether the version was read less than `limit` before `now`. A
    /// read in the future, as after a clock change, counts as recent.
    fn read_within(&self, limit: Duration, now: SystemTime) -> bool {
        now.duration_since(self.observed_at)
            .map_or(true, |age| age < limit)
    }

    fn to_bytes(&self) -> Vec<u8> {
        let since = self
            .observed_at
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let mut bytes = Vec::with_capacity(FINGERPRINT_HEADER_BYTES + self.key.len());
        bytes.extend_from_slice(&since.as_secs().to_le_bytes());
        bytes.extend_from_slice(&since.subsec_nanos().to_le_bytes());
        bytes.extend_from_slice(&self.size.to_le_bytes());
        bytes.extend_from_slice(&self.key);
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let seconds = u64::from_le_bytes(bytes.get(..8)?.try_into().ok()?);
        let nanoseconds = u32::from_le_bytes(bytes.get(8..12)?.try_into().ok()?);
        let size = u64::from_le_bytes(bytes.get(12..FINGERPRINT_HEADER_BYTES)?.try_into().ok()?);
        let observed_at = UNIX_EPOCH.checked_add(Duration::new(seconds, nanoseconds))?;
        Some(Self {
            key: bytes.get(FINGERPRINT_HEADER_BYTES..)?.to_vec(),
            size,
            observed_at,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedEntry {
    pub(crate) version: EntryVersion,
    pub(crate) fingerprint: Fingerprint,
}

impl ObservedEntry {
    pub fn version(&self) -> EntryVersion {
        self.version
    }

    /// Bytes a copy of this entry's data needs.
    pub fn size(&self) -> u64 {
        self.fingerprint.size
    }

    /// Opaque bytes for a snapshot store.
    pub fn fingerprint_bytes(&self) -> Vec<u8> {
        self.fingerprint.to_bytes()
    }

    #[cfg(test)]
    pub(crate) fn for_tests(version: EntryVersion, size: u64) -> Self {
        Self {
            version,
            fingerprint: Fingerprint {
                key: vec![1, 2, 3],
                size,
                observed_at: SystemTime::UNIX_EPOCH,
            },
        }
    }

    /// Rebuild a stored entry; `None` if the bytes are not a fingerprint.
    pub fn restore(version: EntryVersion, fingerprint: &[u8]) -> Option<Self> {
        Some(Self {
            version,
            fingerprint: Fingerprint::from_bytes(fingerprint)?,
        })
    }
}

/// A complete tree observation plus leftovers to recover before writing.
#[derive(Debug, Default)]
pub struct Observation {
    pub(crate) entries: BTreeMap<SyncPath, ObservedEntry>,
    pub(crate) leftovers: Vec<SyncPath>,
    pub(crate) oversized: Vec<SyncPath>,
    pub(crate) old: Vec<SyncPath>,
    pub(crate) unavailable: Vec<SyncPath>,
}

impl Observation {
    pub fn entries(&self) -> &BTreeMap<SyncPath, ObservedEntry> {
        &self.entries
    }

    pub fn leftovers(&self) -> &[SyncPath] {
        &self.leftovers
    }

    /// Files skipped for exceeding the filter's size limit.
    pub fn oversized(&self) -> &[SyncPath] {
        &self.oversized
    }

    /// Observed files modified before the filter's cutoff.
    pub fn old(&self) -> &[SyncPath] {
        &self.old
    }

    /// Special files and cloud-only entries, left unread. Both folders
    /// must skip these paths and their descendants, never infer deletion.
    pub fn unavailable(&self) -> &[SyncPath] {
        &self.unavailable
    }

    /// Report the entry at `from` under `to`: the same file, spelled as
    /// the pair knows it.
    pub fn respell(&mut self, from: &SyncPath, to: SyncPath) {
        if let Some(entry) = self.entries.remove(from) {
            self.entries.insert(to.clone(), entry);
        }
        for listed in self
            .oversized
            .iter_mut()
            .chain(&mut self.old)
            .chain(&mut self.unavailable)
        {
            if listed == from {
                *listed = to.clone();
            }
        }
    }

    /// Drop paths the other folder skipped, so both skip them.
    pub fn exclude(&mut self, paths: &[SyncPath]) {
        for path in paths {
            self.entries.remove(path);
        }
    }

    /// The versions the planner compares.
    pub fn versions(&self) -> BTreeMap<SyncPath, EntryVersion> {
        self.entries
            .iter()
            .map(|(path, entry)| (path.clone(), entry.version))
            .collect()
    }
}

struct Directory {
    root: Root,
    path: Option<SyncPath>,
    before: EntryMetadata,
    entries: Box<dyn Iterator<Item = io::Result<DirectoryEntry>>>,
}

impl Directory {
    fn open(root: Root, path: Option<SyncPath>) -> Result<Self, ScanError> {
        let before = at_scan(&path, root.root_metadata())?;
        reject_dataless(before.data_residency(), &path)?;
        let entries = Box::new(at_scan(&path, root.entries(DirectoryScope::Root))?);
        Ok(Self {
            root,
            path,
            before,
            entries,
        })
    }
}

impl LocalEndpoint {
    pub fn open(path: &Path) -> Result<Self, ScanError> {
        Self::open_protecting(path, None)
    }

    /// `open` that knows the agent's own state directory: when this root
    /// holds it, that subtree is protected — see `ProtectedState`.
    pub fn open_protecting(path: &Path, state: Option<&Path>) -> Result<Self, ScanError> {
        let root = match Root::open(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ScanError::RootMissing);
            }
            result => at(&None, result)?,
        };
        let identity = at(&None, root.root_metadata())?.identity();
        let state = protected_state(&root, path, state)?;
        Ok(Self {
            root,
            identity,
            state,
        })
    }

    /// `path` is the state directory or inside it.
    fn protects(&self, path: &SyncPath) -> bool {
        let Some(state) = &self.state else {
            return false;
        };
        if state.prefix.is_empty() {
            return true;
        }
        path.as_bytes()
            .strip_prefix(state.prefix.as_slice())
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(b"/"))
    }

    /// `path` holds the state directory inside it: replacing, renaming or
    /// removing it would take the agent's records too.
    fn shelters(&self, path: &SyncPath) -> bool {
        let Some(state) = &self.state else {
            return false;
        };
        !state.prefix.is_empty()
            && state
                .prefix
                .strip_prefix(path.as_bytes())
                .is_some_and(|rest| rest.starts_with(b"/"))
    }

    /// The entry is the state directory itself, under any spelling.
    fn is_state(&self, metadata: &EntryMetadata) -> bool {
        self.state
            .as_ref()
            .is_some_and(|state| metadata.identity() == state.identity)
    }

    /// Refuse to read, list or write the agent's own state directory.
    fn guarded(&self, path: &SyncPath) -> Result<(), ScanError> {
        if self.protects(path) {
            return Err(ScanError::ProtectedPath(path.clone()));
        }
        self.alias_guard(path, false)
    }

    /// `guarded`, and `path` must not be a directory holding the state
    /// inside it: a removal or replacement of one takes the agent down.
    fn guarded_subtree(&self, path: &SyncPath) -> Result<(), ScanError> {
        if self.shelters(path) || self.protects(path) {
            return Err(ScanError::ProtectedPath(path.clone()));
        }
        self.alias_guard(path, true)
    }

    /// The prefix only sees the canonical spelling; an alias — a bind
    /// mount, a firmlink, a case-folded name — resolves to the same
    /// directories. `path` and each ancestor are opened and compared by
    /// held-directory identity, which no spelling can evade.
    fn alias_guard(&self, path: &SyncPath, subtree: bool) -> Result<(), ScanError> {
        let Some(state) = &self.state else {
            return Ok(());
        };
        // Runs per guarded call — including every listed directory of a
        // holding root — opening and fstat'ing each ancestor. If scans of
        // large home folders regress, cache ancestor identities populated
        // during scan descent instead of re-walking here.
        let mut at_path = Some(path.clone());
        while let Some(dir) = at_path {
            // Fail-open is fine: the guarded operation resolves `path`
            // through this same `Root` (`open_dir_nofollow` per component),
            // so an ancestor that fails `open_directory` here (besides the
            // expected NotFound for not-yet-created leaves) fails the
            // operation's own traversal too.
            if let Ok(held) = self.root.open_directory(&dir) {
                match at(&Some(dir.clone()), held.relation_to(&state.root))? {
                    RootRelation::Same | RootRelation::Within => {
                        return Err(ScanError::ProtectedPath(path.clone()));
                    }
                    RootRelation::Contains if subtree => {
                        return Err(ScanError::ProtectedPath(path.clone()));
                    }
                    _ => {}
                }
            }
            at_path = dir.parent();
        }
        Ok(())
    }

    pub fn relation_to(&self, other: &Self) -> Result<RootRelation, ScanError> {
        at(&None, self.root.relation_to(&other.root))
    }

    /// Where the root's volume is mounted from when it is a network share,
    /// e.g. `smb://alice@nas.local/Photos`.
    pub fn network_source(&self) -> Result<Option<String>, ScanError> {
        at(&None, self.root.network_source())
    }

    /// How many copies the root's volume takes at once: a spinning disk
    /// loses speed to every extra stream, and one that does not say may
    /// be one.
    pub fn workers(&self) -> NonZeroUsize {
        match self.root.medium() {
            Ok(Medium::SolidState | Medium::Network) => PARALLEL_COPIES,
            Ok(Medium::Rotational | Medium::Unknown) | Err(_) => NonZeroUsize::MIN,
        }
    }

    /// Read the root's first entry: macOS privacy protection and file
    /// permissions refuse listings, not opening the folder.
    pub fn probe_access(&self) -> Result<(), ScanError> {
        let mut entries = at(&None, self.root.entries(DirectoryScope::Root))?;
        match entries.next() {
            Some(Err(source)) => Err(ScanError::Io { path: None, source }),
            Some(Ok(_)) | None => Ok(()),
        }
    }

    /// One directory's entries for a quick change check; `None` once the
    /// directory is gone or is no longer one.
    pub fn list(
        &self,
        directory: Option<&SyncPath>,
        filter: &Filter,
    ) -> Result<Option<Vec<ListedEntry>>, ScanError> {
        let path = directory.cloned();
        if let Some(directory) = directory {
            self.guarded(directory)?;
        }
        let scope = match directory {
            None => DirectoryScope::Root,
            Some(directory) => DirectoryScope::At(directory),
        };
        let entries = match self.root.entries(scope) {
            Ok(entries) => entries,
            Err(error) if gone(&error) => return Ok(None),
            Err(source) => return Err(ScanError::Io { path, source }),
        };
        let mut listed = Vec::new();
        for entry in entries {
            let entry = at_scan(&path, entry)?;
            if protected(entry.name()) {
                continue;
            }
            if let Ok(path) = child_path(directory, entry.name())
                && (self.protects(&path) || self.is_state(entry.metadata()))
            {
                continue;
            }
            let metadata = entry.metadata();
            let kind = match metadata.kind() {
                NodeKind::Directory => ListedKind::Directory,
                NodeKind::File | NodeKind::Symlink | NodeKind::Other => ListedKind::Other,
            };
            listed.extend(ListedEntry::kept(
                directory,
                filter,
                entry.name().to_vec(),
                kind,
                metadata.size(),
                Some(metadata.modified()),
            ));
        }
        Ok(Some(listed))
    }

    /// Bytes an unprivileged writer can still use on the root's volume.
    pub fn available_space(&self) -> Result<u64, ScanError> {
        at(&None, self.root.available_space())
    }

    /// How this root's filesystem compares names. macOS volumes ignore
    /// Unicode normalization, and most ignore case; a volume that does not
    /// say counts as ignoring case, which flags more clashes, never fewer.
    pub fn name_matching(&self) -> NameMatching {
        #[cfg(target_os = "macos")]
        {
            let case = match self
                .filesystem_features()
                .map(|features| features.support(FilesystemFeature::CaseSensitiveNames))
            {
                Ok(DeclaredSupport::Supported) => CaseMatching::Exact,
                _ => CaseMatching::Insensitive,
            };
            NameMatching::new(case, UnicodeMatching::Normalized)
        }
        #[cfg(not(target_os = "macos"))]
        NameMatching::EXACT
    }

    /// What `._` names hold on this root's volume. Only macOS keeps
    /// attributes in sidecars, and only there do volumes say whether they
    /// keep them natively.
    fn sidecars(&self) -> Sidecars {
        #[cfg(target_os = "macos")]
        if let Ok(features) = self.filesystem_features() {
            return Sidecars::of(features.support(FilesystemFeature::NativeXattrs));
        }
        Sidecars::Unclassified
    }

    pub fn filesystem_features(&self) -> Result<FilesystemFeatures, ScanError> {
        let before = at(&None, self.root.root_metadata())?;
        if before.identity() != self.identity {
            return Err(ScanError::Changed(None));
        }
        reject_dataless(before.data_residency(), &None)?;
        let features = at(&None, self.root.filesystem_features())?;
        if at(&None, self.root.root_metadata())? != before {
            return Err(ScanError::Changed(None));
        }
        Ok(features)
    }

    /// Hash only an observed regular entry. A digest does not include xattrs,
    /// ACLs or resource forks and cannot authorize an executable plan by itself.
    pub fn hash_file(
        &self,
        entry: &LocalEntry,
        control: &mut impl FnMut(u64) -> ScanControl,
    ) -> Result<Digest, ScanError> {
        self.validate_content_access(entry)?;
        self.root
            .hash_file(entry.path(), entry.metadata(), control)
            .map_err(|error| match error {
                HashError::Io(source) => ScanError::Io {
                    path: Some(entry.path.clone()),
                    source,
                },
                HashError::Changed => ScanError::Changed(Some(entry.path.clone())),
                HashError::NotRegular => ScanError::UnsupportedEntry(entry.path.clone()),
                HashError::Dataless => ScanError::Dataless(Some(entry.path.clone())),
                HashError::Cancelled => ScanError::Cancelled,
            })
    }

    /// Observe native extended metadata under the same namespace/device rules as
    /// data reads. Capabilities and naming policy still precede file versions.
    pub fn read_extended_metadata(
        &self,
        entry: &LocalEntry,
        control: &mut impl FnMut(u64) -> ScanControl,
    ) -> Result<ExtendedMetadata, ScanError> {
        self.validate_content_access(entry)?;
        self.root
            .read_extended_metadata(entry.path(), entry.metadata(), control)
            .map_err(|source| metadata_error(Some(entry.path.clone()), source))
    }

    pub fn read_root_extended_metadata(
        &self,
        control: &mut impl FnMut(u64) -> ScanControl,
    ) -> Result<(EntryMetadata, ExtendedMetadata), ScanError> {
        let observed = at_scan(&None, self.root.root_metadata())?;
        if observed.identity() != self.identity {
            return Err(ScanError::Changed(None));
        }
        let extended = self
            .root
            .read_root_extended_metadata(&observed, control)
            .map_err(|source| metadata_error(None, source))?;
        Ok((observed, extended))
    }

    fn validate_content_access(&self, entry: &LocalEntry) -> Result<(), ScanError> {
        if self.is_state(&entry.metadata) {
            return Err(ScanError::ProtectedPath(entry.path.clone()));
        }
        self.guarded(&entry.path)?;
        if entry.path.components().any(protected) {
            return Err(ScanError::ProtectedPath(entry.path.clone()));
        }
        if entry
            .path
            .components()
            .any(|component| component.starts_with(b"._"))
        {
            return Err(ScanError::UnclassifiedSidecar(entry.path.clone()));
        }
        if entry.metadata.identity().device() != self.identity.device() {
            return Err(ScanError::NestedFilesystem(entry.path.clone()));
        }
        Ok(())
    }

    /// Stream depth-first metadata rows without opening file data or buffering a
    /// whole directory. Enumeration order is filesystem-defined; plans must sort.
    /// The sink must discard partial rows when this returns an error. Cancellation
    /// is checked between rows; it does not interrupt a blocked filesystem call.
    pub fn scan_metadata(
        &self,
        sink: &mut impl FnMut(LocalEntry) -> ScanControl,
    ) -> Result<ScanStats, ScanError> {
        self.scan_filtered(&Filter::none(), sink)
    }

    /// `scan_metadata` without excluded entries; excluded folders are not
    /// entered at all.
    pub fn scan_filtered(
        &self,
        filter: &Filter,
        sink: &mut impl FnMut(LocalEntry) -> ScanControl,
    ) -> Result<ScanStats, ScanError> {
        let root = at_scan(&None, self.root.try_clone())?;
        if at_scan(&None, root.root_metadata())?.identity() != self.identity {
            return Err(ScanError::Changed(None));
        }
        let sidecars = self.sidecars();
        let mut directories = vec![Directory::open(root, None)?];
        let mut stats = ScanStats::default();

        while let Some(directory) = directories.last_mut() {
            let Some(entry) = directory.entries.next() else {
                if at_scan(&directory.path, directory.root.root_metadata())? != directory.before {
                    return Err(ScanError::Changed(directory.path.clone()));
                }
                let finished = directories.pop().expect("the current directory exists");
                if let Some(parent) = directories.last() {
                    let path = finished.path.as_ref().expect("a descendant has a path");
                    let name = SyncPath::try_from(path.file_name())?;
                    if at_scan(&finished.path, parent.root.metadata(&name))? != finished.before {
                        return Err(ScanError::Changed(finished.path));
                    }
                }
                continue;
            };
            let entry = at_scan(&directory.path, entry)?;

            if protected(entry.name()) {
                stats.skipped += 1;
                if temporary(entry.name()) {
                    stats
                        .leftovers
                        .push(child_path(directory.path.as_ref(), entry.name())?);
                }
                continue;
            }
            let path = child_path(directory.path.as_ref(), entry.name())?;
            if self.protects(&path) || self.is_state(entry.metadata()) {
                stats.skipped += 1;
                continue;
            }
            if filter.excludes(&path, entry.metadata().kind() == NodeKind::Directory) {
                stats.skipped += 1;
                continue;
            }
            if entry.metadata().kind() == NodeKind::File
                && filter.too_large(entry.metadata().size())
            {
                stats.skipped += 1;
                stats.oversized.push(path);
                continue;
            }
            if entry.metadata().kind() == NodeKind::File
                && filter.is_old(entry.metadata().modified())
            {
                stats.old.push(path.clone());
            }
            if entry.name().starts_with(b"._") {
                // Sidecars are files; a folder so named is someone's.
                let file = entry.metadata().kind() == NodeKind::File;
                if sidecars == Sidecars::Metadata && file {
                    stats.skipped += 1;
                    continue;
                }
                return Err(ScanError::UnclassifiedSidecar(path));
            }
            let at_path = Some(path.clone());
            let name = SyncPath::try_from(entry.name())?;
            let metadata = entry.metadata().clone();
            // An entry kept only in iCloud is not absence, and reading it
            // would download it (PLAN.md §4.10): it is set aside unread,
            // folders without their contents, for both folders to skip.
            // run.rs's `unavailable` treats everything below one as
            // unknown, never as absent.
            if metadata.data_residency() == DataResidency::Dataless {
                stats.skipped += 1;
                stats.unavailable.push(path);
                continue;
            }
            if metadata.identity().device() != self.identity.device() {
                return Err(ScanError::NestedFilesystem(path));
            }
            if metadata.kind() == NodeKind::Other {
                // Sockets, pipes and devices have no transferable file content.
                stats.skipped += 1;
                stats.unavailable.push(path);
                continue;
            }
            let link_target = if metadata.kind() == NodeKind::Symlink {
                Some(at_scan(&at_path, directory.root.read_link(&name))?)
            } else {
                None
            };
            if at_scan(&at_path, directory.root.metadata(&name))? != metadata {
                return Err(ScanError::Changed(at_path));
            }
            if sink(LocalEntry {
                path: path.clone(),
                metadata: metadata.clone(),
                link_target,
            }) == ScanControl::Cancel
            {
                return Err(ScanError::Cancelled);
            }
            stats.entries += 1;

            if metadata.kind() != NodeKind::Directory {
                continue;
            }
            let child = at_scan(&at_path, directory.root.open_directory(&name))?;
            let child = Directory::open(child, at_path.clone())?;
            if child.before != metadata {
                return Err(ScanError::Changed(at_path));
            }
            if directories
                .iter()
                .any(|ancestor| ancestor.before.identity() == child.before.identity())
            {
                return Err(ScanError::DirectoryCycle(path));
            }
            directories.push(child);
        }
        Ok(stats)
    }

    /// Versions for every entry below the root, ordered by path. A read
    /// error, a change since the scan or cancellation fails the whole
    /// observation; the planner never receives a partial tree.
    pub fn observe(
        &self,
        policy: MetadataPolicy,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<Observation, ScanError> {
        self.observe_from(policy, &Filter::none(), &BTreeMap::new(), control)
    }

    /// `observe`, reusing a previous observation's version for every entry
    /// whose stat is unchanged and settled, so unchanged data is not reread.
    /// `previous` must come from the same root under the same policy.
    pub fn observe_from(
        &self,
        policy: MetadataPolicy,
        filter: &Filter,
        previous: &BTreeMap<SyncPath, ObservedEntry>,
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
        previous: &BTreeMap<SyncPath, ObservedEntry>,
        control: &mut impl FnMut() -> ScanControl,
        progress: &mut impl FnMut(ObserveStep),
    ) -> Result<Observation, ScanError> {
        let mut entries = Vec::new();
        let stats = self.scan_filtered(filter, &mut |entry| {
            entries.push(entry);
            progress(ObserveStep::Listed);
            control()
        })?;

        let mut observation = Observation {
            entries: BTreeMap::new(),
            leftovers: stats.leftovers,
            oversized: stats.oversized,
            old: stats.old,
            unavailable: stats.unavailable,
        };
        for entry in entries {
            if control() == ScanControl::Cancel {
                return Err(ScanError::Cancelled);
            }
            if let Some(known) = previous.get(&entry.path)
                && known.fingerprint.proves_unchanged(&entry.metadata)
            {
                observation.entries.insert(entry.path, known.clone());
                progress(ObserveStep::Checked);
                continue;
            }
            let observed_at = SystemTime::now();
            let version = self.entry_version(&entry, policy, control)?;
            observation.entries.insert(
                entry.path,
                ObservedEntry {
                    version,
                    fingerprint: Fingerprint::new(&entry.metadata, observed_at),
                },
            );
            progress(ObserveStep::Checked);
        }
        Ok(observation)
    }

    /// One entry's content and tracked metadata. Every read is checked
    /// against the entry's scan observation.
    pub fn entry_version(
        &self,
        entry: &LocalEntry,
        policy: MetadataPolicy,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<EntryVersion, ScanError> {
        let mut checkpoint = |_| control();
        let kind = entry.metadata.kind();
        let extended = self.optional_extended_metadata(entry, &mut checkpoint)?;
        let payload = match kind {
            NodeKind::File => Payload::File(self.hash_file(entry, &mut checkpoint)?),
            NodeKind::Directory => Payload::Directory,
            NodeKind::Symlink => Payload::Symlink(
                entry
                    .link_target()
                    .ok_or_else(|| ScanError::UnsupportedEntry(entry.path.clone()))?,
            ),
            NodeKind::Other => return Err(ScanError::UnsupportedEntry(entry.path.clone())),
        };

        let observed = observed(&entry.metadata, extended.as_ref());
        Ok(entry_version(payload, &observed, policy))
    }

    /// Extended metadata, or `None` where the platform cannot report it.
    pub(crate) fn optional_extended_metadata(
        &self,
        entry: &LocalEntry,
        control: &mut impl FnMut(u64) -> ScanControl,
    ) -> Result<Option<ExtendedMetadata>, ScanError> {
        match self.read_extended_metadata(entry, control) {
            Ok(extended) => Ok(Some(extended)),
            // Linux cannot read link xattrs through a held descriptor (ADR 0016).
            Err(ScanError::Metadata {
                source: MetadataReadError::UnsupportedEntry,
                ..
            }) if entry.metadata.kind() == NodeKind::Symlink => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The entry at `path` now, under the inventory's protections; `None`
    /// when nothing is there.
    pub(crate) fn entry_at(&self, path: &SyncPath) -> Result<Option<LocalEntry>, ScanError> {
        let at_path = Some(path.clone());
        let metadata = match self.root.metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(ScanError::Io {
                    path: at_path,
                    source,
                });
            }
        };
        let mut entry = LocalEntry {
            path: path.clone(),
            metadata,
            link_target: None,
        };
        self.validate_content_access(&entry)?;
        reject_dataless(entry.metadata.data_residency(), &at_path)?;
        match entry.metadata.kind() {
            NodeKind::Other => return Err(ScanError::UnsupportedEntry(path.clone())),
            NodeKind::Symlink => {
                entry.link_target = Some(at(&at_path, self.root.read_link(path))?);
                if at(&at_path, self.root.metadata(path))? != entry.metadata {
                    return Err(ScanError::Changed(at_path));
                }
            }
            NodeKind::File | NodeKind::Directory => {}
        }
        Ok(Some(entry))
    }
}

/// The fields a version depends on, from one consistent observation.
pub(crate) fn observed<'a>(
    metadata: &'a EntryMetadata,
    extended: Option<&'a ExtendedMetadata>,
) -> Observed<'a> {
    Observed {
        kind: metadata.kind(),
        permissions: metadata.permissions(),
        modified: metadata.modified(),
        user_flags: metadata.user_flags(),
        attributes: extended.map(|extended| {
            extended
                .attributes()
                .iter()
                .map(|attribute| Attribute {
                    role: metadata.attribute_role(attribute.name()),
                    name: attribute.name(),
                    size: attribute.size(),
                    digest: attribute.digest(),
                })
                .collect()
        }),
        access_control: extended.map(ExtendedMetadata::access_control),
    }
}

pub(crate) fn metadata_error(path: Option<SyncPath>, source: MetadataReadError) -> ScanError {
    match source {
        MetadataReadError::Io(source) => ScanError::Io { path, source },
        MetadataReadError::Changed => ScanError::Changed(path),
        MetadataReadError::Dataless => ScanError::Dataless(path),
        MetadataReadError::Cancelled => ScanError::Cancelled,
        source => ScanError::Metadata { path, source },
    }
}

fn reject_dataless(residency: DataResidency, path: &Option<SyncPath>) -> Result<(), ScanError> {
    if residency == DataResidency::Dataless {
        return Err(ScanError::Dataless(path.clone()));
    }
    Ok(())
}

pub(crate) fn child_path(
    parent: Option<&SyncPath>,
    name: &[u8],
) -> Result<SyncPath, InvalidSyncPath> {
    let mut bytes = Vec::new();
    if let Some(parent) = parent {
        bytes.extend_from_slice(parent.as_bytes());
        bytes.push(b'/');
    }
    bytes.extend_from_slice(name);
    SyncPath::try_from(bytes)
}

/// Foldover's own names, which never sync and never count as changes.
pub(crate) fn is_internal_name(name: &[u8]) -> bool {
    protected(name)
}

/// The agent's state directory as this root sees it, for
/// `open_protecting`. `None` when the state directory is disjoint from the
/// root. Canonical paths spell components the way a scan descends through
/// them; the spelled prefix is then proved to resolve to the state
/// directory itself, so an aliased root cannot leave it reachable.
fn protected_state(
    root: &Root,
    root_path: &Path,
    state: Option<&Path>,
) -> Result<Option<ProtectedState>, ScanError> {
    let Some(state) = state else {
        return Ok(None);
    };
    // A missing state directory has nothing to protect. Only open roots
    // after the agent has created the state directory: an endpoint opened
    // while it is missing will not protect it once it appears.
    let held = match Root::open(state) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        result => at(&None, result)?,
    };
    let identity = at(&None, held.root_metadata())?.identity();
    let root_path = at(&None, root_path.canonicalize())?;
    let state_path = at(&None, state.canonicalize())?;
    let prefix = match state_path.strip_prefix(&root_path) {
        Ok(relative) => relative.as_os_str().as_encoded_bytes().to_vec(),
        // The root is the state directory or inside it: protect all of it.
        Err(_) if root_path.strip_prefix(&state_path).is_ok() => Vec::new(),
        Err(_) => return Ok(None),
    };
    if !prefix.is_empty() {
        // The canonical prefix must resolve to the held state directory;
        // an aliased root could otherwise leave it reachable.
        let path = SyncPath::try_from(prefix.as_slice())?;
        if at(&Some(path.clone()), root.metadata(&path))?.identity() != identity {
            // The root's spelling of the prefix no longer resolves to the
            // state directory we opened: the root was swapped under us.
            return Err(ScanError::Changed(Some(path)));
        }
    }
    Ok(Some(ProtectedState {
        prefix,
        root: held,
        identity,
    }))
}

pub(crate) fn protected(name: &[u8]) -> bool {
    // Reserve ASCII aliases too: another endpoint may be case-insensitive.
    name.eq_ignore_ascii_case(INTERNAL_DIRECTORY)
        || name.eq_ignore_ascii_case(FINDER_STATE)
        || temporary(name)
}

pub(crate) fn temporary(name: &[u8]) -> bool {
    name.get(..TEMPORARY_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(TEMPORARY_PREFIX))
}

/// The directory a listing asked for no longer exists as one.
fn gone(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

fn at<T>(path: &Option<SyncPath>, result: io::Result<T>) -> Result<T, ScanError> {
    result.map_err(|source| ScanError::Io {
        path: path.clone(),
        source,
    })
}

/// `at` inside a scan: a path that is gone means the tree moved under
/// the listing — a change to rescan, not an I/O failure.
fn at_scan<T>(path: &Option<SyncPath>, result: io::Result<T>) -> Result<T, ScanError> {
    at(path, result).map_err(|error| match error {
        ScanError::Io { ref source, .. } if gone(source) => ScanError::Changed(path.clone()),
        error => error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PLAN.md §4.8: an unchanged stat stands for a version only until it
    /// is half a week to a week old, so same-size, same-time edits are
    /// found within a week, and entries are not all reread at once.
    #[test]
    fn stats_prove_versions_for_at_most_a_week() {
        let now = SystemTime::now();
        let modified = now - Duration::from_secs(365 * 24 * 60 * 60);
        let read = |ago: Duration| Fingerprint::from_key(b"key".to_vec(), 1, now - ago);
        assert!(read(Duration::from_secs(60 * 60)).proves_unchanged_by(b"key", modified));
        assert!(!read(REREAD).proves_unchanged_by(b"key", modified));
        assert!(!read(Duration::from_secs(60 * 60)).proves_unchanged_by(b"other", modified));

        let limits: std::collections::BTreeSet<Duration> = (0..64_u32)
            .map(|key| Fingerprint::from_key(key.to_le_bytes().to_vec(), 1, now).reread_after())
            .collect();
        assert!(
            limits
                .iter()
                .all(|limit| *limit >= REREAD / 2 && *limit < REREAD)
        );
        assert!(limits.len() > 32, "rereads are spread");
    }
    #[cfg(target_os = "linux")]
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    #[test]
    fn dataless_roots_and_reads_are_refused() {
        for path in [
            None,
            Some(SyncPath::try_from(b"placeholder".as_slice()).unwrap()),
        ] {
            assert!(
                matches!(reject_dataless(DataResidency::Dataless, &path), Err(ScanError::Dataless(actual)) if actual == path)
            );
            assert!(reject_dataless(DataResidency::Materialized, &path).is_ok());
            assert!(reject_dataless(DataResidency::NotReported, &path).is_ok());
        }
    }

    #[test]
    fn hashes_observed_regular_files_but_never_follows_links() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("file"), b"abc").unwrap();
        symlink("/outside", directory.path().join("link")).unwrap();
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();
        let mut entries = Vec::new();
        endpoint
            .scan_metadata(&mut |entry| {
                entries.push(entry);
                ScanControl::Continue
            })
            .unwrap();
        let file = entries
            .iter()
            .find(|entry| entry.path.as_bytes() == b"file")
            .unwrap();
        let digest = endpoint
            .hash_file(file, &mut |_| ScanControl::Continue)
            .unwrap();
        let expected = [
            0x64, 0x37, 0xb3, 0xac, 0x38, 0x46, 0x51, 0x33, 0xff, 0xb6, 0x3b, 0x75, 0x27, 0x3a,
            0x8d, 0xb5, 0x48, 0xc5, 0x58, 0x46, 0x5d, 0x79, 0xdb, 0x03, 0xfd, 0x35, 0x9c, 0x6c,
            0xd5, 0xbd, 0x9d, 0x85,
        ];
        assert_eq!(digest.as_bytes(), &expected);
        let link = entries
            .iter()
            .find(|entry| entry.path.as_bytes() == b"link")
            .unwrap();
        assert!(matches!(
            endpoint.hash_file(link, &mut |_| ScanControl::Continue),
            Err(ScanError::UnsupportedEntry(_))
        ));
        fs::write(directory.path().join("file"), b"changed").unwrap();
        assert!(matches!(
            endpoint.hash_file(file, &mut |_| ScanControl::Continue),
            Err(ScanError::Changed(_))
        ));
    }

    #[test]
    fn content_access_keeps_internal_names_protected_independently_of_inventory() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join(".foldover")).unwrap();
        fs::write(directory.path().join(".foldover/private"), b"private").unwrap();
        fs::write(directory.path().join("._sidecar"), b"metadata").unwrap();
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();
        for bytes in [b".foldover/private".as_slice(), b"._sidecar"] {
            let path = SyncPath::try_from(bytes).unwrap();
            let entry = LocalEntry {
                metadata: endpoint.root.metadata(&path).unwrap(),
                path,
                link_target: None,
            };
            assert!(matches!(
                endpoint.hash_file(&entry, &mut |_| panic!("Protected reads must not start")),
                Err(ScanError::ProtectedPath(_) | ScanError::UnclassifiedSidecar(_))
            ));
            assert!(matches!(
                endpoint.read_extended_metadata(&entry, &mut |_| panic!(
                    "Protected reads must not start"
                )),
                Err(ScanError::ProtectedPath(_) | ScanError::UnclassifiedSidecar(_))
            ));
        }
    }

    #[test]
    fn extended_metadata_access_preserves_observations_and_cancellation() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("file"), b"data").unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        let mut entries = Vec::new();
        endpoint
            .scan_metadata(&mut |entry| {
                entries.push(entry);
                ScanControl::Continue
            })
            .unwrap();
        let entry = &entries[0];
        endpoint
            .read_extended_metadata(entry, &mut |_| ScanControl::Continue)
            .unwrap();
        let (root, _) = endpoint
            .read_root_extended_metadata(&mut |_| ScanControl::Continue)
            .unwrap();
        assert_eq!(root.identity(), endpoint.identity);
        assert!(matches!(
            endpoint.read_extended_metadata(entry, &mut |_| ScanControl::Cancel),
            Err(ScanError::Cancelled)
        ));
        fs::write(temporary.path().join("file"), b"changed").unwrap();
        assert!(matches!(
            endpoint.read_extended_metadata(entry, &mut |_| ScanControl::Continue),
            Err(ScanError::Changed(_))
        ));
    }

    #[test]
    fn feature_observations_keep_the_held_root_and_reject_changed_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let selected = temporary.path().join("selected");
        fs::create_dir(&selected).unwrap();
        let mut endpoint = LocalEndpoint::open(&selected).unwrap();
        let original = endpoint.filesystem_features().unwrap();
        fs::rename(&selected, temporary.path().join("moved")).unwrap();
        symlink("/unavailable", &selected).unwrap();
        assert_eq!(endpoint.filesystem_features().unwrap(), original);
        endpoint.identity = Root::open(temporary.path())
            .unwrap()
            .root_metadata()
            .unwrap()
            .identity();
        assert!(matches!(
            endpoint.filesystem_features(),
            Err(ScanError::Changed(None))
        ));
    }

    #[test]
    fn an_empty_root_completes_without_inventing_entries() {
        let temporary = tempfile::tempdir().unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        let stats = endpoint
            .scan_metadata(&mut |_| panic!("an empty root has no rows"))
            .unwrap();
        assert_eq!(stats.entries(), 0);
        assert_eq!(stats.skipped(), 0);
    }

    #[test]
    fn inventories_without_internal_paths_or_link_traversal() {
        let temporary = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), b"outside").unwrap();
        fs::create_dir_all(temporary.path().join("tree/empty")).unwrap();
        fs::create_dir_all(temporary.path().join(".foldover/archive")).unwrap();
        fs::write(temporary.path().join(".foldover/archive/old"), b"archive").unwrap();
        fs::create_dir_all(temporary.path().join("tree/.FOLDOVER")).unwrap();
        fs::write(temporary.path().join("tree/.FOLDOVER/id"), b"internal").unwrap();
        fs::write(temporary.path().join(".foldover-tmp-write"), b"staged").unwrap();
        fs::write(temporary.path().join("tree/.DS_Store"), b"finder").unwrap();
        fs::write(temporary.path().join("z-last"), b"last").unwrap();
        fs::write(temporary.path().join("tree/file"), b"content").unwrap();
        symlink(outside.path(), temporary.path().join("a-link")).unwrap();
        symlink("../missing", temporary.path().join("tree/dangling")).unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        let mut entries = Vec::new();
        let stats = endpoint
            .scan_metadata(&mut |entry| {
                entries.push(entry);
                ScanControl::Continue
            })
            .unwrap();
        entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        let paths = entries
            .iter()
            .map(|entry| entry.path().as_bytes())
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            [
                b"a-link".as_slice(),
                b"tree",
                b"tree/dangling",
                b"tree/empty",
                b"tree/file",
                b"z-last",
            ]
        );
        assert_eq!(stats.entries(), entries.len());
        assert_eq!(stats.skipped(), 4);
        assert_eq!(
            entries[0].link_target(),
            Some(outside.path().as_os_str().as_bytes())
        );
        assert_eq!(entries[2].link_target(), Some(b"../missing".as_slice()));
        assert_eq!(entries[4].metadata().kind(), NodeKind::File);
        assert_eq!(entries[4].metadata().size(), 7);
        assert_eq!(entries[4].link_target(), None);
        let mut repeated = Vec::new();
        assert_eq!(
            endpoint
                .scan_metadata(&mut |entry| {
                    repeated.push(entry.path);
                    ScanControl::Continue
                })
                .unwrap(),
            stats,
        );
        repeated.sort_unstable();
        assert_eq!(
            repeated.iter().map(SyncPath::as_bytes).collect::<Vec<_>>(),
            paths
        );
    }

    #[test]
    fn cancellation_never_reports_a_complete_inventory() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("a"), b"a").unwrap();
        fs::write(temporary.path().join("b"), b"b").unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        let mut count = 0;
        let result = endpoint.scan_metadata(&mut |_| {
            count += 1;
            ScanControl::Cancel
        });
        assert!(matches!(result, Err(ScanError::Cancelled)));
        assert_eq!(count, 1);
    }

    #[test]
    fn refuses_a_directory_changed_to_an_outside_link_after_its_row() {
        let temporary = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(temporary.path().join("directory")).unwrap();
        fs::write(outside.path().join("secret"), b"outside").unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        let mut paths = Vec::new();
        let result = endpoint.scan_metadata(&mut |entry| {
            paths.push(entry.path);
            fs::remove_dir(temporary.path().join("directory")).unwrap();
            symlink(outside.path(), temporary.path().join("directory")).unwrap();
            ScanControl::Continue
        });
        assert!(matches!(result, Err(ScanError::Changed(_))));
        assert_eq!(
            paths,
            [SyncPath::try_from(b"directory".as_slice()).unwrap()]
        );
    }

    #[test]
    fn disappearing_entries_are_errors_not_empty_trees() {
        let temporary = tempfile::tempdir().unwrap();
        fs::create_dir(temporary.path().join("directory")).unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        let result = endpoint.scan_metadata(&mut |_| {
            fs::remove_dir(temporary.path().join("directory")).unwrap();
            ScanControl::Continue
        });
        assert!(matches!(result, Err(ScanError::Changed(_))));
    }

    #[test]
    fn home_folder_scans_skip_sockets_and_fifos_without_reading_them() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        fs::create_dir_all(root.join(".config/op")).unwrap();
        let _socket = UnixListener::bind(root.join(".config/op/op-daemon.sock")).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(root.join("pipe"))
                .status()
                .unwrap()
                .success()
        );
        fs::write(root.join("notes.txt"), b"keep").unwrap();
        symlink(".config/op/op-daemon.sock", root.join("link")).unwrap();
        let endpoint = LocalEndpoint::open(root).unwrap();
        let path = |bytes: &[u8]| SyncPath::try_from(bytes).unwrap();
        let mut paths = Vec::new();
        let stats = endpoint
            .scan_metadata(&mut |entry| {
                paths.push(entry.path);
                ScanControl::Continue
            })
            .unwrap();
        paths.sort();
        assert_eq!(
            paths,
            [
                path(b".config"),
                path(b".config/op"),
                path(b"link"),
                path(b"notes.txt")
            ]
        );
        assert_eq!((stats.entries(), stats.skipped()), (4, 2));

        let observed = endpoint
            .observe(ALL, &mut || ScanControl::Continue)
            .unwrap();
        assert_eq!(
            observed.entries().keys().cloned().collect::<Vec<_>>(),
            paths
        );
        let mut unavailable = observed.unavailable().to_vec();
        unavailable.sort();
        assert_eq!(
            unavailable,
            [path(b".config/op/op-daemon.sock"), path(b"pipe")]
        );
    }

    #[test]
    fn sidecars_are_not_misclassified_as_ordinary_file_data() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("._file"), b"metadata").unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        assert!(matches!(
            endpoint.scan_metadata(&mut |_| panic!("sidecar must not be emitted")),
            Err(ScanError::UnclassifiedSidecar(_)),
        ));
    }

    #[test]
    fn detects_directory_changes_even_after_the_last_row() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("only"), b"one").unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        let result = endpoint.scan_metadata(&mut |_| {
            fs::write(temporary.path().join("new"), b"new").unwrap();
            ScanControl::Continue
        });
        assert!(matches!(result, Err(ScanError::Changed(None))));
    }

    #[test]
    fn held_endpoint_does_not_switch_to_a_replacement_root() {
        let temporary = tempfile::tempdir().unwrap();
        let selected = temporary.path().join("selected");
        fs::create_dir(&selected).unwrap();
        fs::write(selected.join("original"), b"one").unwrap();
        let endpoint = LocalEndpoint::open(&selected).unwrap();
        fs::rename(&selected, temporary.path().join("moved")).unwrap();
        fs::create_dir(&selected).unwrap();
        fs::write(selected.join("replacement"), b"two").unwrap();
        let mut paths = Vec::new();
        let stats = endpoint
            .scan_metadata(&mut |entry| {
                paths.push(entry.path);
                ScanControl::Continue
            })
            .unwrap();
        assert_eq!(stats.entries(), 1);
        assert_eq!(paths, [SyncPath::try_from(b"original".as_slice()).unwrap()]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn preserves_non_utf8_path_and_link_target_bytes() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join(OsStr::from_bytes(b"\xff")), b"file").unwrap();
        symlink(OsStr::from_bytes(b"../\xfe"), temporary.path().join("link")).unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        let mut entries = Vec::new();
        endpoint
            .scan_metadata(&mut |entry| {
                entries.push(entry);
                ScanControl::Continue
            })
            .unwrap();
        entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        assert_eq!(entries[0].link_target(), Some(b"../\xfe".as_slice()));
        assert_eq!(entries[1].path().as_bytes(), b"\xff");
    }

    const ALL: MetadataPolicy = MetadataPolicy::new(
        foldover_types::Propagation::Sync,
        foldover_types::Propagation::Sync,
        foldover_types::Propagation::Sync,
    );

    #[cfg(target_os = "linux")]
    const TEST_ATTRIBUTE: &str = "user.ch.lkmc.foldover.test";
    #[cfg(not(target_os = "linux"))]
    const TEST_ATTRIBUTE: &str = "ch.lkmc.foldover.test";

    #[test]
    fn observations_version_content_and_tracked_metadata_independently() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir(root.join("folder")).unwrap();
        fs::write(root.join("folder/file"), b"data").unwrap();
        symlink("folder/file", root.join("link")).unwrap();
        fs::create_dir(root.join(".foldover")).unwrap();
        fs::write(root.join(".foldover/private"), b"private").unwrap();
        let path = |bytes: &[u8]| SyncPath::try_from(bytes).unwrap();
        let (folder, file, link) = (path(b"folder"), path(b"folder/file"), path(b"link"));

        let endpoint = LocalEndpoint::open(root).unwrap();
        let observe = || {
            endpoint
                .observe(ALL, &mut || ScanControl::Continue)
                .unwrap()
                .versions()
        };
        let first = observe();
        assert_eq!(
            first.keys().cloned().collect::<Vec<_>>(),
            [folder.clone(), file.clone(), link.clone()]
        );
        assert_eq!(
            first[&folder].content(),
            foldover_types::EntryContent::Directory
        );
        assert!(matches!(
            first[&link].content(),
            foldover_types::EntryContent::Symlink(_)
        ));
        assert_eq!(observe(), first);

        let handle = fs::File::open(root.join("folder/file")).unwrap();
        rustix::fs::fsetxattr(
            &handle,
            TEST_ATTRIBUTE,
            b"tag",
            rustix::fs::XattrFlags::empty(),
        )
        .unwrap();
        let tagged = observe();
        assert_eq!(tagged[&file].content(), first[&file].content());
        assert_ne!(tagged[&file].metadata(), first[&file].metadata());

        fs::set_permissions(root.join("folder/file"), fs::Permissions::from_mode(0o600)).unwrap();
        let restricted = observe();
        assert_eq!(restricted[&file].content(), first[&file].content());
        assert_ne!(restricted[&file].metadata(), tagged[&file].metadata());

        // A new child moves the directory mtime, which versions ignore.
        fs::write(root.join("folder/other"), b"").unwrap();
        assert_eq!(observe()[&folder], first[&folder]);

        fs::write(root.join("folder/file"), b"edited").unwrap();
        assert_ne!(observe()[&file].content(), first[&file].content());

        let elsewhere = tempfile::tempdir().unwrap();
        fs::write(elsewhere.path().join("file"), b"edited").unwrap();
        let other = LocalEndpoint::open(elsewhere.path())
            .unwrap()
            .observe(ALL, &mut || ScanControl::Continue)
            .unwrap()
            .versions();
        assert_eq!(other[&path(b"file")].content(), observe()[&file].content());
    }

    #[test]
    fn cancelled_observations_return_no_versions() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("file"), b"data").unwrap();
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();
        let mut calls = 0;
        let result = endpoint.observe(ALL, &mut || {
            calls += 1;
            if calls > 1 {
                ScanControl::Cancel
            } else {
                ScanControl::Continue
            }
        });
        assert!(matches!(result, Err(ScanError::Cancelled)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_resource_forks_change_content_and_tags_change_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let file_path = directory.path().join("file");
        fs::write(&file_path, b"data").unwrap();
        let path = SyncPath::try_from(b"file".as_slice()).unwrap();
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();
        // APFS moves the mtime when a fork changes; leave times out so only
        // the fork's effect on content and metadata is compared.
        let policy = MetadataPolicy::new(
            foldover_types::Propagation::Ignore,
            foldover_types::Propagation::Sync,
            foldover_types::Propagation::Sync,
        );
        let observe = || {
            endpoint
                .observe(policy, &mut || ScanControl::Continue)
                .unwrap()
                .versions()[&path]
        };
        let plain = observe();
        let set = |name: &str, value: &[u8]| {
            let handle = fs::File::open(&file_path).unwrap();
            rustix::fs::fsetxattr(&handle, name, value, rustix::fs::XattrFlags::empty()).unwrap();
        };

        set("com.apple.ResourceFork", b"fork");
        let forked = observe();
        assert_ne!(forked.content(), plain.content());
        assert_eq!(forked.metadata(), plain.metadata());

        set("com.apple.ResourceFork", b"edited fork");
        assert_ne!(observe().content(), forked.content());

        set("com.apple.metadata:_kMDItemUserTags", b"tags");
        let tagged = observe();
        assert_ne!(tagged.metadata(), plain.metadata());
    }

    #[test]
    fn previous_observations_skip_settled_unchanged_entries_only() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let past = UNIX_EPOCH + Duration::from_secs(1_000_000);
        for (name, content) in [("settled", b"one".as_slice()), ("edited", b"two")] {
            fs::write(root.join(name), content).unwrap();
            fs::File::options()
                .write(true)
                .open(root.join(name))
                .unwrap()
                .set_modified(past)
                .unwrap();
        }
        fs::write(root.join("recent"), b"three").unwrap();
        let endpoint = LocalEndpoint::open(root).unwrap();
        let first = endpoint
            .observe(ALL, &mut || ScanControl::Continue)
            .unwrap();

        // Persisted fingerprints restore exactly.
        let restored: BTreeMap<_, _> = first
            .entries()
            .iter()
            .map(|(path, entry)| {
                let bytes = entry.fingerprint_bytes();
                (
                    path.clone(),
                    ObservedEntry::restore(entry.version(), &bytes).unwrap(),
                )
            })
            .collect();
        assert_eq!(&restored, first.entries());
        assert!(
            ObservedEntry::restore(
                first.versions()[&SyncPath::try_from(b"settled".as_slice()).unwrap()],
                b"short"
            )
            .is_none()
        );

        // A stale previous version shows whether it was reused.
        let stale = EntryVersion::new(
            foldover_types::EntryContent::File(Digest::from_bytes([9; Digest::BYTE_LEN])),
            Digest::from_bytes([9; Digest::BYTE_LEN]),
        );
        let previous: BTreeMap<_, _> = restored
            .iter()
            .map(|(path, entry)| {
                (
                    path.clone(),
                    ObservedEntry::restore(stale, &entry.fingerprint_bytes()).unwrap(),
                )
            })
            .collect();
        fs::write(root.join("edited"), b"TWO").unwrap();
        fs::File::options()
            .write(true)
            .open(root.join("edited"))
            .unwrap()
            .set_modified(past)
            .unwrap();
        let second = endpoint
            .observe_from(ALL, &Filter::none(), &previous, &mut || {
                ScanControl::Continue
            })
            .unwrap()
            .versions();
        let path = |name: &[u8]| SyncPath::try_from(name).unwrap();
        assert_eq!(second[&path(b"settled")], stale);
        assert_ne!(second[&path(b"edited")], stale);
        // Modified moments before its observation: never trusted.
        assert_ne!(second[&path(b"recent")], stale);
    }

    #[test]
    fn filtered_scans_skip_excluded_entries_and_folders() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir_all(root.join("build/deep")).unwrap();
        fs::write(root.join("build/deep/out.o"), b"").unwrap();
        fs::write(root.join("a.tmp"), b"").unwrap();
        fs::write(root.join("._sidecar"), b"").unwrap();
        fs::write(root.join("kept.txt"), b"").unwrap();
        let endpoint = LocalEndpoint::open(root).unwrap();
        let filter = Filter::new(["build/", "*.tmp", "._*"]).unwrap();
        let observed = endpoint
            .observe_from(ALL, &filter, &BTreeMap::new(), &mut || {
                ScanControl::Continue
            })
            .unwrap();
        assert_eq!(
            observed.versions().keys().collect::<Vec<_>>(),
            [&SyncPath::try_from(b"kept.txt".as_slice()).unwrap()]
        );
    }

    /// Only a volume that says it has no extended attributes keeps them
    /// in `._` sidecars; one that does not say stops the scan as before.
    #[test]
    fn sidecars_are_bookkeeping_only_where_attributes_are_not_native() {
        use foldover_fs::DeclaredSupport;
        assert_eq!(
            Sidecars::of(DeclaredSupport::Unsupported),
            Sidecars::Metadata
        );
        assert_eq!(
            Sidecars::of(DeclaredSupport::Supported),
            Sidecars::Unclassified
        );
        assert_eq!(
            Sidecars::of(DeclaredSupport::NotReported),
            Sidecars::Unclassified
        );
    }
}
