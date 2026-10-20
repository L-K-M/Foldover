//! What crosses the wire for a shared folder beyond the JSON messages:
//! observed entries in binary batches, and errors both ways.
//!
//! ```text
//! entry := path length (u32 BE) path
//!          kind (u8: 1 file, 2 folder, 3 link) content (32) metadata (32)
//!          fingerprint length (u16 BE) fingerprint
//! ```

use std::io;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use foldover_fs::{ForeignMetadata, NativeMetadata};
use foldover_proto::peer::{
    FolderError, FolderErrorKind, RunSettings, WireAttribute, WireMetadata,
};
use foldover_types::{Digest, EntryContent, EntryVersion, PairOptions, SyncPath};

use crate::{ApplyError, ObservedEntry, RunScope, ScanError};

const FILE: u8 = 1;
const DIRECTORY: u8 = 2;
const SYMLINK: u8 = 3;
/// A batch is sent once it holds this many bytes.
pub(super) const BATCH_BYTES: usize = 256 * 1024;

pub(super) fn put_entry(batch: &mut Vec<u8>, path: &SyncPath, entry: &ObservedEntry) {
    let path = path.as_bytes();
    batch.extend_from_slice(&u32::try_from(path.len()).unwrap_or(u32::MAX).to_be_bytes());
    batch.extend_from_slice(path);
    put_observed(batch, entry);
}

/// Every entry in a batch, or None if any is malformed.
pub(super) fn entries(mut batch: &[u8]) -> Option<Vec<(SyncPath, ObservedEntry)>> {
    let mut entries = Vec::new();
    while !batch.is_empty() {
        let path_len = u32::from_be_bytes(take(&mut batch, 4)?.try_into().ok()?) as usize;
        let path = SyncPath::try_from(take(&mut batch, path_len)?).ok()?;
        entries.push((path, take_observed(&mut batch)?));
    }
    Some(entries)
}

/// One entry without its path, as a request names it.
pub(super) fn entry_bytes(entry: &ObservedEntry) -> Vec<u8> {
    let mut bytes = Vec::new();
    put_observed(&mut bytes, entry);
    bytes
}

pub(super) fn entry_from(mut bytes: &[u8]) -> Option<ObservedEntry> {
    let entry = take_observed(&mut bytes)?;
    bytes.is_empty().then_some(entry)
}

pub(super) fn version_bytes(version: EntryVersion) -> Vec<u8> {
    let mut bytes = Vec::new();
    put_version(&mut bytes, version);
    bytes
}

pub(super) fn version_from(mut bytes: &[u8]) -> Option<EntryVersion> {
    let version = take_version(&mut bytes)?;
    bytes.is_empty().then_some(version)
}

fn put_observed(bytes: &mut Vec<u8>, entry: &ObservedEntry) {
    let fingerprint = entry.fingerprint_bytes();
    put_version(bytes, entry.version());
    bytes.extend_from_slice(
        &u16::try_from(fingerprint.len())
            .unwrap_or(u16::MAX)
            .to_be_bytes(),
    );
    bytes.extend_from_slice(&fingerprint);
}

fn take_observed(bytes: &mut &[u8]) -> Option<ObservedEntry> {
    let version = take_version(bytes)?;
    let fingerprint_len = u16::from_be_bytes(take(bytes, 2)?.try_into().ok()?) as usize;
    ObservedEntry::restore(version, take(bytes, fingerprint_len)?)
}

fn put_version(bytes: &mut Vec<u8>, version: EntryVersion) {
    let (kind, content) = match version.content() {
        EntryContent::File(digest) => (FILE, *digest.as_bytes()),
        EntryContent::Directory => (DIRECTORY, [0; Digest::BYTE_LEN]),
        EntryContent::Symlink(digest) => (SYMLINK, *digest.as_bytes()),
    };
    bytes.push(kind);
    bytes.extend_from_slice(&content);
    bytes.extend_from_slice(version.metadata().as_bytes());
}

