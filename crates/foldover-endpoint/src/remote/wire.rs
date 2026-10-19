//! What crosses the wire for a shared folder beyond the JSON messages:
//! observed entries in binary batches, and errors both ways.
//!
//! ```text
//! entry := path length (u32 BE) path
//!          kind (u8: 1 file, 2 folder, 3 link) content (32) metadata (32)
//!          fingerprint length (u16 BE) fingerprint
//! ```

use std::io;

use foldover_proto::peer::{FolderError, FolderErrorKind};
use foldover_types::{Digest, EntryContent, EntryVersion, SyncPath};

use crate::{ApplyError, ObservedEntry, ScanError};

const FILE: u8 = 1;
const DIRECTORY: u8 = 2;
const SYMLINK: u8 = 3;
/// A batch is sent once it holds this many bytes.
pub(super) const BATCH_BYTES: usize = 256 * 1024;

pub(super) fn put_entry(batch: &mut Vec<u8>, path: &SyncPath, entry: &ObservedEntry) {
    let path = path.as_bytes();
    let fingerprint = entry.fingerprint_bytes();
    batch.extend_from_slice(&u32::try_from(path.len()).unwrap_or(u32::MAX).to_be_bytes());
    batch.extend_from_slice(path);
    let version = entry.version();
    let (kind, content) = match version.content() {
        EntryContent::File(digest) => (FILE, *digest.as_bytes()),
        EntryContent::Directory => (DIRECTORY, [0; Digest::BYTE_LEN]),
        EntryContent::Symlink(digest) => (SYMLINK, *digest.as_bytes()),
    };
    batch.push(kind);
    batch.extend_from_slice(&content);
    batch.extend_from_slice(version.metadata().as_bytes());
    batch.extend_from_slice(
        &u16::try_from(fingerprint.len())
            .unwrap_or(u16::MAX)
            .to_be_bytes(),
    );
    batch.extend_from_slice(&fingerprint);
}

/// Every entry in a batch, or None if any is malformed.
pub(super) fn entries(mut batch: &[u8]) -> Option<Vec<(SyncPath, ObservedEntry)>> {
    let mut entries = Vec::new();
    while !batch.is_empty() {
        let path_len = u32::from_be_bytes(take(&mut batch, 4)?.try_into().ok()?) as usize;
        let path = SyncPath::try_from(take(&mut batch, path_len)?).ok()?;
        let kind = take(&mut batch, 1)?[0];
        let content = Digest::from_bytes(take(&mut batch, Digest::BYTE_LEN)?.try_into().ok()?);
        let metadata = Digest::from_bytes(take(&mut batch, Digest::BYTE_LEN)?.try_into().ok()?);
        let content = match kind {
            FILE => EntryContent::File(content),
            DIRECTORY => EntryContent::Directory,
            SYMLINK => EntryContent::Symlink(content),
            _ => return None,
        };
        let fingerprint_len = u16::from_be_bytes(take(&mut batch, 2)?.try_into().ok()?) as usize;
        let fingerprint = take(&mut batch, fingerprint_len)?;
        let entry = ObservedEntry::restore(EntryVersion::new(content, metadata), fingerprint)?;
        entries.push((path, entry));
    }
    Some(entries)
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

/// A write failure on the answering Mac, for the asking one.
pub(super) fn apply_failure(error: &ApplyError) -> FolderError {
    match error {
        ApplyError::Scan(error) => scan_failure(error),
        error => FolderError {
            kind: FolderErrorKind::Other,
            path: None,
            message: error.to_string(),
        },
    }
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
