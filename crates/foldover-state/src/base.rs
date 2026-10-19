//! Stored encoding of the per-path base.
//!
//! ```text
//! version = kind (1 byte) | content digest (32) | metadata digest (32)
//! kind    = 1 file, 2 directory (content all zero), 3 symlink
//! ```

use foldover_types::{BaselineEntry, Digest, EntryContent, EntryVersion, SyncPath};

use crate::Error;

const VERSION_BYTES: usize = 1 + 2 * Digest::BYTE_LEN;
const FILE: u8 = 1;
const DIRECTORY: u8 = 2;
const SYMLINK: u8 = 3;
const NO_CONTENT: [u8; Digest::BYTE_LEN] = [0; Digest::BYTE_LEN];

/// One path's change to the last-synced state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BaseUpdate {
    Record(SyncPath, BaselineEntry),
    Forget(SyncPath),
}

impl BaseUpdate {
    pub fn path(&self) -> &SyncPath {
        match self {
            Self::Record(path, _) | Self::Forget(path) => path,
        }
    }
}

pub(crate) fn encode(version: EntryVersion) -> [u8; VERSION_BYTES] {
    let (kind, content) = match version.content() {
        EntryContent::File(digest) => (FILE, *digest.as_bytes()),
        EntryContent::Directory => (DIRECTORY, NO_CONTENT),
        EntryContent::Symlink(digest) => (SYMLINK, *digest.as_bytes()),
    };
    let mut bytes = [0; VERSION_BYTES];
    bytes[0] = kind;
    bytes[1..=Digest::BYTE_LEN].copy_from_slice(&content);
    bytes[1 + Digest::BYTE_LEN..].copy_from_slice(version.metadata().as_bytes());
    bytes
}

pub(crate) fn decode(bytes: &[u8]) -> Result<EntryVersion, Error> {
    let bytes: &[u8; VERSION_BYTES] = bytes
        .try_into()
        .map_err(|_| Error::InvalidStored("base version length"))?;
    let digest = |range: std::ops::Range<usize>| {
        Digest::from_bytes(bytes[range].try_into().expect("fixed-size digest range"))
    };
    let content = digest(1..1 + Digest::BYTE_LEN);
    let metadata = digest(1 + Digest::BYTE_LEN..VERSION_BYTES);
    let content = match bytes[0] {
        FILE => EntryContent::File(content),
        DIRECTORY if *content.as_bytes() == NO_CONTENT => EntryContent::Directory,
        SYMLINK => EntryContent::Symlink(content),
        _ => return Err(Error::InvalidStored("base version kind")),
    };
    Ok(EntryVersion::new(content, metadata))
}

pub(crate) fn decode_entry(left: &[u8], right: &[u8]) -> Result<BaselineEntry, Error> {
    BaselineEntry::new(decode(left)?, decode(right)?)
        .map_err(|_| Error::InvalidStored("base sides disagree on content"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(content: EntryContent, metadata: u8) -> EntryVersion {
        EntryVersion::new(content, Digest::from_bytes([metadata; Digest::BYTE_LEN]))
    }

    #[test]
    fn every_kind_round_trips_exactly() {
        let digest = Digest::from_bytes([7; Digest::BYTE_LEN]);
        for content in [
            EntryContent::File(digest),
            EntryContent::Directory,
            EntryContent::Symlink(digest),
        ] {
            let version = version(content, 9);
            assert_eq!(decode(&encode(version)).unwrap(), version);
        }
    }

    #[test]
    fn malformed_versions_are_errors() {
        let file = encode(version(
            EntryContent::File(Digest::from_bytes([7; Digest::BYTE_LEN])),
            9,
        ));
        assert!(decode(&file[1..]).is_err());

        let mut unknown = file;
        unknown[0] = 4;
        assert!(decode(&unknown).is_err());

        let mut directory = file;
        directory[0] = DIRECTORY;
        assert!(decode(&directory).is_err());

        let left = encode(version(EntryContent::Directory, 1));
        assert!(decode_entry(&left, &file).is_err());
        assert!(decode_entry(&left, &encode(version(EntryContent::Directory, 2))).is_ok());
    }
}
