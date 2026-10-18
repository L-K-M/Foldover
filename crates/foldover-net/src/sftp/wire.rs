//! SFTP version 3 wire format (draft-ietf-secsh-filexfer-02): packets of
//! a length, a type and big-endian fields. Names are raw bytes.

use std::io;

pub(super) const INIT: u8 = 1;
pub(super) const VERSION: u8 = 2;
pub(super) const OPEN: u8 = 3;
pub(super) const CLOSE: u8 = 4;
pub(super) const READ: u8 = 5;
pub(super) const WRITE: u8 = 6;
pub(super) const LSTAT: u8 = 7;
pub(super) const FSETSTAT: u8 = 10;
pub(super) const SETSTAT: u8 = 9;
pub(super) const OPENDIR: u8 = 11;
pub(super) const READDIR: u8 = 12;
pub(super) const REMOVE: u8 = 13;
pub(super) const MKDIR: u8 = 14;
pub(super) const RMDIR: u8 = 15;
pub(super) const REALPATH: u8 = 16;
pub(super) const RENAME: u8 = 18;
pub(super) const READLINK: u8 = 19;
pub(super) const SYMLINK: u8 = 20;
pub(super) const STATUS: u8 = 101;
pub(super) const HANDLE: u8 = 102;
pub(super) const DATA: u8 = 103;
pub(super) const NAME: u8 = 104;
pub(super) const ATTRS: u8 = 105;
pub(super) const EXTENDED: u8 = 200;
pub(super) const EXTENDED_REPLY: u8 = 201;

pub(super) const STATUS_OK: u32 = 0;
pub(super) const STATUS_EOF: u32 = 1;
pub(super) const STATUS_NO_SUCH_FILE: u32 = 2;
pub(super) const STATUS_PERMISSION_DENIED: u32 = 3;
pub(super) const STATUS_OP_UNSUPPORTED: u32 = 8;

pub(super) const OPEN_READ: u32 = 0x01;
pub(super) const OPEN_WRITE: u32 = 0x02;
pub(super) const OPEN_CREATE: u32 = 0x08;
pub(super) const OPEN_EXCLUSIVE: u32 = 0x20;

const ATTR_SIZE: u32 = 0x01;
const ATTR_UIDGID: u32 = 0x02;
const ATTR_PERMISSIONS: u32 = 0x04;
const ATTR_ACMODTIME: u32 = 0x08;
const ATTR_EXTENDED: u32 = 0x8000_0000;

/// File type bits of `permissions`, as in `stat(2)`.
const TYPE_MASK: u32 = 0o170_000;
const TYPE_DIRECTORY: u32 = 0o040_000;
const TYPE_FILE: u32 = 0o100_000;
const TYPE_SYMLINK: u32 = 0o120_000;

/// An entry's kind, from its permission bits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// What a server reports about an entry; any field may be missing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Attributes {
    pub size: Option<u64>,
    pub owner: Option<(u32, u32)>,
    pub permissions: Option<u32>,
    /// Access and modification times, whole seconds since the epoch.
    pub times: Option<(u32, u32)>,
}

impl Attributes {
    pub fn kind(&self) -> FileKind {
        match self.permissions.map(|mode| mode & TYPE_MASK) {
            Some(TYPE_FILE) => FileKind::File,
            Some(TYPE_DIRECTORY) => FileKind::Directory,
            Some(TYPE_SYMLINK) => FileKind::Symlink,
            _ => FileKind::Other,
        }
    }

    pub fn modified(&self) -> Option<u32> {
        self.times.map(|(_, modified)| modified)
    }
}

/// A request body under construction.
pub(super) struct Writer(Vec<u8>);

impl Writer {
    pub(super) fn new(kind: u8, id: u32) -> Self {
        let mut bytes = vec![0; 4];
        bytes.push(kind);
        bytes.extend_from_slice(&id.to_be_bytes());
        Self(bytes)
    }

    pub(super) fn init(version: u32) -> Self {
        let mut bytes = vec![0; 4];
        bytes.push(INIT);
        bytes.extend_from_slice(&version.to_be_bytes());
        Self(bytes)
    }

