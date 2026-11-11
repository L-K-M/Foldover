mod apply;
pub(crate) mod archive;
mod catalog;
mod conflict;
pub(crate) mod filter;
mod foreign;
pub(crate) use foreign::Carried;
mod fingerprint_key;
mod identity;
mod lock;
mod observation_ownership;
pub(crate) mod version;

use std::collections::BTreeMap;
use std::io;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use foldover_fs::{
    DataResidency, DirectoryEntry, DirectoryScope, EntryMetadata, ExtendedMetadata, FileIdentity,
    FilesystemFeatures, HashError, Medium, MetadataCapture, MetadataReadError, NodeKind,
    ObserveFileError, PathScope, Root, RootRelation,
};
#[cfg(target_os = "macos")]
use foldover_fs::{DeclaredSupport, FilesystemFeature};
pub use foldover_types::ScanControl;
use foldover_types::{
    BaselineEntry, Digest, Effort, EntryVersion, InvalidSyncPath, MetadataPolicy, NameMatching,
    PathMap, PathMapBuilder, PathPrefixIndex, Side, SyncPath,
};
#[cfg(target_os = "macos")]
use foldover_types::{CaseMatching, UnicodeMatching};

use fingerprint_key::FingerprintKey;
use version::{Attribute, Observed, Payload, capture_for, entry_version, metadata_version};

use crate::observation_cache::{Cache, Comparison, Recycled};
use crate::observation_entries::ObservationEntries;
use crate::observation_reads::Reads;
use crate::observation_versions::{ObservationOrigin, ObservedVersion, VersionPoolBuilder};
use crate::{Bottleneck, ListedEntry, ListedKind, ObservationHints, ObserveStep};

pub(crate) use apply::Archiving;
pub use apply::{ApplyError, Expected, RunScope};
pub use archive::{ArchivedItem, ArchivedRun, Pruning, archive_name, run_name, run_owner};
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
    /// How hard observing this folder may work: `Full` until a run says.
    effort: Effort,
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
    #[error("AppleDouble sidecar cannot be classified safely: {0:?}")]
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

    /// Only a bounded subtree can be skipped. Root failures, cancellation,
    /// transport loss and protection failures still invalidate the observation.
    pub(crate) fn into_issue(self) -> Result<ScanIssue, Self> {
        let path = match &self {
            Self::Io {
                path: Some(path),
                source,
            } if !matches!(
                source.kind(),
                io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::NotConnected
                    | io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::TimedOut
            ) =>
            {
                path
            }
            Self::Changed(Some(path))
            | Self::Dataless(Some(path))
            | Self::Metadata {
                path: Some(path), ..
            }
            | Self::NestedFilesystem(path)
            | Self::DirectoryCycle(path)
            | Self::UnsupportedEntry(path)
            | Self::UnclassifiedSidecar(path) => path,
            _ => return Err(self),
        };
        Ok(ScanIssue {
            path: path.clone(),
            message: self.to_string(),
        })
    }
}

/// A path left unread, with its descendants, rather than treated as absent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanIssue {
    pub(crate) path: SyncPath,
    pub(crate) message: String,
}

impl ScanIssue {
    pub fn path(&self) -> &SyncPath {
        &self.path
    }
    pub fn message(&self) -> &str {
        &self.message
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

/// Returned only after traversal finishes. Unstable listings cannot establish
/// absence; even readable entries need content/metadata and freshness checks.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScanStats {
    entries: usize,
    skipped: usize,
    leftovers: Vec<SyncPath>,
    oversized: Vec<SyncPath>,
    old: Vec<SyncPath>,
    unavailable: Vec<SyncPath>,
    issues: Vec<ScanIssue>,
    unstable: Vec<Option<SyncPath>>,
}

impl ScanStats {
    /// Directories whose missing entries do not prove absence. `None` is root.
    pub fn unstable_directories(&self) -> &[Option<SyncPath>] {
        &self.unstable
    }

    /// These subtrees must be discarded even if the sink already saw rows.
    pub fn unavailable(&self) -> &[SyncPath] {
        &self.unavailable
    }

    pub fn issues(&self) -> &[ScanIssue] {
        &self.issues
    }

    fn skip_error(&mut self, error: ScanError) -> Result<(), ScanError> {
        let issue = error.into_issue()?;
        self.skipped += 1;
        self.unavailable.push(issue.path.clone());
        self.issues.push(issue);
        Ok(())
    }

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
    /// copy of another volume's bookkeeping, and ambiguous: leave it
    /// unchanged and report it.
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
    /// Lossless in-memory storage; persistence and reread jitter use the
    /// original stat bytes, including every zero and timestamp field.
    key: FingerprintKey,
    size: u64,
    observed_at: SystemTime,
}

/// Persisted form: seconds and nanoseconds of `observed_at`, the size, then
/// the stat key.
const FINGERPRINT_HEADER_BYTES: usize = 8 + 4 + 8;

impl Fingerprint {
    fn new(metadata: &EntryMetadata, observed_at: SystemTime) -> Self {
        let captured = metadata.stat_key();
        Self {
            // A realloc shrink may keep the capture's larger malloc size
            // class. Allocate the payload while that buffer is still held.
            key: FingerprintKey::new(captured.as_slice()),
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
            key: FingerprintKey::new(key.as_slice()),
            size,
            observed_at,
        }
    }

    /// Whether this fingerprint records the stat `key` of `size` bytes.
    pub(crate) fn same_stat(&self, key: &[u8], size: u64) -> bool {
        self.key.matches(key) && self.size == size
    }

    fn proves_unchanged_by(&self, key: &[u8], modified: SystemTime) -> bool {
        // Equal keys include equal mtimes, so `modified` is the current one.
        let settled = self
            .observed_at
            .duration_since(modified)
            .is_ok_and(|age| age >= RACY_WINDOW);
        settled && self.key.matches(key) && self.read_within(self.reread_after(), SystemTime::now())
    }

    /// How long after it was read this version must be read again:
    /// between half of `REREAD` and all of it, fixed by the stat.
    fn reread_after(&self) -> Duration {
        let hash = self.key.hash();
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
        self.key.append_to(&mut bytes);
        bytes
    }

    /// The size and observation time from a persisted header. A
    /// nanosecond field of a full second or more is malformed, not a
    /// normalizable value: it would change the observation and can panic
    /// `Duration::new` when the seconds are near `u64::MAX`.
    fn header(bytes: &[u8]) -> Option<(u64, SystemTime)> {
        let seconds = u64::from_le_bytes(bytes.get(..8)?.try_into().ok()?);
        let nanoseconds = u32::from_le_bytes(bytes.get(8..12)?.try_into().ok()?);
        let size = u64::from_le_bytes(bytes.get(12..FINGERPRINT_HEADER_BYTES)?.try_into().ok()?);
        if nanoseconds >= 1_000_000_000 {
            return None;
        }
        let observed_at = UNIX_EPOCH.checked_add(Duration::new(seconds, nanoseconds))?;
        Some((size, observed_at))
    }

    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let (size, observed_at) = Self::header(bytes)?;
        Some(Self {
            key: FingerprintKey::new(bytes.get(FINGERPRINT_HEADER_BYTES..)?),
            size,
            observed_at,
        })
    }

    /// Consume a stored fingerprint and retain a fresh key allocation.
    /// Shrinking the original buffer can keep the header's malloc size class.
    fn from_owned(bytes: Vec<u8>) -> Option<Self> {
        let (size, observed_at) = Self::header(&bytes)?;
        Some(Self {
            key: FingerprintKey::new(&bytes[FINGERPRINT_HEADER_BYTES..]),
            size,
            observed_at,
        })
    }
}

/// One verified observation: an exact version handle plus the exact stat it
/// was derived from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedEntry {
    version: Option<ObservedVersion>,
    fingerprint: Fingerprint,
}

impl ObservedEntry {
    pub(crate) fn new(version: EntryVersion, fingerprint: Fingerprint) -> Self {
        Self::with_version(ObservedVersion::single(version), fingerprint)
    }

    pub(crate) fn with_version(version: ObservedVersion, fingerprint: Fingerprint) -> Self {
        Self {
            version: Some(version),
            fingerprint,
        }
    }

    pub(crate) fn fingerprint(&self) -> &Fingerprint {
        &self.fingerprint
    }

    /// The resolved version. Every normal entry holds one; the option is
    /// internal bookkeeping.
    pub fn version(&self) -> EntryVersion {
        self.version
            .as_ref()
            .expect("an observed entry always holds a version")
            .version()
    }

    /// The version handle, for previews that must resolve it after the run
    /// and its inputs are gone.
    pub fn observed_version(&self) -> ObservedVersion {
        self.version
            .clone()
            .expect("an observed entry always holds a version")
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
        Self::new(
            version,
            Fingerprint {
                key: FingerprintKey::new(&[1, 2, 3]),
                size,
                observed_at: SystemTime::UNIX_EPOCH,
            },
        )
    }

    /// Rebuild a stored entry; `None` if the bytes are not a fingerprint.
    pub fn restore(version: EntryVersion, fingerprint: &[u8]) -> Option<Self> {
        Some(Self::new(version, Fingerprint::from_bytes(fingerprint)?))
    }

    /// Consume a stored fingerprint; `None` if invalid. Only its key payload
    /// remains allocated after decoding.
    pub fn restore_owned(version: EntryVersion, fingerprint: Vec<u8>) -> Option<Self> {
        Some(Self::new(version, Fingerprint::from_owned(fingerprint)?))
    }
}

/// One-percent growth headroom reserved for new paths when a snapshot is
/// recycled. `capacity.div_ceil(ROW_GROWTH_HEADROOM_DIVISOR)` extra rows
/// keep a small number of additions in the existing allocation.
pub(crate) const ROW_GROWTH_HEADROOM_DIVISOR: usize = 100;

/// Builds one side's observed snapshot against the shared immutable base.
///
/// A version that exactly matches the baseline row's side resolves to that
/// row; anything else is appended to one per-side pool. The base is shared,
/// not copied, so a snapshot holds a handle per row instead of a second
/// version per row.
pub struct SnapshotBuilder {
    versions: VersionPoolBuilder,
    rows: PathMapBuilder<ObservedEntry>,
    side: Side,
}

impl SnapshotBuilder {
    /// A builder over `base` for `side`, with room for `capacity` rows plus
    /// one percent growth headroom.
    pub fn new(base: Arc<PathMap<BaselineEntry>>, side: Side, capacity: usize) -> Self {
        let reserved = capacity.saturating_add(capacity.div_ceil(ROW_GROWTH_HEADROOM_DIVISOR));
        Self {
            versions: VersionPoolBuilder::new(base),
            rows: PathMapBuilder::with_capacity(reserved),
            side,
        }
    }

    /// Adds one observed row. `false` when `fingerprint` is not a valid
    /// fingerprint; nothing is interned in that case. The path is consumed
    /// once, borrowed only to resolve its baseline row.
    pub fn insert(&mut self, path: SyncPath, version: EntryVersion, fingerprint: &[u8]) -> bool {
        let Some(fingerprint) = Fingerprint::from_bytes(fingerprint) else {
            return false;
        };
        let version = self.versions.intern(&path, self.side, version);
        self.rows
            .push(path, ObservedEntry::with_version(version, fingerprint));
        true
    }

    /// Seals the appended versions first, then freezes the rows. Handles
    /// already handed out keep the pool alive.
    pub fn freeze(self) -> PathMap<ObservedEntry> {
        let Self {
            versions,
            rows,
            side: _,
        } = self;
        drop(versions.freeze());
        let mut rows = rows.freeze_retaining_capacity();
        if rows.is_empty() {
            // An all-invalid cache must not retain the reserved payload.
            rows.shrink_to_fit();
        }
        rows
    }