fn take_version(bytes: &mut &[u8]) -> Option<EntryVersion> {
    let kind = take(bytes, 1)?[0];
    let content = Digest::from_bytes(take(bytes, Digest::BYTE_LEN)?.try_into().ok()?);
    let metadata = Digest::from_bytes(take(bytes, Digest::BYTE_LEN)?.try_into().ok()?);
    let content = match kind {
        FILE => EntryContent::File(content),
        DIRECTORY => EntryContent::Directory,
        SYMLINK => EntryContent::Symlink(content),
        _ => return None,
    };
    Some(EntryVersion::new(content, metadata))
}

pub(super) fn metadata_wire(
    metadata: ForeignMetadata,
    native: Option<&NativeMetadata>,
) -> WireMetadata {
    let (seconds, nanos) = match metadata.modified.duration_since(UNIX_EPOCH) {
        Ok(after) => (
            i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
            after.subsec_nanos(),
        ),
        // Before 1970: whole seconds back, then nanoseconds forward.
        Err(before) => {
            let before = before.duration();
            let seconds = i64::try_from(before.as_secs()).unwrap_or(i64::MAX);
            match before.subsec_nanos() {
                0 => (-seconds, 0),
                nanos => (-seconds - 1, 1_000_000_000 - nanos),
            }
        }
    };
    WireMetadata {
        permissions: metadata.permissions,
        modified_seconds: seconds,
        modified_nanos: nanos,
        attributes: native.map(|native| {
            native
                .attributes
                .iter()
                .map(|(name, value)| WireAttribute {
                    name: name.clone(),
                    value: value.clone(),
                })
                .collect()
        }),
        flags: native.and_then(|native| native.flags),
    }
}

pub(super) fn metadata_from(metadata: WireMetadata) -> (ForeignMetadata, Option<NativeMetadata>) {
    let seconds = Duration::from_secs(metadata.modified_seconds.unsigned_abs());
    let base = if metadata.modified_seconds >= 0 {
        UNIX_EPOCH + seconds
    } else {
        UNIX_EPOCH - seconds
    };
    let nanos = Duration::from_nanos(u64::from(metadata.modified_nanos.min(999_999_999)));
    let foreign = ForeignMetadata {
        permissions: metadata.permissions,
        modified: base + nanos,
    };
    let flags = metadata.flags;
    let native = metadata.attributes.map(|attributes| NativeMetadata {
        attributes: attributes
            .into_iter()
            .map(|attribute| (attribute.name, attribute.value))
            .collect(),
        flags,
    });
    (foreign, native)
}

pub(super) fn run_wire(run: &RunScope) -> RunSettings {
    RunSettings {
        name: run.name().to_owned(),
        options: run.options().into(),
    }
}

/// A run another Mac named, or None for an unusable name or options.
pub(super) fn run_from(run: RunSettings) -> Option<RunScope> {
    let options = PairOptions::try_from(run.options).ok()?;
    RunScope::named(&run.name, options)
}

pub(super) fn milliseconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
    })
}

fn take<'a>(batch: &mut &'a [u8], count: usize) -> Option<&'a [u8]> {
    if batch.len() < count {
        return None;
    }
    let (taken, rest) = batch.split_at(count);
    *batch = rest;
    Some(taken)
}

/// A scan failure on the answering Mac, for the asking one.
pub(super) fn scan_failure(error: &ScanError) -> FolderError {
    let (kind, path) = match error {
        ScanError::RootMissing => (FolderErrorKind::Missing, None),
        ScanError::Cancelled => (FolderErrorKind::Cancelled, None),
        ScanError::Changed(path) => (FolderErrorKind::Changed, path.clone()),
        ScanError::InvalidIgnoreFile => (FolderErrorKind::Invalid, None),
        error if error.is_access_denied() => (FolderErrorKind::AccessDenied, None),
        _ => (FolderErrorKind::Other, None),
    };
    FolderError {
        kind,
        path: path.map(|path| path.as_bytes().to_vec()),
        message: error.to_string(),
    }
}