    pub(super) fn u32(mut self, value: u32) -> Self {
        self.0.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub(super) fn u64(mut self, value: u64) -> Self {
        self.0.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub(super) fn bytes(mut self, value: &[u8]) -> Self {
        let length = u32::try_from(value.len()).expect("SFTP fields stay below 4 GiB");
        self.0.extend_from_slice(&length.to_be_bytes());
        self.0.extend_from_slice(value);
        self
    }

    pub(super) fn attributes(mut self, attributes: &Attributes) -> Self {
        let mut flags = 0;
        if attributes.size.is_some() {
            flags |= ATTR_SIZE;
        }
        if attributes.owner.is_some() {
            flags |= ATTR_UIDGID;
        }
        if attributes.permissions.is_some() {
            flags |= ATTR_PERMISSIONS;
        }
        if attributes.times.is_some() {
            flags |= ATTR_ACMODTIME;
        }
        self = self.u32(flags);
        if let Some(size) = attributes.size {
            self = self.u64(size);
        }
        if let Some((uid, gid)) = attributes.owner {
            self = self.u32(uid).u32(gid);
        }
        if let Some(permissions) = attributes.permissions {
            self = self.u32(permissions);
        }
        if let Some((accessed, modified)) = attributes.times {
            self = self.u32(accessed).u32(modified);
        }
        self
    }

    /// The finished packet, length prefix included.
    pub(super) fn finish(mut self) -> Vec<u8> {
        let length = u32::try_from(self.0.len() - 4).expect("SFTP packets stay below 4 GiB");
        self.0[..4].copy_from_slice(&length.to_be_bytes());
        self.0
    }
}

/// A response body being read.
pub(super) struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn take(&mut self, count: usize) -> io::Result<&'a [u8]> {
        if self.0.len() < count {
            return Err(malformed());
        }
        let (taken, rest) = self.0.split_at(count);
        self.0 = rest;
        Ok(taken)
    }

    pub(super) fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub(super) fn u32(&mut self) -> io::Result<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes(bytes.try_into().expect("four bytes")))
    }

    pub(super) fn u64(&mut self) -> io::Result<u64> {
        let bytes = self.take(8)?;
        Ok(u64::from_be_bytes(bytes.try_into().expect("eight bytes")))
    }

    pub(super) fn bytes(&mut self) -> io::Result<&'a [u8]> {
        let length = usize::try_from(self.u32()?).map_err(|_| malformed())?;
        self.take(length)
    }

    pub(super) fn attributes(&mut self) -> io::Result<Attributes> {
        let flags = self.u32()?;
        let mut attributes = Attributes::default();
        if flags & ATTR_SIZE != 0 {
            attributes.size = Some(self.u64()?);
        }
        if flags & ATTR_UIDGID != 0 {
            attributes.owner = Some((self.u32()?, self.u32()?));
        }
        if flags & ATTR_PERMISSIONS != 0 {
            attributes.permissions = Some(self.u32()?);
        }
        if flags & ATTR_ACMODTIME != 0 {
            attributes.times = Some((self.u32()?, self.u32()?));
        }
        if flags & ATTR_EXTENDED != 0 {
            for _ in 0..self.u32()? {
                self.bytes()?;
                self.bytes()?;
            }
        }
        Ok(attributes)
    }
}

pub(super) fn malformed() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Malformed SFTP response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_round_trip_with_only_present_fields() {
        let attributes = Attributes {
            size: Some(7),
            owner: None,
            permissions: Some(TYPE_FILE | 0o644),
            times: Some((1, 2)),
        };
        let packet = Writer::new(ATTRS, 9).attributes(&attributes).finish();
        assert_eq!(
            u32::from_be_bytes(packet[..4].try_into().unwrap()) as usize,
            packet.len() - 4
        );
        let mut reader = Reader::new(&packet[4..]);
        assert_eq!(reader.u8().unwrap(), ATTRS);
        assert_eq!(reader.u32().unwrap(), 9);
        let decoded = reader.attributes().unwrap();
        assert_eq!(decoded, attributes);
        assert_eq!(decoded.kind(), FileKind::File);
        assert!(reader.is_empty());
        assert!(Reader::new(&[0, 0]).u32().is_err());
    }
}