    /// Number of appended versions, for tests.
    #[cfg(test)]
    pub(crate) fn owned_versions(&self) -> usize {
        self.versions.owned_len()
    }
}

/// Previous verified entries for one root under the same metadata policy.
/// Observation readers only need borrowed exact-path lookups; callers may
/// keep the rows in either a tree or a dense frozen table.
pub trait PreviousObservation: Sync {
    fn get_key_value(&self, path: &SyncPath) -> Option<(&SyncPath, &ObservedEntry)>;
    fn get(&self, path: &SyncPath) -> Option<&ObservedEntry> {
        self.get_key_value(path).map(|(_, entry)| entry)
    }
}

impl PreviousObservation for BTreeMap<SyncPath, ObservedEntry> {
    fn get_key_value(&self, path: &SyncPath) -> Option<(&SyncPath, &ObservedEntry)> {
        BTreeMap::get_key_value(self, path)
    }
}

impl PreviousObservation for PathMap<ObservedEntry> {
    fn get_key_value(&self, path: &SyncPath) -> Option<(&SyncPath, &ObservedEntry)> {
        PathMap::get_key_value(self, path)
    }
}

/// Known entries plus explicit unread subtrees and leftovers to recover.
pub struct Observation {
    pub(crate) entries: ObservationEntries,
    pending: PathMapBuilder<ObservedEntry>,
    versions: Option<VersionPoolBuilder>,
    recycled: Option<Arc<Mutex<Recycled>>>,
    comparison: Option<Comparison>,
    pub(crate) leftovers: Vec<SyncPath>,
    pub(crate) oversized: Vec<SyncPath>,
    pub(crate) old: Vec<SyncPath>,
    pub(crate) unavailable: Vec<SyncPath>,
    pub(crate) issues: Vec<ScanIssue>,
    pub(crate) unstable: Vec<Option<SyncPath>>,
}

impl Default for Observation {
    fn default() -> Self {
        Self {
            entries: ObservationEntries::default(),
            pending: PathMapBuilder::new(),
            versions: Some(VersionPoolBuilder::default()),
            recycled: None,
            comparison: None,
            leftovers: Vec::new(),
            oversized: Vec::new(),
            old: Vec::new(),
            unavailable: Vec::new(),
            issues: Vec::new(),
            unstable: Vec::new(),
        }
    }
}

impl std::fmt::Debug for Observation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Observation")
            .field("entries", &self.entries)
            .field("issues", &self.issues)
            .field("unavailable", &self.unavailable)
            .finish()
    }
}

/// An unavailable path also covers each descendant. Probe only component
/// prefixes, rather than comparing every entry with every failed subtree.
#[derive(Default)]
struct UnavailablePaths {
    index: PathPrefixIndex,
}

impl UnavailablePaths {
    #[cfg(test)]
    fn from_paths(paths: &[SyncPath]) -> Self {
        Self {
            index: PathPrefixIndex::from_paths(paths),
        }
    }

    fn insert(&mut self, path: &SyncPath) {
        self.index.insert(path);
    }

    fn contains(&self, path: &SyncPath) -> bool {
        self.index.contains(path)
    }

    fn retain_entries(
        &self,
        observation: &mut Observation,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<(), ScanError> {
        let mut cancelled = false;
        observation.retain_entries(|path, _| {
            if cancelled {
                return true;
            }
            if control() == ScanControl::Cancel {
                cancelled = true;
                return true;
            }
            !self.contains(path)
        });
        if cancelled || control() == ScanControl::Cancel {
            return Err(ScanError::Cancelled);
        }
        Ok(())
    }

    fn skip_error(
        &mut self,
        observation: &mut Observation,
        error: ScanError,
    ) -> Result<(), ScanError> {
        let issue = error.into_issue()?;
        self.insert(&issue.path);
        observation.skip_issue(issue);
        Ok(())
    }
}

impl Observation {
    #[cfg(test)]
    pub(crate) fn recycling(
        previous: PathMap<ObservedEntry>,
        base: Arc<PathMap<BaselineEntry>>,
        side: Side,
    ) -> (Self, Cache<'static>) {
        Self::recycling_with_hints(previous, base, side, ObservationHints::default())
    }

    pub(crate) fn recycling_with_hints(
        previous: PathMap<ObservedEntry>,
        base: Arc<PathMap<BaselineEntry>>,
        side: Side,
        hints: ObservationHints,
    ) -> (Self, Cache<'static>) {
        let recycled = Arc::new(Mutex::new(Recycled::with_hints(
            previous, base, side, hints,
        )));
        let cache = Cache::Owned(Arc::clone(&recycled));
        (
            Self {
                recycled: Some(recycled),
                versions: None,
                ..Self::default()
            },
            cache,
        )
    }

    pub(crate) fn record(&mut self, path: SyncPath, mut entry: ObservedEntry) {
        if let Some(recycled) = &self.recycled {
            recycled
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .record(path, entry);
            return;
        }
        let version = self
            .versions
            .as_mut()
            .expect("record precedes finish")
            .intern(&path, Side::Left, entry.version());
        entry.set_version(version);
        entry.set_origin(ObservationOrigin::New);
        self.pending.push(path, entry);
    }

    fn reuse(&mut self, path: SyncPath, known: Arc<ObservedEntry>) {
        if let Some(recycled) = &self.recycled {
            recycled
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .reuse(&path, known);
        } else {
            self.record(
                path,
                Arc::try_unwrap(known).unwrap_or_else(|shared| (*shared).clone()),
            );
        }
    }

    pub(crate) fn finish_entries(&mut self) {
        if let Some(recycled) = self.recycled.take() {
            let (rows, comparison) = recycled
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .finish();
            self.entries = ObservationEntries::new(rows);
            self.comparison = Some(comparison);
            return;
        }
        self.versions.take().expect("finish once").freeze();
        self.entries = ObservationEntries::new(
            std::mem::replace(&mut self.pending, PathMapBuilder::new()).freeze(),
        );
    }

    /// Verified cache rows, including entries excluded from the plan.
    pub fn snapshot_entries(&self) -> impl Iterator<Item = (&SyncPath, &ObservedEntry)> {
        self.entries.snapshot()
    }

    /// Finish exact comparison with the consumed snapshot after alignment
    /// and filtering. Unseen old rows never reach persistence or planning.
    pub fn finish_snapshot(&mut self) -> Option<bool> {
        let comparison = self.comparison.take()?;
        let mut equal = 0;
        let mut changed = false;
        for (_, entry) in self.entries.snapshot() {
            match entry.origin() {
                ObservationOrigin::PriorEqual => equal += 1,
                ObservationOrigin::PriorChanged | ObservationOrigin::New => changed = true,
                ObservationOrigin::Uncompared => unreachable!("snapshot hides unseen entries"),
            }
        }
        self.entries
            .retain(|_, entry| entry.origin() != ObservationOrigin::Uncompared);
        Some(changed || equal != comparison.original_len)
    }

    pub(crate) fn into_snapshot(self) -> PathMap<ObservedEntry> {
        self.entries.into_snapshot()
    }

    /// Reconcile a peer/simulation result with the caller's consumed cache.
    pub(crate) fn recycle_from(
        &mut self,
        previous: PathMap<ObservedEntry>,
        base: Arc<PathMap<BaselineEntry>>,
        side: Side,
        hints: ObservationHints,
    ) {
        let current = std::mem::take(&mut self.entries).into_snapshot();
        let (mut replacement, _) = Self::recycling_with_hints(previous, base, side, hints);
        self.recycled = replacement.recycled.take();
        self.versions = None;
        for (path, entry) in current {
            self.record(path, entry);
        }
        self.finish_entries();
    }

    /// Listings that changed while traversed. Read entries remain usable,
    /// but missing paths below these folders do not prove deletion. `None`
    /// denotes the held root, which has no relative sync path.
    pub fn unstable_directories(&self) -> &[Option<SyncPath>] {
        &self.unstable
    }

    pub fn issues(&self) -> &[ScanIssue] {
        &self.issues
    }

    pub(crate) fn skip_error(&mut self, error: ScanError) -> Result<(), ScanError> {
        self.skip_issue(error.into_issue()?);
        Ok(())
    }

    pub(crate) fn skip_issue(&mut self, issue: ScanIssue) {
        self.unavailable.push(issue.path.clone());
        self.issues.push(issue);
    }

