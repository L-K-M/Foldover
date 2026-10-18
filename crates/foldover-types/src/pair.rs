use std::{fmt, str::FromStr};

use crate::{PairOptions, SyncMode};

pub const MAX_PAIR_NAME_BYTES: usize = 1024;
pub const MAX_LOCAL_ROOT_BYTES: usize = 4096;

/// Opaque persisted identity. Creation belongs to the storage driver.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
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

impl FromStr for PairId {
    type Err = InvalidPair;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != Self::BYTE_LEN * 2 {
            return Err(InvalidPair::Identity);
        }
        let mut bytes = [0; Self::BYTE_LEN];
        let (digits, _) = value.as_bytes().as_chunks::<2>();
        for (byte, digits) in bytes.iter_mut().zip(digits) {
            let digit = |value: u8| match value {
                b'0'..=b'9' => Ok(value - b'0'),
                b'a'..=b'f' => Ok(value - b'a' + 10),
                _ => Err(InvalidPair::Identity),
            };
            *byte = digit(digits[0])? * 16 + digit(digits[1])?;
        }
        Self::from_bytes(bytes)
    }
}

/// Selected absolute POSIX root bytes. This validates syntax only; endpoint
/// identity, overlap, availability and permissions still require driver probes.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LocalRoot(Vec<u8>);

impl LocalRoot {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, InvalidPair> {
        if bytes.first() != Some(&b'/')
            || bytes.contains(&b'\0')
            || bytes.len() > MAX_LOCAL_ROOT_BYTES
        {
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
    options: PairOptions,
}

impl PairDraft {
    pub fn new(
        name: String,
        left: LocalRoot,
        right: LocalRoot,
        mode: SyncMode,
    ) -> Result<Self, InvalidPair> {
        if name.trim().is_empty() || name.contains('\0') || name.len() > MAX_PAIR_NAME_BYTES {
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
            options: PairOptions::default(),
        })
    }

    pub fn with_options(self, options: PairOptions) -> Self {
        Self { options, ..self }
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
    pub fn options(&self) -> &PairOptions {
        &self.options
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
    Schedule,
    Filters,
}

impl fmt::Display for InvalidPair {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Identity => "Pair identity must be 32 lowercase hex digits and not all zero",
            Self::Name => "Pair name needs text, no NUL characters and at most 1024 bytes",
            Self::Root => "Local root must be absolute, without NUL bytes, and at most 4096 bytes",
            Self::IdenticalRoots => "A pair needs two different selected roots",
            Self::Revision => "Pair revision is outside the supported range",
            Self::Schedule => "A sync interval must be from 1 minute to 1 week",
            Self::Filters => "Filters allow at most 256 single-line rules of at most 1024 bytes",
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

    #[test]
    fn identity_text_round_trips_and_rejects_noncanonical_input() {
        let id = PairId::from_bytes([0xab; PairId::BYTE_LEN]).unwrap();
        assert_eq!(id.to_string().parse(), Ok(id));
        for text in ["", "00", &"0".repeat(32), &"A".repeat(32), &"é".repeat(16)] {
            assert!(text.parse::<PairId>().is_err());
        }
    }

    #[test]
    fn configuration_sizes_bound_wire_responses() {
        let left = LocalRoot::from_bytes(b"/left".to_vec()).unwrap();
        let right = LocalRoot::from_bytes(b"/right".to_vec()).unwrap();
        assert!(
            PairDraft::new(
                "a".repeat(MAX_PAIR_NAME_BYTES),
                left.clone(),
                right.clone(),
                SyncMode::Sync
            )
            .is_ok()
        );
        assert!(
            PairDraft::new(
                "a".repeat(MAX_PAIR_NAME_BYTES + 1),
                left,
                right,
                SyncMode::Sync
            )
            .is_err()
        );
        assert!(LocalRoot::from_bytes(vec![b'/'; MAX_LOCAL_ROOT_BYTES]).is_ok());
        assert!(LocalRoot::from_bytes(vec![b'/'; MAX_LOCAL_ROOT_BYTES + 1]).is_err());
    }
}
