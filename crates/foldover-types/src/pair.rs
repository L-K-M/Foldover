use std::fmt;

use crate::SyncMode;

/// Opaque persisted identity. Creation belongs to the storage driver.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PairId([u8; Self::BYTE_LEN]);

impl PairId {
    pub const BYTE_LEN: usize = 16;

    pub fn from_bytes(bytes: [u8; Self::BYTE_LEN]) -> Result<Self, InvalidPair> {
        if bytes == [0; Self::BYTE_LEN] {
            return Err(InvalidPair::Identity);
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; Self::BYTE_LEN] {
        &self.0
    }
}

impl fmt::Display for PairId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Selected absolute POSIX root bytes. This validates syntax only; endpoint
/// identity, overlap, availability and permissions still require driver probes.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LocalRoot(Vec<u8>);

impl LocalRoot {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, InvalidPair> {
        if bytes.first() != Some(&b'/') || bytes.contains(&b'\0') {
            return Err(InvalidPair::Root);
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairDraft {
    name: String,
    left: LocalRoot,
    right: LocalRoot,
    mode: SyncMode,
}

impl PairDraft {
    pub fn new(
        name: String,
        left: LocalRoot,
        right: LocalRoot,
        mode: SyncMode,
    ) -> Result<Self, InvalidPair> {
        if name.trim().is_empty() || name.contains('\0') {
            return Err(InvalidPair::Name);
        }
        if left == right {
            return Err(InvalidPair::IdenticalRoots);
        }
        Ok(Self {
            name,
            left,
            right,
            mode,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn left(&self) -> &LocalRoot {
        &self.left
    }
    pub fn right(&self) -> &LocalRoot {
        &self.right
    }
    pub fn mode(&self) -> SyncMode {
        self.mode
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairConfig {
    id: PairId,
    revision: u64,
    draft: PairDraft,
}

impl PairConfig {
    pub fn from_stored(id: PairId, revision: u64, draft: PairDraft) -> Result<Self, InvalidPair> {
        if revision == 0 || revision > i64::MAX as u64 {
            return Err(InvalidPair::Revision);
        }
        Ok(Self {
            id,
            revision,
            draft,
        })
    }

    pub fn id(&self) -> PairId {
        self.id
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn draft(&self) -> &PairDraft {
        &self.draft
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidPair {
    Identity,
    Name,
    Root,
    IdenticalRoots,
    Revision,
}

impl fmt::Display for InvalidPair {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Identity => "Pair identity must not be empty",
            Self::Name => "Pair name must contain text and no NUL characters",
            Self::Root => "Local root must be absolute and contain no NUL bytes",
            Self::IdenticalRoots => "A pair needs two different selected roots",
            Self::Revision => "Pair revision is outside the supported range",
        })
    }
}

impl std::error::Error for InvalidPair {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_configuration_and_preserves_raw_roots() {
        assert!(PairId::from_bytes([0; PairId::BYTE_LEN]).is_err());
        for root in [b"".as_slice(), b"relative", b"/bad\0root"] {
            assert!(LocalRoot::from_bytes(root.to_vec()).is_err());
        }
        let left = LocalRoot::from_bytes(b"/left/\xff".to_vec()).unwrap();
        let right = LocalRoot::from_bytes(b"/right".to_vec()).unwrap();
        assert_eq!(left.as_bytes(), b"/left/\xff");
        assert!(PairDraft::new(" \n".into(), left.clone(), right.clone(), SyncMode::Sync).is_err());
        assert!(PairDraft::new("pair".into(), left.clone(), left.clone(), SyncMode::Sync).is_err());
        let draft = PairDraft::new("pair".into(), left, right, SyncMode::Sync).unwrap();
        let id = PairId::from_bytes([1; PairId::BYTE_LEN]).unwrap();
        assert!(PairConfig::from_stored(id, 0, draft.clone()).is_err());
        assert!(PairConfig::from_stored(id, u64::MAX, draft).is_err());
    }
}
