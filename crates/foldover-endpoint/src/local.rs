mod apply;
mod archive;
mod conflict;
mod filter;
mod identity;
mod version;

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use foldover_fs::{
    DataResidency, DirectoryEntry, DirectoryScope, EntryMetadata, ExtendedMetadata, FileIdentity,
    FilesystemFeatures, HashError, MetadataReadError, NodeKind, Root, RootRelation,
};
pub use foldover_types::ScanControl;
use foldover_types::{Digest, EntryVersion, InvalidSyncPath, MetadataPolicy, SyncPath};

use version::{Attribute, Observed, Payload, entry_version, metadata_version};

pub use apply::{ApplyError, Expected, RunScope};
pub use archive::{ArchivedItem, ArchivedRun, archive_name};
pub use filter::{Filter, InvalidFilter};

const INTERNAL_DIRECTORY: &[u8] = b".foldover";
const TEMPORARY_PREFIX: &[u8] = b".foldover-tmp-";
const FINDER_STATE: &[u8] = b".DS_Store";

/// A held local root. Metadata inventory does not establish volume UUIDs,
/// content equality, metadata fidelity or readiness to execute a sync.
pub struct LocalEndpoint {
    root: Root,
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

/// How long after its last modification an entry's stat stays untrusted.
/// Coarse timestamps (FAT, exFAT) cannot show an edit within the same tick.
const RACY_WINDOW: Duration = Duration::from_secs(3);

/// The exact stat a version was derived from. An identical later stat proves
/// the version unchanged without rereading data, outside the racy window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fingerprint {
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
        // Equal keys include equal mtimes, so `current` supplies it.
        let settled = self
            .observed_at
            .duration_since(current.modified())
            .is_ok_and(|age| age >= RACY_WINDOW);
        settled && current.stat_key() == self.key
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
    version: EntryVersion,
    fingerprint: Fingerprint,
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
    entries: BTreeMap<SyncPath, ObservedEntry>,
    leftovers: Vec<SyncPath>,
}

impl Observation {
    pub fn entries(&self) -> &BTreeMap<SyncPath, ObservedEntry> {
        &self.entries
    }

    pub fn leftovers(&self) -> &[SyncPath] {
        &self.leftovers
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
        let before = at(&path, root.root_metadata())?;
        reject_dataless(before.data_residency(), &path)?;
        let entries = Box::new(at(&path, root.entries(DirectoryScope::Root))?);
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
        let root = match Root::open(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ScanError::RootMissing);
            }
            result => at(&None, result)?,
        };
        let identity = at(&None, root.root_metadata())?.identity();
        Ok(Self { root, identity })
    }

    pub fn relation_to(&self, other: &Self) -> Result<RootRelation, ScanError> {
        at(&None, self.root.relation_to(&other.root))
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

    /// Bytes an unprivileged writer can still use on the root's volume.
    pub fn available_space(&self) -> Result<u64, ScanError> {
        at(&None, self.root.available_space())
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
        let observed = at(&None, self.root.root_metadata())?;
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
        let root = at(&None, self.root.try_clone())?;
        if at(&None, root.root_metadata())?.identity() != self.identity {
            return Err(ScanError::Changed(None));
        }
        let mut directories = vec![Directory::open(root, None)?];
        let mut stats = ScanStats::default();

        while let Some(directory) = directories.last_mut() {
            let Some(entry) = directory.entries.next() else {
                if at(&directory.path, directory.root.root_metadata())? != directory.before {
                    return Err(ScanError::Changed(directory.path.clone()));
                }
                let finished = directories.pop().expect("the current directory exists");
                if let Some(parent) = directories.last() {
                    let path = finished.path.as_ref().expect("a descendant has a path");
                    let name = SyncPath::try_from(path.file_name())?;
                    if at(&finished.path, parent.root.metadata(&name))? != finished.before {
                        return Err(ScanError::Changed(finished.path));
                    }
                }
                continue;
            };
            let entry = at(&directory.path, entry)?;

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
            if filter.excludes(&path, entry.metadata().kind() == NodeKind::Directory) {
                stats.skipped += 1;
                continue;
            }
            if entry.name().starts_with(b"._") {
                return Err(ScanError::UnclassifiedSidecar(path));
            }
            let at_path = Some(path.clone());
            let name = SyncPath::try_from(entry.name())?;
            let metadata = entry.metadata().clone();
            // An unavailable entry is not absence. Stop rather than publish a
            // complete inventory that could turn skipped placeholders into deletes.
            reject_dataless(metadata.data_residency(), &at_path)?;
            if metadata.identity().device() != self.identity.device() {
                return Err(ScanError::NestedFilesystem(path));
            }
            if metadata.kind() == NodeKind::Other {
                return Err(ScanError::UnsupportedEntry(path));
            }
            let link_target = if metadata.kind() == NodeKind::Symlink {
                Some(at(&at_path, directory.root.read_link(&name))?)
            } else {
                None
            };
            if at(&at_path, directory.root.metadata(&name))? != metadata {
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
            let child = at(&at_path, directory.root.open_directory(&name))?;
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
        let mut entries = Vec::new();
        let stats = self.scan_filtered(filter, &mut |entry| {
            entries.push(entry);
            control()
        })?;

        let mut observation = Observation {
            entries: BTreeMap::new(),
            leftovers: stats.leftovers,
        };
        for entry in entries {
            if control() == ScanControl::Cancel {
                return Err(ScanError::Cancelled);
            }
            if let Some(known) = previous.get(&entry.path)
                && known.fingerprint.proves_unchanged(&entry.metadata)
            {
                observation.entries.insert(entry.path, known.clone());
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
    fn optional_extended_metadata(
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
    fn entry_at(&self, path: &SyncPath) -> Result<Option<LocalEntry>, ScanError> {
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
fn observed<'a>(
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

fn metadata_error(path: Option<SyncPath>, source: MetadataReadError) -> ScanError {
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

fn child_path(parent: Option<&SyncPath>, name: &[u8]) -> Result<SyncPath, InvalidSyncPath> {
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

fn protected(name: &[u8]) -> bool {
    // Reserve ASCII aliases too: another endpoint may be case-insensitive.
    name.eq_ignore_ascii_case(INTERNAL_DIRECTORY)
        || name.eq_ignore_ascii_case(FINDER_STATE)
        || temporary(name)
}

fn temporary(name: &[u8]) -> bool {
    name.get(..TEMPORARY_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(TEMPORARY_PREFIX))
}

fn at<T>(path: &Option<SyncPath>, result: io::Result<T>) -> Result<T, ScanError> {
    result.map_err(|source| ScanError::Io {
        path: path.clone(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    #[test]
    fn dataless_roots_and_entries_cannot_complete_an_inventory() {
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
        assert!(matches!(result, Err(ScanError::Io { .. })));
        assert_eq!(
            paths,
            [SyncPath::try_from(b"directory".as_slice()).unwrap()]
        );
    }

    #[test]
    fn disappearing_entries_and_special_items_are_errors_not_empty_trees() {
        let temporary = tempfile::tempdir().unwrap();
        fs::create_dir(temporary.path().join("directory")).unwrap();
        let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
        let result = endpoint.scan_metadata(&mut |_| {
            fs::remove_dir(temporary.path().join("directory")).unwrap();
            ScanControl::Continue
        });
        assert!(matches!(result, Err(ScanError::Io { .. })));

        let _socket = UnixListener::bind(temporary.path().join("socket")).unwrap();
        assert!(matches!(
            endpoint.scan_metadata(&mut |_| ScanControl::Continue),
            Err(ScanError::UnsupportedEntry(_)),
        ));
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
}
