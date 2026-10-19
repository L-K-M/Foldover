use std::fmt;
use std::str::FromStr;

/// Another Mac running Foldover, named by its certificate fingerprint
/// (PLAN.md §4.11): pairing pins it, and every later connection must
/// present the same certificate.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeviceId([u8; Self::BYTE_LEN]);

impl DeviceId {
    pub const BYTE_LEN: usize = 32;

    pub const fn from_bytes(bytes: [u8; Self::BYTE_LEN]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; Self::BYTE_LEN] {
        &self.0
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct InvalidDeviceId;

impl fmt::Display for InvalidDeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("A device ID is 64 lowercase hex digits")
    }
}

impl std::error::Error for InvalidDeviceId {}

impl FromStr for DeviceId {
    type Err = InvalidDeviceId;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.as_bytes();
        if text.len() != Self::BYTE_LEN * 2 {
            return Err(InvalidDeviceId);
        }
        let digit = |value: u8| match value {
            b'0'..=b'9' => Ok(value - b'0'),
            b'a'..=b'f' => Ok(value - b'a' + 10),
            _ => Err(InvalidDeviceId),
        };
        let mut bytes = [0; Self::BYTE_LEN];
        let (pairs, _) = text.as_chunks::<2>();
        for (byte, pair) in bytes.iter_mut().zip(pairs) {
            *byte = digit(pair[0])? * 16 + digit(pair[1])?;
        }
        Ok(Self(bytes))
    }
}

impl fmt::Display for DeviceId {
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
    fn hex_round_trips_and_rejects_other_text() {
        let id = DeviceId::from_bytes([0xa5; DeviceId::BYTE_LEN]);
        assert_eq!(id.to_string().parse(), Ok(id));
        for text in ["", "a5", &"A".repeat(64), &"g".repeat(64), &"0".repeat(65)] {
            assert_eq!(text.parse::<DeviceId>(), Err(InvalidDeviceId));
        }
    }
}