/// A write failure on the answering Mac, for the asking one. Either
/// side's entry having changed is `changed`; which side it was follows
/// from the operation.
pub(super) fn apply_failure(error: &ApplyError) -> FolderError {
    let (kind, path) = match error {
        ApplyError::Scan(error) => return scan_failure(error),
        ApplyError::SourceChanged(path) | ApplyError::DestinationChanged(path) => {
            (FolderErrorKind::Changed, Some(path.clone()))
        }
        _ => (FolderErrorKind::Other, None),
    };
    FolderError {
        kind,
        path: path.map(|path| path.as_bytes().to_vec()),
        message: error.to_string(),
    }
}

/// The answering Mac's write failure as this Mac's own; `changed` names
/// the side an operation reads or writes.
pub(super) fn apply_error(error: FolderError, changed: fn(SyncPath) -> ApplyError) -> ApplyError {
    if error.kind == FolderErrorKind::Changed
        && let Some(path) = error
            .path
            .as_deref()
            .and_then(|path| SyncPath::try_from(path).ok())
    {
        return changed(path);
    }
    ApplyError::Scan(scan_error(error))
}

/// The answering Mac's failure as this Mac's own scan error.
pub(super) fn scan_error(error: FolderError) -> ScanError {
    let path = error
        .path
        .and_then(|path| SyncPath::try_from(path.as_slice()).ok());
    match error.kind {
        FolderErrorKind::Missing => ScanError::RootMissing,
        FolderErrorKind::Cancelled => ScanError::Cancelled,
        FolderErrorKind::Changed => ScanError::Changed(path),
        FolderErrorKind::AccessDenied => ScanError::Io {
            path,
            source: io::Error::new(io::ErrorKind::PermissionDenied, error.message),
        },
        FolderErrorKind::Invalid | FolderErrorKind::Other => ScanError::Peer(error.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_batches_round_trip_and_reject_truncation() {
        let path = SyncPath::try_from(&b"photos/a.jpg"[..]).unwrap();
        let version = EntryVersion::new(
            EntryContent::File(Digest::from_bytes([7; Digest::BYTE_LEN])),
            Digest::from_bytes([9; Digest::BYTE_LEN]),
        );
        let entry = ObservedEntry::for_tests(version, 42);
        let folder = SyncPath::try_from(&b"photos"[..]).unwrap();
        let directory = ObservedEntry::for_tests(
            EntryVersion::new(EntryContent::Directory, Digest::from_bytes([1; 32])),
            0,
        );
        let mut batch = Vec::new();
        put_entry(&mut batch, &folder, &directory);
        put_entry(&mut batch, &path, &entry);

        let decoded = entries(&batch).unwrap();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[1].0, path);
        assert_eq!(decoded[1].1.version(), version);
        assert_eq!(decoded[1].1.size(), 42);
        assert_eq!(decoded[0].1.version().content(), EntryContent::Directory);

        assert!(entries(&batch[..batch.len() - 1]).is_none());
        let mut unknown = batch.clone();
        unknown[4 + b"photos".len()] = 9;
        assert!(entries(&unknown).is_none());
    }

    #[test]
    fn folder_errors_keep_their_kind_across_the_wire() {
        for error in [
            ScanError::RootMissing,
            ScanError::Cancelled,
            ScanError::Changed(Some(SyncPath::try_from(&b"a"[..]).unwrap())),
        ] {
            let returned = scan_error(scan_failure(&error));
            assert_eq!(returned.to_string(), error.to_string());
        }
        let denied = ScanError::Io {
            path: None,
            source: io::Error::new(io::ErrorKind::PermissionDenied, "no"),
        };
        assert!(scan_error(scan_failure(&denied)).is_access_denied());
        let other = scan_error(scan_failure(&ScanError::InvalidIdentity));
        assert!(matches!(other, ScanError::Peer(message) if message.contains("identity")));
    }
}
