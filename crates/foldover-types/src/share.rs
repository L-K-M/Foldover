use std::fmt;
use std::str::FromStr;

/// A folder one Mac shares with one paired Mac (ADR 0031). Opaque and
/// persisted; creation belongs to the storage driver.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ShareId([u8; Self::BYTE_LEN]);

impl ShareId {
    pub const BYTE_LEN: usize = 16;

    pub const fn from_bytes(bytes: [u8; Self::BYTE_LEN]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; Self::BYTE_LEN] {
        &self.0
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct InvalidShareId;

impl fmt::Display for InvalidShareId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("A share ID is 32 lowercase hex digits")
    }
}

impl std::error::Error for InvalidShareId {}

impl FromStr for ShareId {
    type Err = InvalidShareId;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.as_bytes();
        if text.len() != Self::BYTE_LEN * 2 {
            return Err(InvalidShareId);
        }
        let digit = |value: u8| match value {
            b'0'..=b'9' => Ok(value - b'0'),
            b'a'..=b'f' => Ok(value - b'a' + 10),
            _ => Err(InvalidShareId),
        };
        let mut bytes = [0; Self::BYTE_LEN];
        let (pairs, _) = text.as_chunks::<2>();
        for (byte, pair) in bytes.iter_mut().zip(pairs) {
            *byte = digit(pair[0])? * 16 + digit(pair[1])?;
        }
        Ok(Self(bytes))
    }
}

impl fmt::Display for ShareId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_ids_round_trip_as_lowercase_hex_only() {
        let id = ShareId::from_bytes([0xab; ShareId::BYTE_LEN]);
        assert_eq!(id.to_string(), "ab".repeat(16));
        assert_eq!(id.to_string().parse(), Ok(id));
        for text in ["AB".repeat(16), "ab".repeat(15), "zz".repeat(16)] {
            assert_eq!(text.parse::<ShareId>(), Err(InvalidShareId));
        }
    }
}