    pub fn entries(&self) -> &ObservationEntries {
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

    /// Failed reads, special files and cloud-only entries. Both folders
    /// must skip these paths and their descendants, never infer deletion.
    pub fn unavailable(&self) -> &[SyncPath] {
        &self.unavailable
    }

    /// Report the entry at `from` under `to`: the same file, spelled as
    /// the pair knows it.
    pub fn respell(&mut self, from: &SyncPath, to: SyncPath) {
        let origin = self
            .entries
            .get(from)
            .map(|entry| self.aligned_origin(&to, entry));
        if origin.is_some() {
            self.preserve_original(from);
            self.preserve_original(&to);
        }
        if let Some(origin) = origin
            && let Some(mut entry) = self.entries.remove(from)
        {
            entry.set_origin(origin);
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
        for directory in self.unstable.iter_mut().flatten() {
            if directory == from {
                *directory = to.clone();
            }
        }
        for issue in &mut self.issues {
            if &issue.path == from {
                issue.path = to.clone();
            }
        }
    }

    /// Apply independent spelling changes together. Mapping sources and
    /// destinations must be distinct; the alignment pass establishes this
    /// before changing any rows. Dense rows are rebuilt once, so a restored
    /// tree with many normalized spellings does not shift a whole table for
    /// every renamed entry.
    pub fn respell_all(&mut self, mappings: &BTreeMap<SyncPath, SyncPath>) {
        if mappings.is_empty() {
            return;
        }
        let rename = |path: &mut SyncPath| {
            if let Some(to) = mappings.get(path) {
                *path = to.clone();
            }
        };
        // Compare every final path against the original flags before
        // changing any flag. Mapping cycles must not make a newly observed
        // source look like an original at another mapping's destination.
        let origins: Vec<_> = mappings
            .iter()
            .map(|(from, to)| {
                self.entries
                    .get(from)
                    .map(|entry| self.aligned_origin(to, entry))
            })
            .collect();
        let mut targets: Vec<_> = mappings
            .iter()
            .zip(&origins)
            .filter_map(|((_, to), origin)| origin.is_some().then_some(to))
            .collect();
        for ((from, to), origin) in mappings.iter().zip(&origins) {
            if origin.is_some() {
                self.preserve_original(from);
                self.preserve_original(to);
            }
        }
        for ((from, _), origin) in mappings.iter().zip(&origins) {
            if let Some(origin) = origin {
                self.entries.set_origin(from, *origin);
            }
        }
        drop(origins);
        targets.sort_unstable();
        self.entries.retain(|path, entry| {
            (entry.origin() != ObservationOrigin::Uncompared
                && !entry.is_excluded()
                && mappings.contains_key(path))
                || targets.binary_search(&path).is_err()
        });
        drop(targets);
        let mut rows = std::mem::take(&mut self.entries).into_rows();
        for (path, entry) in &mut rows {
            if entry.origin() != ObservationOrigin::Uncompared && !entry.is_excluded() {
                rename(path);
            }
        }
        self.entries =
            ObservationEntries::new(PathMapBuilder::from_rows(rows).freeze_retaining_capacity());
        for path in self
            .oversized
            .iter_mut()
            .chain(&mut self.old)
            .chain(&mut self.unavailable)
        {
            rename(path);
        }
        for directory in self.unstable.iter_mut().flatten() {
            rename(directory);
        }
        for issue in &mut self.issues {
            rename(&mut issue.path);
        }
    }

    fn aligned_origin(&self, path: &SyncPath, entry: &ObservedEntry) -> ObservationOrigin {
        let Some(comparison) = &self.comparison else {
            return entry.origin();
        };
        if !comparison.changed.contains_key(path)
            && let Some(time) = comparison.original_time(path)
            && let Some(original) = self.entries.raw().get(path)
        {
            return if original.equals_at(entry, time) {
                ObservationOrigin::PriorEqual
            } else {
                ObservationOrigin::PriorChanged
            };
        }
        let original = comparison.changed.get(path).map(Arc::as_ref).or_else(|| {
            self.entries.raw().get(path).filter(|entry| {
                matches!(
                    entry.origin(),
                    ObservationOrigin::Uncompared | ObservationOrigin::PriorEqual
                )
            })
        });
        match original {
            Some(original) if original == entry => ObservationOrigin::PriorEqual,
            Some(_) => ObservationOrigin::PriorChanged,
            None => ObservationOrigin::New,
        }
    }

    fn preserve_original(&mut self, path: &SyncPath) {
        if let Some(comparison) = &mut self.comparison
            && let Some(entry) = self.entries.raw().get(path)
        {
            comparison.preserve_original(path, entry);
        }
    }

    pub(crate) fn retain_entries(
        &mut self,
        mut keep: impl FnMut(&SyncPath, &ObservedEntry) -> bool,
    ) {
        self.entries.retain(|path, entry| {
            let retained = keep(path, entry);
            if !retained && let Some(comparison) = &mut self.comparison {
                comparison.preserve_original(path, entry);
            }
            retained
        });
    }

    /// Drop paths the other folder skipped, so both skip them.
    pub fn exclude(&mut self, paths: &[SyncPath]) {
        if paths.is_empty() {
            return;
        }
        self.entries.exclude(paths);
    }

    /// The versions the planner compares.
    pub fn versions(&self) -> BTreeMap<SyncPath, EntryVersion> {
        self.entries
            .iter()
            .map(|(path, entry)| (path.clone(), entry.version()))
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

    fn next_entry(&mut self, stats: &mut ScanStats) -> Result<Option<DirectoryEntry>, ScanError> {
        loop {
            match self.entries.next() {
                None => return Ok(None),
                Some(Ok(entry)) => return Ok(Some(entry)),
                Some(Err(error)) if gone(&error) => {
                    // The driver can lose a name between readdir and stat.
                    // Its name is unknown, so preserve unproven absences in
                    // this directory while continuing with later entries.
                    stats.unstable.push(self.path.clone());
                }
                Some(Err(source)) => {
                    return Err(ScanError::Io {
                        path: self.path.clone(),
                        source,
                    });
                }
            }
        }
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
            effort: Effort::Full,
        })
    }

    /// Observe at `effort` from now on (ADR 0054).
    pub fn set_effort(&mut self, effort: Effort) {
        self.effort = effort;
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
        self.alias_guard(path, PathScope::Entry)
    }

    /// `guarded`, and `path` must not be a directory holding the state
    /// inside it: a removal or replacement of one takes the agent down.
    fn guarded_subtree(&self, path: &SyncPath) -> Result<(), ScanError> {
        if self.shelters(path) || self.protects(path) {
            return Err(ScanError::ProtectedPath(path.clone()));
        }
        self.alias_guard(path, PathScope::Subtree)
    }

    /// The prefix only sees the canonical spelling; an alias — a bind
    /// mount, a firmlink, a case-folded name — resolves to the same
    /// directories. `path` and each ancestor are opened and compared by
    /// held-directory identity, which no spelling can evade.
    fn alias_guard(&self, path: &SyncPath, scope: PathScope) -> Result<(), ScanError> {
        let Some(state) = &self.state else {
            return Ok(());
        };
        if at(
            &Some(path.clone()),
            self.root.path_reaches(path, &state.root, scope),
        )? {
            return Err(ScanError::ProtectedPath(path.clone()));
        }
        Ok(())
    }

    pub fn relation_to(&self, other: &Self) -> Result<RootRelation, ScanError> {
        at(&None, self.root.relation_to(&other.root))
    }

    /// How this root nests with `path`, where nothing is now: `Contains`
    /// when it would hold `path`, else `Disjoint` (`Root::relation_to_missing`).
    pub fn relation_to_missing(&self, path: &Path) -> Result<RootRelation, ScanError> {
        at(&None, self.root.relation_to_missing(path))
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

    /// What limits this folder's reads and writes: the network for a
    /// share, otherwise this Mac's own disk and processor.
    pub fn bottleneck(&self) -> Bottleneck {
        match self.root.medium() {
            Ok(Medium::Network) => Bottleneck::Network,
            Ok(Medium::SolidState | Medium::Rotational | Medium::Unknown) | Err(_) => {
                Bottleneck::Local
            }
        }
    }

    /// How many files an observation reads at once. A gentle one reads a
    /// local disk serially on the run's own thread, so it uses at most
    /// one core; a share keeps its overlapped reads, which wait on the
    /// network rather than the processor (ADR 0054).
    fn readers(&self) -> NonZeroUsize {
        match (self.effort, self.bottleneck()) {
            (Effort::Gentle, Bottleneck::Local) => NonZeroUsize::MIN,
            (Effort::Gentle, Bottleneck::Network) | (Effort::Full, _) => self.workers(),
        }
    }

    /// Read the root's first entry: macOS privacy protection and file
    /// permissions refuse listings, not opening the folder.
    pub fn probe_access(&self) -> Result<(), ScanError> {
        let mut entries = at(&None, self.root.entries(DirectoryScope::Root))?;
        listing_allowed(entries.next())
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
            let path = child_path(directory, entry.name())?;
            if self.protects(&path) || self.is_state(entry.metadata()) {
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
    /// `capture` selects the values and ACL the caller can use.
    pub fn read_extended_metadata(
        &self,
        entry: &LocalEntry,
        capture: MetadataCapture,
        control: &mut impl FnMut(u64) -> ScanControl,
    ) -> Result<ExtendedMetadata, ScanError> {
        self.validate_content_access(entry)?;
        self.root
            .read_extended_metadata(entry.path(), entry.metadata(), capture, control)
            .map_err(|source| metadata_error(Some(entry.path.clone()), source))
    }

    pub fn read_root_extended_metadata(
        &self,
        capture: MetadataCapture,
        control: &mut impl FnMut(u64) -> ScanControl,
    ) -> Result<(EntryMetadata, ExtendedMetadata), ScanError> {
        let observed = at_scan(&None, self.root.root_metadata())?;
        if observed.identity() != self.identity {
            return Err(ScanError::Changed(None));
        }
        let extended = self
            .root
            .read_root_extended_metadata(&observed, capture, control)
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
    /// Buffer rows and post-filter `ScanStats::unavailable()` subtrees, which
    /// can fail after rows are emitted. `ScanStats::unstable_directories()`
    /// listings cannot prove absent names. Discard all rows on error. Cancellation
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
            let next = match directory.next_entry(&mut stats) {
                Ok(next) => next,
                Err(error) => {
                    // An unreadable listing leaves its whole directory
                    // unknown, including entries already emitted from it.
                    stats.skip_error(error)?;
                    directories.pop();
                    continue;
                }
            };
            let Some(entry) = next else {
                let finished = directories.pop().expect("the current directory exists");
                let checked = (|| {
                    let current = at_scan(&finished.path, finished.root.root_metadata())?;
                    if current.identity() != finished.before.identity()
                        || current.kind() != NodeKind::Directory
                    {
                        return Err(ScanError::Changed(finished.path.clone()));
                    }
                    reject_dataless(current.data_residency(), &finished.path)?;
                    let mut changed = current != finished.before;
                    if let Some(parent) = directories.last() {
                        let path = finished.path.as_ref().expect("a descendant has a path");
                        let name = SyncPath::try_from(path.file_name())?;
                        let bound = at_scan(&finished.path, parent.root.metadata(&name))?;
                        if bound.identity() != finished.before.identity()
                            || bound.kind() != NodeKind::Directory
                        {
                            return Err(ScanError::Changed(finished.path.clone()));
                        }
                        reject_dataless(bound.data_residency(), &finished.path)?;
                        changed |= bound != finished.before;
                    }
                    if changed {
                        // readdir is not a snapshot. Keep verified entries,
                        // but do not turn an omitted name into a deletion.
                        stats.unstable.push(finished.path.clone());
                    }
                    Ok(())
                })();
                if let Err(error) = checked {
                    stats.skip_error(error)?;
                }
                continue;
            };
            match self.scan_entry(directory, entry, filter, sidecars, &mut stats, sink) {
                Ok(Some(child))
                    if directories
                        .iter()
                        .any(|ancestor| ancestor.before.identity() == child.before.identity()) =>
                {
                    stats.skip_error(ScanError::DirectoryCycle(
                        child.path.expect("a child has a path"),
                    ))?;
                }
                Ok(Some(child)) => directories.push(child),
                Ok(None) => {}
                Err(error) => stats.skip_error(error)?,
            }
        }
        stats.unstable.sort();
        stats.unstable.dedup();
        Ok(stats)
    }

    fn scan_entry(
        &self,
        directory: &Directory,
        entry: DirectoryEntry,
        filter: &Filter,
        sidecars: Sidecars,
        stats: &mut ScanStats,
        sink: &mut impl FnMut(LocalEntry) -> ScanControl,
    ) -> Result<Option<Directory>, ScanError> {
        let path = child_path(directory.path.as_ref(), entry.name())?;
        if protected(entry.name()) || self.protects(&path) || self.is_state(entry.metadata()) {
            stats.skipped += 1;
            if temporary(entry.name()) {
                stats.leftovers.push(path);
            }
            return Ok(None);
        }
        let mut metadata = entry.metadata().clone();
        if filter.excludes(&path, metadata.kind() == NodeKind::Directory) {
            stats.skipped += 1;
            return Ok(None);
        }
        if metadata.kind() == NodeKind::File && filter.too_large(metadata.size()) {
            stats.skipped += 1;
            stats.oversized.push(path);
            return Ok(None);
        }
        if metadata.kind() == NodeKind::File && filter.is_old(metadata.modified()) {
            stats.old.push(path.clone());
        }
        if entry.name().starts_with(b"._") {
            // Native sidecars travel as metadata. Ambiguous ones stay unread.
            if sidecars == Sidecars::Metadata && metadata.kind() == NodeKind::File {
                stats.skipped += 1;
                return Ok(None);
            }
            return Err(ScanError::UnclassifiedSidecar(path));
        }
        if metadata.data_residency() == DataResidency::Dataless
            || metadata.kind() == NodeKind::Other
        {
            // Cloud placeholders must not download; sockets and devices have no file data.
            stats.skipped += 1;
            stats.unavailable.push(path);
            return Ok(None);
        }
        if metadata.identity().device() != self.identity.device() {
            return Err(ScanError::NestedFilesystem(path));
        }
        let at_path = Some(path.clone());
        let name = SyncPath::try_from(entry.name())?;
        let link_target = if metadata.kind() == NodeKind::Symlink {
            Some(at_scan(&at_path, directory.root.read_link(&name))?)
        } else {
            None
        };
        let current = at_scan(&at_path, directory.root.metadata(&name))?;
        if current != metadata {
            if metadata.kind() != NodeKind::Directory
                || current.kind() != NodeKind::Directory
                || current.identity() != metadata.identity()
            {
                return Err(ScanError::Changed(at_path));
            }
            reject_dataless(current.data_residency(), &at_path)?;
            stats.unstable.push(at_path.clone());
            metadata = current;
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
            return Ok(None);
        }

        let child = at_scan(&at_path, directory.root.open_directory(&name))?;
        let child = Directory::open(child, at_path.clone())?;
        if child.before.identity() != metadata.identity() {
            return Err(ScanError::Changed(at_path));
        }
        if child.before != metadata {
            stats.unstable.push(at_path);
        }
        Ok(Some(child))
    }

    /// Versions for readable entries, with failed subtrees explicitly marked
    /// unavailable. Cancellation and unbounded failures discard all rows.
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
        previous: &impl PreviousObservation,
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
        previous: &impl PreviousObservation,
        control: &mut impl FnMut() -> ScanControl,
        progress: &mut impl FnMut(ObserveStep),
    ) -> Result<Observation, ScanError> {
        self.observe_reporting_workers(policy, filter, previous, control, progress, self.readers())
    }

    /// `observe_reporting` on a chosen worker count. One worker keeps the
    /// serial walk; more overlap it with bounded reads. Tests choose the
    /// count, so no public switch exists.
    fn observe_reporting_workers(
        &self,
        policy: MetadataPolicy,
        filter: &Filter,
        previous: &impl PreviousObservation,
        control: &mut impl FnMut() -> ScanControl,
        progress: &mut impl FnMut(ObserveStep),
        workers: NonZeroUsize,
    ) -> Result<Observation, ScanError> {
        self.observe_cached(
            policy,
            filter,
            &Cache::Borrowed(previous),
            Observation::default(),
            control,
            progress,
            workers,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn observe_owned(
        &self,
        policy: MetadataPolicy,
        filter: &Filter,
        previous: PathMap<ObservedEntry>,
        base: Arc<PathMap<BaselineEntry>>,
        side: Side,
        hints: ObservationHints,
        control: &mut impl FnMut() -> ScanControl,
        progress: &mut impl FnMut(ObserveStep),
    ) -> Result<Observation, ScanError> {
        let (observation, previous) =
            Observation::recycling_with_hints(previous, base, side, hints);
        self.observe_cached(
            policy,
            filter,
            &previous,
            observation,
            control,
            progress,
            self.readers(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn observe_cached(
        &self,
        policy: MetadataPolicy,
        filter: &Filter,
        previous: &Cache<'_>,
        observation: Observation,
        control: &mut impl FnMut() -> ScanControl,
        progress: &mut impl FnMut(ObserveStep),
        workers: NonZeroUsize,
    ) -> Result<Observation, ScanError> {
        if control() == ScanControl::Cancel {
            return Err(ScanError::Cancelled);
        }
        if workers == NonZeroUsize::MIN {
            self.observe_reporting_serial(policy, filter, previous, observation, control, progress)
        } else {
            let read = |(entry, _observed_at): &(LocalEntry, SystemTime),
                        asked: &dyn Fn() -> ScanControl| {
                self.entry_version(entry, policy, &mut || asked())
            };
            self.observe_cached_overlapped_with(
                policy,
                filter,
                previous,
                observation,
                control,
                progress,
                workers,
                &read,
            )
        }
    }

    /// The walk and the reads on this thread, one entry at a time.
    fn observe_reporting_serial(
        &self,
        policy: MetadataPolicy,
        filter: &Filter,
        previous: &Cache<'_>,
        mut observation: Observation,
        control: &mut impl FnMut() -> ScanControl,
        progress: &mut impl FnMut(ObserveStep),
    ) -> Result<Observation, ScanError> {
        let mut unavailable = UnavailablePaths::default();
        let mut directories = Vec::new();
        let mut failure = None;
        let scanned = self.scan_filtered(filter, &mut |entry| {
            progress(ObserveStep::Listed);
            // Directory stats may change during traversal. Refresh those after
            // the walk as before; files need no whole-tree metadata inventory.
            if entry.metadata.kind() == NodeKind::Directory {
                directories.push(entry);
                return control();
            }
            match self.observe_entry(
                entry,
                policy,
                previous,
                &mut observation,
                &mut unavailable,
                control,
            ) {
                Ok(()) => {
                    progress(ObserveStep::Checked);
                    ScanControl::Continue
                }
                Err(error) => {
                    failure = Some(error);
                    ScanControl::Cancel
                }
            }
        });
        if let Some(error) = failure {
            return Err(error);
        }
        let stats = scanned?;
        self.finish_observation(
            policy,
            previous,
            observation,
            unavailable,
            directories,
            stats,
            control,
            progress,
        )
    }

    /// The overlapped walk with a chosen read, so a test can substitute a
    /// deterministic helper and observe the driver's own handoff. The
    /// production read calls `entry_version`.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    fn observe_reporting_overlapped_with<G>(
        &self,
        policy: MetadataPolicy,
        filter: &Filter,
        previous: &impl PreviousObservation,
        control: &mut impl FnMut() -> ScanControl,
        progress: &mut impl FnMut(ObserveStep),
        workers: NonZeroUsize,
        read: &G,
    ) -> Result<Observation, ScanError>
    where
        G: Fn(
                &(LocalEntry, SystemTime),
                &dyn Fn() -> ScanControl,
            ) -> Result<EntryVersion, ScanError>
            + Sync,
    {
        self.observe_cached_overlapped_with(
            policy,
            filter,
            &Cache::Borrowed(previous),
            Observation::default(),
            control,
            progress,
            workers,
            read,
        )
    }

    /// The walk on this thread, the reads on bounded workers. This thread
    /// alone asks `control`, reports `progress`, and mutates the observation,
    /// the unavailable index and the stats; a worker only captures one
    /// entry's verified version and checks the shared stop.
    #[allow(clippy::too_many_arguments)]
    fn observe_cached_overlapped_with<G>(
        &self,
        policy: MetadataPolicy,
        filter: &Filter,
        previous: &Cache<'_>,
        mut observation: Observation,
        control: &mut impl FnMut() -> ScanControl,
        progress: &mut impl FnMut(ObserveStep),
        workers: NonZeroUsize,
        read: &G,
    ) -> Result<Observation, ScanError>
    where
        G: Fn(
                &(LocalEntry, SystemTime),
                &dyn Fn() -> ScanControl,
            ) -> Result<EntryVersion, ScanError>
            + Sync,
    {
        let mut unavailable = UnavailablePaths::default();
        let mut directories = Vec::new();
        let mut failure = None;
        let stop = AtomicBool::new(false);
        let (scanned, finish) = thread::scope(|threads| {
            let mut pending = Reads::start(threads, workers, self.effort, &stop, read);
            let scanned = self.scan_filtered(filter, &mut |entry| {
                progress(ObserveStep::Listed);
                // Directory stats may change during traversal. Refresh those
                // after the readers finish; files need no whole-tree metadata
                // inventory.
                if entry.metadata.kind() == NodeKind::Directory {
                    directories.push(entry);
                    return control();
                }
                let prepared = match self.prepare_entry(
                    entry,
                    previous,
                    &mut observation,
                    &mut unavailable,
                    control,
                ) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        failure = Some(error);
                        return ScanControl::Cancel;
                    }
                };
                let Some(job) = prepared else {
                    progress(ObserveStep::Checked);
                    return ScanControl::Continue;
                };
                if let Err(error) = pending.submit(
                    job,
                    &mut |(entry, observed_at), result| {
                        land_entry(
                            entry,
                            *observed_at,
                            result,
                            &mut observation,
                            &mut unavailable,
                        )?;
                        progress(ObserveStep::Checked);
                        Ok(())
                    },
                    control,
                ) {
                    failure = Some(error);
                    return ScanControl::Cancel;
                }
                ScanControl::Continue
            });
            if failure.is_some() || scanned.is_err() {
                // A producer failure is terminal even when a caller's
                // cancellation callback returns Continue on its next call.
                stop.store(true, Ordering::Relaxed);
            }
            let finish = pending.finish(
                &mut |(entry, observed_at), result| {
                    land_entry(
                        entry,
                        *observed_at,
                        result,
                        &mut observation,
                        &mut unavailable,
                    )?;
                    progress(ObserveStep::Checked);
                    Ok(())
                },
                control,
            );
            (scanned, finish)
        });
        if let Some(error) = failure {
            return Err(error);
        }
        let stats = scanned?;
        finish?;
        self.finish_observation(
            policy,
            previous,
            observation,
            unavailable,
            directories,
            stats,
            control,
            progress,
        )
    }

    /// Settle the walk's stats, refresh the deferred directories, then drop
    /// rows below a subtree that failed late.
    #[allow(clippy::too_many_arguments)]
    fn finish_observation(
        &self,
        policy: MetadataPolicy,
        previous: &Cache<'_>,
        mut observation: Observation,
        mut unavailable: UnavailablePaths,
        directories: Vec<LocalEntry>,
        stats: ScanStats,
        control: &mut impl FnMut() -> ScanControl,
        progress: &mut impl FnMut(ObserveStep),
    ) -> Result<Observation, ScanError> {
        observation.leftovers = stats.leftovers;
        observation.oversized = stats.oversized;
        observation.old = stats.old;
        observation.unstable.extend(stats.unstable);
        for path in &stats.unavailable {
            unavailable.insert(path);
        }
        observation.unavailable.extend(stats.unavailable);
        observation.issues.extend(stats.issues);
        for entry in directories {
            self.observe_entry(
                entry,
                policy,
                previous,
                &mut observation,
                &mut unavailable,
                control,
            )?;
            progress(ObserveStep::Checked);
        }
        // A directory can fail after its children were inserted. Suppress
        // those children too, without repeating every failed-root comparison.
        observation.finish_entries();
        unavailable.retain_entries(&mut observation, control)?;
        observation.unstable.sort();
        observation.unstable.dedup();
        Ok(observation)
    }

    /// Retain one verified entry; bounded failures mark its subtree unknown.
    fn observe_entry(
        &self,
        entry: LocalEntry,
        policy: MetadataPolicy,
        previous: &Cache<'_>,
        observation: &mut Observation,
        unavailable: &mut UnavailablePaths,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<(), ScanError> {
        if let Some((entry, observed_at)) =
            self.prepare_entry(entry, previous, observation, unavailable, control)?
        {
            let result = self.entry_version(&entry, policy, control);
            land_entry(&entry, observed_at, result, observation, unavailable)?;
        }
        Ok(())
    }

    /// Check the caller's control, a settled cache entry and a directory's
    /// fresh binding, and capture the observation time before any read.
    /// `None` means the entry is already recorded and needs no read.
    fn prepare_entry(
        &self,
        mut entry: LocalEntry,
        previous: &Cache<'_>,
        observation: &mut Observation,
        unavailable: &mut UnavailablePaths,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<Option<(LocalEntry, SystemTime)>, ScanError> {
        if control() == ScanControl::Cancel {
            return Err(ScanError::Cancelled);
        }
        if unavailable.contains(&entry.path) {
            return Ok(None);
        }
        if entry.metadata.kind() == NodeKind::Directory {
            // Child changes update directory stats during the walk. Read
            // its current metadata under the same identity; file and link
            // versions still require their exact listing observations.
            let current = at_scan(&Some(entry.path.clone()), self.root.metadata(&entry.path));
            match current {
                Ok(current)
                    if current.kind() == NodeKind::Directory
                        && current.identity() == entry.metadata.identity() =>
                {
                    if current != entry.metadata {
                        observation.unstable.push(Some(entry.path.clone()));
                    }
                    entry.metadata = current;
                }
                current => {
                    let error = current
                        .err()
                        .unwrap_or_else(|| ScanError::Changed(Some(entry.path.clone())));
                    unavailable.skip_error(observation, error)?;
                    return Ok(None);
                }
            }
        }
        if let Some(path) = previous.key(&entry.path) {
            entry.path = path;
        }
        if let Some(known) = previous.get(&entry.path)
            && known.fingerprint().proves_unchanged(&entry.metadata)
        {
            observation.reuse(entry.path, known);
            return Ok(None);
        }
        Ok(Some((entry, SystemTime::now())))
    }

    /// One entry's content and tracked metadata. Every read is checked
    /// against the entry's scan observation. A regular file's selected
    /// extended metadata and data digest come from one held descriptor, with
    /// the same namespace and change checks as the separate reads.
    pub fn entry_version(
        &self,
        entry: &LocalEntry,
        policy: MetadataPolicy,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<EntryVersion, ScanError> {
        let mut checkpoint = |_| control();
        let kind = entry.metadata.kind();
        let (payload, extended) = match kind {
            NodeKind::File => {
                // Validate once, then fuse the metadata capture and the data
                // digest through one descriptor.
                self.validate_content_access(entry)?;
                let (digest, captured) = self
                    .root
                    .observe_file(
                        entry.path(),
                        entry.metadata(),
                        capture_for(policy, NodeKind::File),
                        &mut checkpoint,
                    )
                    .map_err(|error| observe_file_error(entry.path(), error))?;
                (Payload::File(digest), Some(captured))
            }
            NodeKind::Directory => {
                let extended = self.optional_extended_metadata(entry, policy, &mut checkpoint)?;
                (Payload::Directory, extended)
            }
            NodeKind::Symlink => {
                let extended = self.optional_extended_metadata(entry, policy, &mut checkpoint)?;
                let target = entry
                    .link_target()
                    .ok_or_else(|| ScanError::UnsupportedEntry(entry.path.clone()))?;
                (Payload::Symlink(target), extended)
            }
            NodeKind::Other => {
                // Preserve the existing failure order: the metadata read
                // reports the unsupported entry before the payload arm.
                self.optional_extended_metadata(entry, policy, &mut checkpoint)?;
                return Err(ScanError::UnsupportedEntry(entry.path.clone()));
            }
        };

        let observed = observed(&entry.metadata, extended.as_ref());
        Ok(entry_version(payload, &observed, policy))
    }

    /// Extended metadata, or `None` where the platform cannot report it.
    pub(crate) fn optional_extended_metadata(
        &self,
        entry: &LocalEntry,
        policy: MetadataPolicy,
        control: &mut impl FnMut(u64) -> ScanControl,
    ) -> Result<Option<ExtendedMetadata>, ScanError> {
        match self.read_extended_metadata(
            entry,
            capture_for(policy, entry.metadata.kind()),
            control,
        ) {
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

/// Record one read result, or mark its subtree unavailable. Bounded read
/// failures never discard the whole observation; cancellation propagates.
fn land_entry(
    entry: &LocalEntry,
    observed_at: SystemTime,
    result: Result<EntryVersion, ScanError>,
    observation: &mut Observation,
    unavailable: &mut UnavailablePaths,
) -> Result<(), ScanError> {
    match result {
        Ok(version) => {
            observation.record(
                entry.path.clone(),
                ObservedEntry::new(version, Fingerprint::new(&entry.metadata, observed_at)),
            );
            Ok(())
        }
        Err(error) => unavailable.skip_error(observation, error),
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

/// Map a fused observation onto the exact scan failures its separate reads
/// produced: content errors keep their file paths, metadata errors use the
/// shared metadata mapping.
fn observe_file_error(path: &SyncPath, error: ObserveFileError) -> ScanError {
    match error {
        ObserveFileError::Hash(HashError::Io(source)) => ScanError::Io {
            path: Some(path.clone()),
            source,
        },
        ObserveFileError::Hash(HashError::Changed) => ScanError::Changed(Some(path.clone())),
        ObserveFileError::Hash(HashError::NotRegular) => ScanError::UnsupportedEntry(path.clone()),
        ObserveFileError::Hash(HashError::Dataless) => ScanError::Dataless(Some(path.clone())),
        ObserveFileError::Hash(HashError::Cancelled) => ScanError::Cancelled,
        ObserveFileError::Metadata(source) => metadata_error(Some(path.clone()), source),
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

/// Whether a root's first listed entry shows that listing it is allowed.
/// A name gone before its metadata was read was still listed: a write
/// there, such as another copy placing its staged file, moved it in
/// between.
fn listing_allowed(first: Option<io::Result<DirectoryEntry>>) -> Result<(), ScanError> {
    match first {
        Some(Err(error)) if gone(&error) => Ok(()),
        Some(Err(source)) => Err(ScanError::Io { path: None, source }),
        Some(Ok(_)) | None => Ok(()),
    }
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
mod churn_tests;
#[cfg(test)]
mod filtering_tests;
#[cfg(test)]
mod refresh_tests;

/// How long a filesystem may take to show a mutation in an entry's stat
/// key. `ctime` advances in coarse ticks on several filesystems, so a
/// mutation made in the same tick as the observation leaves the key — and
/// every fingerprint derived from it — unchanged.
#[cfg(test)]
const STAT_SETTLE_STEP: Duration = Duration::from_millis(20);
#[cfg(test)]
const STAT_SETTLE_ATTEMPTS: u32 = 250;

/// Repeat `mutate` until the entry's stat key differs from the one it had
/// before, so a test can observe its own edit on any filesystem. A
/// fingerprint that cannot see the edit is exactly the case `REREAD`
/// covers, so the wait is what makes the assertion deterministic.
/// `mutate` runs once per attempt, up to `STAT_SETTLE_ATTEMPTS` times, so it
/// must be idempotent: every call has to leave the fixture in the state one
/// call would, or the retries compound it.
#[cfg(test)]
pub(crate) fn settled_stat(endpoint: &LocalEndpoint, path: &SyncPath, mut mutate: impl FnMut()) {
    let key = || {
        endpoint
            .entry_at(path)
            .expect("the entry is reachable")
            .expect("the entry exists")
            .metadata()
            .stat_key()
    };
    let before = key();
    for _ in 0..STAT_SETTLE_ATTEMPTS {
        mutate();
        if key() != before {
            return;
        }
        std::thread::sleep(STAT_SETTLE_STEP);
    }
    panic!("the filesystem never showed the mutation in the entry's stat key");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// ADR 0054: a gentle observation reads this Mac's disk serially on the
    /// run's thread; the copy count narrows in the run, not here.
    #[test]
    fn a_gentle_observation_reads_a_local_disk_on_one_thread() {
        let directory = tempfile::tempdir().unwrap();
        let mut endpoint = LocalEndpoint::open(directory.path()).unwrap();
        assert_eq!(endpoint.bottleneck(), Bottleneck::Local);
        assert_eq!(endpoint.readers(), endpoint.workers());
        let workers = endpoint.workers();
        endpoint.set_effort(Effort::Gentle);
        assert_eq!(endpoint.readers(), NonZeroUsize::MIN);
        assert_eq!(endpoint.workers(), workers);
    }

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

    /// PLAN.md §4.8: a full second in the persisted nanosecond field used
    /// to panic `Duration::new` instead of restoring to `None`.
    #[test]
    fn malformed_fingerprint_headers_are_rejected() {
        let version = EntryVersion::new(
            foldover_types::EntryContent::File(Digest::from_bytes([1; Digest::BYTE_LEN])),
            Digest::from_bytes([2; Digest::BYTE_LEN]),
        );
        let mut header = Vec::new();
        header.extend_from_slice(&u64::MAX.to_le_bytes());
        header.extend_from_slice(&1_000_000_000u32.to_le_bytes());
        header.extend_from_slice(&7u64.to_le_bytes());
        header.extend_from_slice(b"key");

        assert!(ObservedEntry::restore(version, &header).is_none());
        assert!(ObservedEntry::restore_owned(version, header.clone()).is_none());

        // The largest valid nanosecond field with the largest seconds: no
        // representable observation, so still no entry.
        header[8..12].copy_from_slice(&0u32.to_le_bytes());
        assert!(ObservedEntry::restore(version, &header).is_none());
        assert!(ObservedEntry::restore_owned(version, header.clone()).is_none());

        // Too short for the header is not a fingerprint either.
        let short = &header[..FINGERPRINT_HEADER_BYTES - 1];
        assert!(ObservedEntry::restore(version, short).is_none());
        assert!(ObservedEntry::restore_owned(version, short.to_vec()).is_none());
    }

    /// The inline representation is bounded: a version handle plus the
    /// exact fingerprint payload, with no shared data allocation.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn observed_entry_is_56_bytes_on_64_bit() {
        assert_eq!(size_of::<ObservedEntry>(), 56);
    }

    fn path(bytes: &[u8]) -> SyncPath {
        SyncPath::try_from(bytes).unwrap()
    }

    fn file_version(content: u8, metadata: u8) -> EntryVersion {
        EntryVersion::new(
            foldover_types::EntryContent::File(Digest::from_bytes([content; Digest::BYTE_LEN])),
            Digest::from_bytes([metadata; Digest::BYTE_LEN]),
        )
    }

    fn fingerprint_bytes(key: u8) -> Vec<u8> {
        let mut bytes = vec![0; FINGERPRINT_HEADER_BYTES];
        bytes.extend_from_slice(&[key; 93]);
        bytes
    }

    /// An inline entry clones by value: the fingerprint payload is copied,
    /// so the clone stays valid when the original is dropped.
    #[test]
    fn cloning_an_observation_copies_its_value() {
        let version = file_version(1, 2);
        let bytes = fingerprint_bytes(7);
        let original = ObservedEntry::restore(version, &bytes).unwrap();
        let cloned = original.clone();
        assert_ne!(
            original.fingerprint().key.as_ptr(),
            cloned.fingerprint().key.as_ptr()
        );
        assert_eq!(cloned, original);
        drop(original);
        assert_eq!(cloned.version(), version);
        assert_eq!(cloned.fingerprint_bytes(), bytes);
    }

    #[test]
    fn canonical_builder_resolves_unequal_metadata_sides() {
        let left = file_version(1, 2);
        let right = file_version(1, 3);
        let base = BaselineEntry::new(left, right).unwrap();
        let map = PathMap::from_sorted(vec![(path(b"a"), base)]).unwrap();
        let mut builder = SnapshotBuilder::new(Arc::new(map), Side::Left, 2);
        let bytes = fingerprint_bytes(9);

        // An exact left-side match resolves to the baseline row.
        assert!(builder.insert(path(b"a"), left, &bytes));
        assert_eq!(builder.owned_versions(), 0);

        // The right side's version does not match the left side, so it
        // appends an owned version instead of borrowing the wrong row.
        assert!(builder.insert(path(b"a"), right, &bytes));
        assert_eq!(builder.owned_versions(), 1);

        let snapshot = builder.freeze();
        let entry = snapshot.get(&path(b"a")).unwrap();
        assert_eq!(entry.version(), right);
        assert_eq!(entry.fingerprint_bytes(), bytes);
    }

    #[test]
    fn canonical_builder_skips_invalid_fingerprints() {
        let mut builder = SnapshotBuilder::new(Arc::new(PathMap::new()), Side::Left, 1);
        let version = file_version(1, 2);
        let short = vec![0; FINGERPRINT_HEADER_BYTES - 1];
        assert!(!builder.insert(path(b"a"), version, &short));
        assert_eq!(builder.owned_versions(), 0);
        assert!(builder.freeze().is_empty());
    }

    #[test]
    fn snapshot_builder_reserves_one_percent_growth_headroom() {
        let builder = SnapshotBuilder::new(Arc::new(PathMap::new()), Side::Left, 100);
        assert!(builder.rows.capacity() >= 101);
    }

    #[test]
    fn snapshot_builder_freeze_releases_capacity_when_every_row_is_invalid() {
        let mut builder = SnapshotBuilder::new(Arc::new(PathMap::new()), Side::Left, 100);
        let version = file_version(1, 2);
        let short = vec![0; FINGERPRINT_HEADER_BYTES - 1];
        for index in 0..100u8 {
            let name = format!("k{index:03}");
            assert!(!builder.insert(path(name.as_bytes()), version, &short));
        }
        assert_eq!(builder.owned_versions(), 0);
        let snapshot = builder.freeze();
        assert!(snapshot.is_empty());
        assert_eq!(snapshot.capacity(), 0);
    }

    #[test]
    fn builder_versions_survive_builder_and_map_drop() {
        let version = file_version(4, 5);
        let bytes = fingerprint_bytes(1);
        let mut builder = SnapshotBuilder::new(Arc::new(PathMap::new()), Side::Right, 1);
        assert!(builder.insert(path(b"new"), version, &bytes));
        let snapshot = builder.freeze();
        let handle = snapshot.get(&path(b"new")).unwrap().observed_version();
        drop(snapshot);
        assert_eq!(handle.version(), version);
    }

    #[test]
    fn equal_versions_and_fingerprints_compare_equal_across_pools() {
        let version = file_version(3, 4);
        let bytes = fingerprint_bytes(2);
        let mut first = SnapshotBuilder::new(Arc::new(PathMap::new()), Side::Left, 1);
        first.insert(path(b"a"), version, &bytes);
        let mut second = SnapshotBuilder::new(Arc::new(PathMap::new()), Side::Right, 1);
        second.insert(path(b"a"), version, &bytes);
        let first_entry = first.freeze().get(&path(b"a")).unwrap().clone();
        let second_entry = second.freeze().get(&path(b"a")).unwrap().clone();
        assert_eq!(first_entry, second_entry);

        let mut changed = bytes.clone();
        changed[FINGERPRINT_HEADER_BYTES] ^= 1;
        let mut third = SnapshotBuilder::new(Arc::new(PathMap::new()), Side::Left, 1);
        third.insert(path(b"a"), version, &changed);
        let third_entry = third.freeze().get(&path(b"a")).unwrap().clone();
        assert_ne!(first_entry, third_entry);
    }

    /// Shrinking a Vec may keep its old malloc size class. A new payload
    /// allocation avoids retaining the serialized header or capture slack.
    #[test]
    fn fingerprint_payload_does_not_retain_its_capture_buffer() {
        let version = EntryVersion::new(
            foldover_types::EntryContent::File(Digest::from_bytes([1; Digest::BYTE_LEN])),
            Digest::from_bytes([2; Digest::BYTE_LEN]),
        );
        let mut bytes = vec![0; FINGERPRINT_HEADER_BYTES];
        bytes.extend_from_slice(&[7; 93]);
        let original = bytes.as_ptr();
        let restored = ObservedEntry::restore_owned(version, bytes).unwrap();
        assert_ne!(
            restored.fingerprint().key.as_ptr(),
            original,
            "a serialized fingerprint allocation must not become the retained stat key"
        );
        let mut capture = vec![7; 128];
        capture.truncate(93);
        let original = capture.as_ptr();
        let fingerprint = Fingerprint::from_key(capture, 0, UNIX_EPOCH);
        assert_ne!(
            fingerprint.key.as_ptr(),
            original,
            "capture spare capacity must not be retained by the key"
        );
    }

    #[test]
    fn owned_and_borrowed_restore_roundtrip_exact_key() {
        let version = EntryVersion::new(
            foldover_types::EntryContent::File(Digest::from_bytes([1; Digest::BYTE_LEN])),
            Digest::from_bytes([2; Digest::BYTE_LEN]),
        );
        // Every byte value, repeated, so an inexact key could not survive.
        let key: Vec<u8> = (0..=255_u8).chain(0..=63_u8).collect();
        let observed_at =
            UNIX_EPOCH + Duration::from_secs(1_700_000_000) + Duration::from_nanos(123_456_789);
        let fingerprint = Fingerprint::from_key(key.clone(), 4096, observed_at);
        let bytes = fingerprint.to_bytes();

        let borrowed = ObservedEntry::restore(version, &bytes).unwrap();
        let owned = ObservedEntry::restore_owned(version, bytes.clone()).unwrap();

        assert_eq!(borrowed, owned);
        assert!(borrowed.fingerprint().key.matches(&key));
        assert!(owned.fingerprint().key.matches(&key));
        assert_eq!(borrowed.fingerprint().size, 4096);
        assert_eq!(borrowed.fingerprint().observed_at, observed_at);
        assert_eq!(borrowed.fingerprint_bytes(), bytes);
        assert_eq!(owned.fingerprint_bytes(), bytes);

        // A header with no key is a valid fingerprint of an empty key.
        let empty =
            ObservedEntry::restore_owned(version, bytes[..FINGERPRINT_HEADER_BYTES].to_vec())
                .unwrap();
        assert_eq!(empty.fingerprint().key.len(), 0);
        assert_eq!(
            empty.fingerprint_bytes(),
            bytes[..FINGERPRINT_HEADER_BYTES].to_vec()
        );

        // A stored buffer with spare capacity still yields the exact key.
        let mut roomy = Vec::with_capacity(1024);
        roomy.extend_from_slice(&bytes);
        let shrunk = ObservedEntry::restore_owned(version, roomy).unwrap();
        assert_eq!(shrunk, borrowed);
        assert!(shrunk.fingerprint().key.matches(&key));
    }

    /// A pre-epoch observation persists as the epoch, its existing behavior.
    #[test]
    fn pre_epoch_observation_serializes_as_epoch() {
        let version = EntryVersion::new(
            foldover_types::EntryContent::File(Digest::from_bytes([1; Digest::BYTE_LEN])),
            Digest::from_bytes([2; Digest::BYTE_LEN]),
        );
        let fingerprint =
            Fingerprint::from_key(vec![9, 8, 7, 6], 1, UNIX_EPOCH - Duration::from_secs(10));
        let bytes = fingerprint.to_bytes();
        assert_eq!(bytes[..8].to_vec(), 0u64.to_le_bytes().to_vec());
        assert_eq!(bytes[8..12].to_vec(), 0u32.to_le_bytes().to_vec());
        assert_eq!(
            bytes[12..FINGERPRINT_HEADER_BYTES].to_vec(),
            1u64.to_le_bytes().to_vec()
        );

        let restored = ObservedEntry::restore(version, &bytes).unwrap();
        assert_eq!(restored.fingerprint().observed_at, UNIX_EPOCH);
        assert!(restored.fingerprint().key.matches(&[9, 8, 7, 6]));
    }

    #[test]
    fn captured_stat_key_keeps_only_its_payload() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("file"), b"data").unwrap();
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();
        let path = SyncPath::try_from(b"file".as_slice()).unwrap();
        let entry = endpoint
            .entry_at(&path)
            .expect("the entry is reachable")
            .expect("the entry exists");
        let metadata = entry.metadata();
        let key = metadata.stat_key();
        let fingerprint = Fingerprint::new(metadata, SystemTime::UNIX_EPOCH);

        assert!(key.capacity() >= key.len());
        assert_eq!(fingerprint.key.len(), key.len());
        assert!(fingerprint.key.matches(&key));
        assert!(fingerprint.key.encoded_len() <= key.len() + 1);
        assert_eq!(&fingerprint.to_bytes()[FINGERPRINT_HEADER_BYTES..], key);
    }

    #[test]
    fn reread_spread_follows_the_exact_key_bytes() {
        let now = SystemTime::now();
        let key: Vec<u8> = (0..=255_u8).collect();
        let limit = Fingerprint::from_key(key.clone(), 1, now).reread_after();

        let hash = blake3::hash(&key);
        let spread = u64::from_le_bytes(hash.as_bytes()[..8].try_into().expect("8 bytes"));
        let half = (REREAD.as_secs() / 2).max(1);
        assert_eq!(limit, Duration::from_secs(half + spread % half));
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
                endpoint.read_extended_metadata(&entry, MetadataCapture::Full, &mut |_| panic!(
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
            .read_extended_metadata(entry, MetadataCapture::Full, &mut |_| ScanControl::Continue)
            .unwrap();
        let (root, _) = endpoint
            .read_root_extended_metadata(MetadataCapture::Full, &mut |_| ScanControl::Continue)
            .unwrap();
        assert_eq!(root.identity(), endpoint.identity);
        assert!(matches!(
            endpoint.read_extended_metadata(entry, MetadataCapture::Full, &mut |_| {
                ScanControl::Cancel
            }),
            Err(ScanError::Cancelled)
        ));
        fs::write(temporary.path().join("file"), b"changed").unwrap();
        assert!(matches!(
            endpoint.read_extended_metadata(entry, MetadataCapture::Full, &mut |_| {
                ScanControl::Continue
            }),
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
        let stats = result.unwrap();
        assert_eq!(
            stats.unavailable(),
            &[SyncPath::try_from(b"directory".as_slice()).unwrap()]
        );
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
        let stats = result.unwrap();
        assert_eq!(
            stats.unavailable(),
            &[SyncPath::try_from(b"directory".as_slice()).unwrap()]
        );
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
    fn root_transport_and_cancellation_errors_never_become_scan_issues() {
        let path = SyncPath::try_from(b"folder".as_slice()).unwrap();
        let mut errors = vec![
            ScanError::Cancelled,
            ScanError::Changed(None),
            ScanError::ProtectedPath(path.clone()),
            ScanError::Io {
                path: None,
                source: io::ErrorKind::PermissionDenied.into(),
            },
        ];
        errors.extend(
            [
                io::ErrorKind::BrokenPipe,
                io::ErrorKind::ConnectionAborted,
                io::ErrorKind::ConnectionReset,
                io::ErrorKind::NotConnected,
                io::ErrorKind::UnexpectedEof,
                io::ErrorKind::TimedOut,
            ]
            .map(|kind| ScanError::Io {
                path: Some(path.clone()),
                source: kind.into(),
            }),
        );
        for error in errors {
            let mut observation = Observation::default();
            assert!(observation.skip_error(error).is_err());
            assert!(observation.issues().is_empty());
            assert!(observation.unavailable().is_empty());
        }
    }

    #[test]
    fn a_file_changed_after_listing_does_not_discard_healthy_versions() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("changed"), b"before").unwrap();
        fs::write(temporary.path().join("safe"), b"keep").unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        // A streamed first file may finish before the second listing.
        // Change this file at its read boundary, after its stat was listed,
        // so the regression depends on neither scheduling nor readdir order.
        let read = |(entry, _): &(LocalEntry, SystemTime), asked: &dyn Fn() -> ScanControl| {
            if entry.path.as_bytes() == b"changed" {
                fs::write(temporary.path().join("changed"), b"after").unwrap();
            }
            endpoint.entry_version(entry, ALL, &mut || asked())
        };
        let previous = BTreeMap::new();
        let observation = endpoint
            .observe_cached_overlapped_with(
                ALL,
                &Filter::none(),
                &Cache::Borrowed(&previous),
                Observation::default(),
                &mut || ScanControl::Continue,
                &mut |_| {},
                NonZeroUsize::new(4).unwrap(),
                &read,
            )
            .unwrap();
        assert_eq!(observation.entries().len(), 1);
        assert!(
            observation
                .entries()
                .contains_key(&SyncPath::try_from(b"safe".as_slice()).unwrap())
        );
        assert_eq!(
            observation.unavailable(),
            &[SyncPath::try_from(b"changed".as_slice()).unwrap()]
        );
    }

    #[test]
    fn sidecars_are_not_misclassified_as_ordinary_file_data() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("._file"), b"metadata").unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        let stats = endpoint
            .scan_metadata(&mut |_| panic!("sidecar must not be emitted"))
            .unwrap();
        assert_eq!(stats.skipped(), 1);
        assert_eq!(
            stats.issues()[0].path(),
            &SyncPath::try_from(b"._file".as_slice()).unwrap()
        );
        assert!(stats.issues()[0].message().contains("AppleDouble"));
    }

    #[test]
    fn directory_membership_changes_do_not_abort_the_inventory() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("only"), b"one").unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        let result = endpoint.scan_metadata(&mut |_| {
            fs::write(temporary.path().join("new"), b"new").unwrap();
            ScanControl::Continue
        });
        assert!(
            result.is_ok(),
            "directory churn discarded readable entries: {result:?}"
        );
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
        let file_path = root.join("folder/file");
        fs::create_dir(root.join("folder")).unwrap();
        fs::write(&file_path, b"data").unwrap();
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

        let handle = fs::File::open(&file_path).unwrap();
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

        // Toggle a bit so this changes metadata under every process umask.
        let mode = fs::metadata(&file_path).unwrap().permissions().mode() & 0o7777;
        fs::set_permissions(&file_path, fs::Permissions::from_mode(mode ^ 0o100)).unwrap();
        let mode_changed = observe();
        assert_eq!(mode_changed[&file].content(), first[&file].content());
        assert_ne!(mode_changed[&file].metadata(), tagged[&file].metadata());

        // A new child moves the directory mtime, which versions ignore.
        fs::write(root.join("folder/other"), b"").unwrap();
        assert_eq!(observe()[&folder], first[&folder]);

        fs::write(&file_path, b"edited").unwrap();
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
        assert!(restored.iter().eq(first.entries().iter()));
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
        // The edit keeps the size and the modification time, so only the
        // stat key can tell it apart from an unchanged entry.
        let edited = SyncPath::try_from(b"edited".as_slice()).unwrap();
        settled_stat(&endpoint, &edited, || {
            fs::write(root.join("edited"), b"TWO").unwrap();
            fs::File::options()
                .write(true)
                .open(root.join("edited"))
                .unwrap()
                .set_modified(past)
                .unwrap();
        });
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

    #[cfg(target_os = "macos")]
    #[test]
    fn policy_aware_capture_matches_full_capture_across_the_policy_matrix() {
        use foldover_fs::{AccessControl, AttributeRole, MetadataCapture};
        use foldover_types::Propagation;

        let directory = tempfile::tempdir().unwrap();
        let file_path = directory.path().join("file");
        fs::write(&file_path, b"data").unwrap();
        {
            let handle = fs::File::open(&file_path).unwrap();
            for (name, value) in [
                (c"com.apple.ResourceFork", b"fork".as_slice()),
                (c"com.apple.metadata:_kMDItemUserTags", b"tags".as_slice()),
                (c"com.apple.quarantine", b"0083;68b1d0e0;Safari;".as_slice()),
                (c"com.apple.macl", b"managed".as_slice()),
            ] {
                rustix::fs::fsetxattr(&handle, name, value, rustix::fs::XattrFlags::empty())
                    .unwrap();
            }
        }
        let status = std::process::Command::new("/bin/chmod")
            .args([
                "+a",
                "everyone allow read,readattr,readextattr,readsecurity",
            ])
            .arg(&file_path)
            .status()
            .unwrap();
        assert!(status.success());

        let endpoint = LocalEndpoint::open(directory.path()).unwrap();
        let mut entries = Vec::new();
        endpoint
            .scan_metadata(&mut |entry| {
                entries.push(entry);
                ScanControl::Continue
            })
            .unwrap();
        let entry = entries
            .into_iter()
            .find(|entry| entry.path.as_bytes() == b"file")
            .unwrap();

        let full = endpoint
            .read_extended_metadata(&entry, MetadataCapture::Full, &mut |_| {
                ScanControl::Continue
            })
            .unwrap();
        assert!(
            full.attributes()
                .iter()
                .any(|attribute| attribute.name() == b"com.apple.ResourceFork")
        );
        assert!(
            full.attributes()
                .iter()
                .any(|attribute| attribute.name() == b"com.apple.quarantine")
        );
        assert!(matches!(
            full.access_control(),
            AccessControl::MacOs(Some(bytes)) if !bytes.is_empty()
        ));
        let data = endpoint
            .hash_file(&entry, &mut |_| ScanControl::Continue)
            .unwrap();

        for times in [Propagation::Sync, Propagation::Ignore] {
            for extended in [Propagation::Sync, Propagation::Ignore] {
                for permissions in [Propagation::Sync, Propagation::Ignore] {
                    for quarantine in [Propagation::Sync, Propagation::Ignore] {
                        let policy = MetadataPolicy::new(times, extended, permissions)
                            .with_quarantine(quarantine);
                        let expected = entry_version(
                            Payload::File(data),
                            &observed(entry.metadata(), Some(&full)),
                            policy,
                        );
                        let observed_version = endpoint
                            .entry_version(&entry, policy, &mut || ScanControl::Continue)
                            .unwrap();
                        assert_eq!(observed_version, expected, "policy {policy:?}");

                        let capture = capture_for(policy, NodeKind::File);
                        assert!(capture.wants(AttributeRole::ResourceFork));
                        assert_eq!(
                            capture.wants(AttributeRole::Synced),
                            extended == Propagation::Sync
                        );
                        assert_eq!(
                            capture.wants(AttributeRole::Quarantine),
                            policy.quarantine() == Propagation::Sync
                        );
                        assert!(!capture.wants(AttributeRole::SystemManaged));
                        assert!(!capture.wants(AttributeRole::CompressionStorage));
                        assert_eq!(
                            capture.wants_access_control(),
                            permissions == Propagation::Sync
                        );
                    }
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn policy_aware_capture_detects_concurrent_edits_to_wanted_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let file_path = directory.path().join("file");
        fs::write(&file_path, b"data").unwrap();
        let path = SyncPath::try_from(b"file".as_slice()).unwrap();
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();
        let mut entries = Vec::new();
        endpoint
            .scan_metadata(&mut |entry| {
                entries.push(entry);
                ScanControl::Continue
            })
            .unwrap();
        let entry = entries
            .into_iter()
            .find(|entry| entry.path.as_bytes() == b"file")
            .unwrap();

        let result = endpoint.entry_version(&entry, ALL, &mut || {
            settled_stat(&endpoint, &path, || {
                let handle = fs::File::open(&file_path).unwrap();
                rustix::fs::fsetxattr(
                    &handle,
                    c"com.apple.metadata:_kMDItemUserTags",
                    b"changed",
                    rustix::fs::XattrFlags::empty(),
                )
                .unwrap();
            });
            ScanControl::Continue
        });
        assert!(matches!(result, Err(ScanError::Changed(_))));
    }

    #[test]
    fn fused_file_versions_match_separate_content_and_metadata_reads() {
        use foldover_types::Propagation;

        let directory = tempfile::tempdir().unwrap();
        let file_path = directory.path().join("file");
        fs::write(&file_path, b"data").unwrap();
        let path = SyncPath::try_from(b"file".as_slice()).unwrap();
        {
            let handle = fs::File::open(&file_path).unwrap();
            rustix::fs::fsetxattr(
                &handle,
                TEST_ATTRIBUTE,
                b"tag",
                rustix::fs::XattrFlags::empty(),
            )
            .unwrap();
            #[cfg(target_os = "macos")]
            for (name, value) in [
                (c"com.apple.ResourceFork", b"fork".as_slice()),
                (c"com.apple.quarantine", b"0083;68b1d0e0;Safari;".as_slice()),
            ] {
                rustix::fs::fsetxattr(&handle, name, value, rustix::fs::XattrFlags::empty())
                    .unwrap();
            }
        }

        let endpoint = LocalEndpoint::open(directory.path()).unwrap();
        let mut entries = Vec::new();
        endpoint
            .scan_metadata(&mut |entry| {
                entries.push(entry);
                ScanControl::Continue
            })
            .unwrap();
        let entry = entries
            .into_iter()
            .find(|entry| entry.path == path)
            .unwrap();
        let data = endpoint
            .hash_file(&entry, &mut |_| ScanControl::Continue)
            .unwrap();

        for times in [Propagation::Sync, Propagation::Ignore] {
            for extended in [Propagation::Sync, Propagation::Ignore] {
                for permissions in [Propagation::Sync, Propagation::Ignore] {
                    for quarantine in [Propagation::Sync, Propagation::Ignore] {
                        let policy = MetadataPolicy::new(times, extended, permissions)
                            .with_quarantine(quarantine);
                        let capture = capture_for(policy, NodeKind::File);
                        let extended_metadata = endpoint
                            .read_extended_metadata(&entry, capture, &mut |_| ScanControl::Continue)
                            .unwrap();
                        let expected = entry_version(
                            Payload::File(data),
                            &observed(entry.metadata(), Some(&extended_metadata)),
                            policy,
                        );
                        let fused = endpoint
                            .entry_version(&entry, policy, &mut || ScanControl::Continue)
                            .unwrap();
                        assert_eq!(fused, expected, "policy {policy:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn file_observation_streams_checks_within_the_reader_queue_bound() {
        let directory = tempfile::tempdir().unwrap();
        let workers = NonZeroUsize::new(4).expect("four is not zero");
        // More files than the bounded queue holds, so a walk that buffered
        // the whole inventory would check only after listing every entry.
        let files = 2 * workers.get() + 2;
        for index in 0..files {
            fs::write(directory.path().join(format!("file-{index}")), b"payload").unwrap();
        }
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();

        let mut listed = 0;
        let mut first_checked = None;
        let result = endpoint
            .observe_reporting_workers(
                MetadataPolicy::default(),
                &Filter::none(),
                &BTreeMap::new(),
                &mut || ScanControl::Continue,
                &mut |step| match step {
                    ObserveStep::Listed => listed += 1,
                    ObserveStep::Checked => {
                        first_checked.get_or_insert(listed);
                    }
                },
                workers,
            )
            .unwrap();
        assert_eq!(result.entries().len(), files);
        let first_checked = first_checked.expect("every file is checked");
        assert!(
            first_checked <= 2 * workers.get() + 1,
            "the first check landed at {first_checked}, past the reader queue"
        );
        assert!(
            first_checked < files,
            "the first check landed at {first_checked}, after the whole walk"
        );

        // One worker keeps the serial order: each entry is checked as it is
        // listed.
        let mut listed = 0;
        let mut first_checked = None;
        endpoint
            .observe_reporting_workers(
                MetadataPolicy::default(),
                &Filter::none(),
                &BTreeMap::new(),
                &mut || ScanControl::Continue,
                &mut |step| match step {
                    ObserveStep::Listed => listed += 1,
                    ObserveStep::Checked => {
                        first_checked.get_or_insert(listed);
                    }
                },
                NonZeroUsize::MIN,
            )
            .unwrap();
        assert_eq!(first_checked, Some(1));
    }

    /// The overlapped walk must really read on several workers at once. The
    /// probe counts active `entry_version` calls; one worker would leave the
    /// maximum at one.
    #[test]
    fn overlapped_observation_reads_on_several_threads_at_once() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Condvar, Mutex};
        use std::time::Duration;

        let directory = tempfile::tempdir().unwrap();
        let files = 16;
        for index in 0..files {
            fs::write(directory.path().join(format!("file-{index}")), b"payload").unwrap();
        }
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();

        // The helper waits until four reads are in flight. A bounded wait
        // keeps a broken driver from hanging the test; the flag records
        // whether they met.
        let active = Arc::new((Mutex::new(0_usize), Condvar::new()));
        let reached = Arc::new(AtomicBool::new(false));
        let required = 4;
        let met = Arc::clone(&reached);
        let read = move |_: &(LocalEntry, SystemTime), _: &dyn Fn() -> ScanControl| {
            let (lock, changed) = &*active;
            let mut in_flight = lock.lock().unwrap();
            *in_flight += 1;
            if *in_flight >= required {
                met.store(true, Ordering::SeqCst);
                changed.notify_all();
            } else {
                let (guard, _timeout) = changed
                    .wait_timeout_while(in_flight, Duration::from_secs(5), |in_flight| {
                        *in_flight < required && !met.load(Ordering::SeqCst)
                    })
                    .unwrap();
                in_flight = guard;
            }
            drop(in_flight);
            Ok(EntryVersion::new(
                foldover_types::EntryContent::Directory,
                Digest::from_bytes([0; Digest::BYTE_LEN]),
            ))
        };

        let observation = endpoint
            .observe_reporting_overlapped_with(
                MetadataPolicy::default(),
                &Filter::none(),
                &BTreeMap::new(),
                &mut || ScanControl::Continue,
                &mut |_| {},
                NonZeroUsize::new(required).unwrap(),
                &read,
            )
            .unwrap();
        assert_eq!(observation.entries().len(), files);
        assert!(
            reached.load(Ordering::SeqCst),
            "reads did not overlap on four workers"
        );
    }

    /// Workers only capture versions; the caller's control and progress run
    /// on the walk's thread, so a cancellation or a progress callback cannot
    /// race the observation it describes.
    #[test]
    fn control_and_progress_stay_on_the_run_thread() {
        let directory = tempfile::tempdir().unwrap();
        for index in 0..8 {
            fs::write(directory.path().join(format!("file-{index}")), b"payload").unwrap();
        }
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();
        let caller = std::thread::current().id();
        let mut control_thread = None;
        let mut progress_thread = None;
        let observation = endpoint
            .observe_reporting_workers(
                MetadataPolicy::default(),
                &Filter::none(),
                &BTreeMap::new(),
                &mut || {
                    control_thread.get_or_insert(std::thread::current().id());
                    ScanControl::Continue
                },
                &mut |_| {
                    progress_thread.get_or_insert(std::thread::current().id());
                },
                NonZeroUsize::new(4).expect("four is not zero"),
            )
            .unwrap();
        assert_eq!(observation.entries().len(), 8);
        assert_eq!(control_thread, Some(caller));
        assert_eq!(progress_thread, Some(caller));
    }

    /// Cancelling stops the readers and winds them down; the failed call
    /// publishes no partial observation, and the endpoint observes again.
    #[test]
    fn cancelling_an_overlapped_observation_leaves_no_partial_result() {
        let directory = tempfile::tempdir().unwrap();
        let files = 64;
        for index in 0..files {
            fs::write(directory.path().join(format!("file-{index}")), b"payload").unwrap();
        }
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();
        let checked = Cell::new(0);
        let result = endpoint.observe_reporting_workers(
            MetadataPolicy::default(),
            &Filter::none(),
            &BTreeMap::new(),
            &mut || match checked.get() {
                0 => ScanControl::Continue,
                _ => ScanControl::Cancel,
            },
            &mut |step| {
                if step == ObserveStep::Checked {
                    checked.set(checked.get() + 1);
                }
            },
            NonZeroUsize::new(4).expect("four is not zero"),
        );
        assert!(matches!(result, Err(ScanError::Cancelled)), "{result:?}");

        let observation = endpoint
            .observe_reporting_workers(
                MetadataPolicy::default(),
                &Filter::none(),
                &BTreeMap::new(),
                &mut || ScanControl::Continue,
                &mut |_| {},
                NonZeroUsize::new(4).expect("four is not zero"),
            )
            .unwrap();
        assert_eq!(observation.entries().len(), files);
    }

    /// A settled cache entry is recorded without a read job; a recent one
    /// still needs the worker's read.
    #[test]
    fn one_shot_producer_cancellation_stops_reads_already_in_progress() {
        use std::sync::mpsc;
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("a"), b"a").unwrap();
        fs::write(directory.path().join("b"), b"b").unwrap();
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();
        let (started, wait_started) = mpsc::channel();
        let (finished, wait_finished) = mpsc::channel();
        let release = AtomicBool::new(false);
        let stopped = AtomicBool::new(false);
        let cancel_next = Cell::new(false);
        let listed = Cell::new(0);
        thread::scope(|threads| {
            let watchdog_release = &release;
            threads.spawn(move || {
                // A broken implementation still exits so the regression
                // fails rather than hanging the suite.
                if wait_finished.recv_timeout(Duration::from_secs(5)).is_err() {
                    watchdog_release.store(true, Ordering::Relaxed);
                }
            });
            let read = |_: &(LocalEntry, SystemTime), asked: &dyn Fn() -> ScanControl| {
                started.send(()).unwrap();
                while !release.load(Ordering::Relaxed) {
                    if asked() == ScanControl::Cancel {
                        stopped.store(true, Ordering::Relaxed);
                        finished.send(()).unwrap();
                        return Err(ScanError::Cancelled);
                    }
                    thread::yield_now();
                }
                Err(ScanError::Cancelled)
            };
            let result = endpoint.observe_reporting_overlapped_with(
                MetadataPolicy::default(),
                &Filter::none(),
                &BTreeMap::new(),
                &mut || {
                    if cancel_next.replace(false) {
                        ScanControl::Cancel
                    } else {
                        ScanControl::Continue
                    }
                },
                &mut |step| {
                    if step == ObserveStep::Listed {
                        listed.set(listed.get() + 1);
                        if listed.get() == 2 {
                            wait_started.recv_timeout(Duration::from_secs(5)).unwrap();
                            cancel_next.set(true);
                        }
                    }
                },
                NonZeroUsize::new(2).unwrap(),
                &read,
            );
            assert!(matches!(result, Err(ScanError::Cancelled)));
        });
        assert!(
            stopped.load(Ordering::Relaxed),
            "producer cancellation did not reach an active read"
        );
    }

    #[test]
    fn settled_previous_entries_need_no_read_job() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let past = UNIX_EPOCH + Duration::from_secs(1_000_000);
        fs::write(root.join("settled"), b"one").unwrap();
        fs::File::options()
            .write(true)
            .open(root.join("settled"))
            .unwrap()
            .set_modified(past)
            .unwrap();
        fs::write(root.join("recent"), b"two").unwrap();
        let endpoint = LocalEndpoint::open(root).unwrap();
        let previous = endpoint
            .observe(ALL, &mut || ScanControl::Continue)
            .unwrap();
        let mut observation = Observation::default();
        let mut unavailable = UnavailablePaths::default();
        let mut control = || ScanControl::Continue;
        let path = |name: &[u8]| SyncPath::try_from(name).unwrap();

        let settled = endpoint.entry_at(&path(b"settled")).unwrap().unwrap();
        assert!(
            endpoint
                .prepare_entry(
                    settled,
                    &Cache::Borrowed(previous.entries()),
                    &mut observation,
                    &mut unavailable,
                    &mut control,
                )
                .unwrap()
                .is_none()
        );
        observation.finish_entries();
        assert!(observation.entries().contains_key(&path(b"settled")));

        let recent = endpoint.entry_at(&path(b"recent")).unwrap().unwrap();
        assert!(
            endpoint
                .prepare_entry(
                    recent,
                    &Cache::Borrowed(previous.entries()),
                    &mut observation,
                    &mut unavailable,
                    &mut control,
                )
                .unwrap()
                .is_some()
        );
    }

    /// One worker and four find the same versions, skips, unstable
    /// directories and progress counts, so the overlap changes only the
    /// order in which reads finish.
    #[test]
    fn serial_and_overlapped_observations_agree() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir(root.join("folder")).unwrap();
        fs::write(root.join("folder/file"), b"data").unwrap();
        fs::write(root.join("notes"), b"notes").unwrap();
        symlink("folder/file", root.join("link")).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(root.join("pipe"))
                .status()
                .unwrap()
                .success()
        );
        let endpoint = LocalEndpoint::open(root).unwrap();

        let observe = |workers| {
            let mut steps = Vec::new();
            let observation = endpoint
                .observe_reporting_workers(
                    MetadataPolicy::default(),
                    &Filter::none(),
                    &BTreeMap::new(),
                    &mut || ScanControl::Continue,
                    &mut |step| steps.push(step),
                    workers,
                )
                .unwrap();
            (observation, steps)
        };
        let (serial, serial_steps) = observe(NonZeroUsize::MIN);
        let (overlapped, overlapped_steps) = observe(NonZeroUsize::new(4).unwrap());

        let versions = |observation: &Observation| {
            observation
                .entries()
                .iter()
                .map(|(path, entry)| (path.clone(), entry.version()))
                .collect::<Vec<_>>()
        };
        assert_eq!(versions(&overlapped), versions(&serial));

        let unavailable = |observation: &Observation| {
            let mut paths = observation.unavailable().to_vec();
            paths.sort();
            paths
        };
        assert_eq!(unavailable(&overlapped), unavailable(&serial));

        let unstable = |observation: &Observation| {
            let mut directories = observation.unstable_directories().to_vec();
            directories.sort();
            directories
        };
        assert_eq!(unstable(&overlapped), unstable(&serial));

        let issues = |observation: &Observation| {
            let mut issues = observation
                .issues()
                .iter()
                .map(|issue| (issue.path().clone(), issue.message().to_string()))
                .collect::<Vec<_>>();
            issues.sort();
            issues
        };
        assert_eq!(issues(&overlapped), issues(&serial));

        let count = |steps: &[ObserveStep], wanted: ObserveStep| {
            steps.iter().filter(|&&step| step == wanted).count()
        };
        assert_eq!(
            count(&overlapped_steps, ObserveStep::Listed),
            count(&serial_steps, ObserveStep::Listed)
        );
        assert_eq!(
            count(&overlapped_steps, ObserveStep::Checked),
            count(&serial_steps, ObserveStep::Checked)
        );
    }

    #[test]
    fn streamed_files_are_discarded_when_their_directory_loses_its_binding() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("parent")).unwrap();
        fs::write(directory.path().join("parent/file"), b"payload").unwrap();
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();
        let mut moved = false;
        let observation = endpoint
            .observe_reporting(
                MetadataPolicy::default(),
                &Filter::none(),
                &BTreeMap::new(),
                &mut || ScanControl::Continue,
                &mut |step| {
                    if step == ObserveStep::Checked && !moved {
                        fs::rename(
                            directory.path().join("parent"),
                            directory.path().join("moved"),
                        )
                        .unwrap();
                        moved = true;
                    }
                },
            )
            .unwrap();
        assert!(moved);
        let parent = SyncPath::try_from(b"parent".as_slice()).unwrap();
        assert!(observation.unavailable().contains(&parent));
        assert!(
            observation
                .entries()
                .keys()
                .all(|path| path != &parent && !parent.is_ancestor_of(path))
        );
    }
}
