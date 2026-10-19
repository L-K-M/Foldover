use std::fmt;

/// Written into an endpoint root on its first sync, so a different folder or
/// disk mounted at the same path is noticed before anything is changed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EndpointId([u8; Self::BYTE_LEN]);

impl EndpointId {
    pub const BYTE_LEN: usize = 16;

    pub const fn from_bytes(bytes: [u8; Self::BYTE_LEN]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; Self::BYTE_LEN] {
        &self.0
    }

    /// Parse exactly 32 lowercase hex digits.
    pub fn from_hex(text: &[u8]) -> Option<Self> {
        if text.len() != Self::BYTE_LEN * 2 {
            return None;
        }
        let digit = |value: u8| match value {
            b'0'..=b'9' => Some(value - b'0'),
            b'a'..=b'f' => Some(value - b'a' + 10),
            _ => None,
        };
        let mut bytes = [0; Self::BYTE_LEN];
        let (pairs, _) = text.as_chunks::<2>();
        for (byte, pair) in bytes.iter_mut().zip(pairs) {
            *byte = digit(pair[0])? * 16 + digit(pair[1])?;
        }
        Some(Self(bytes))
    }
}

impl fmt::Display for EndpointId {
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
        let id = EndpointId::from_bytes([0xa5; EndpointId::BYTE_LEN]);
        assert_eq!(EndpointId::from_hex(id.to_string().as_bytes()), Some(id));
        for text in [b"".as_slice(), b"a5", &[b'A'; 32], &[b'g'; 32], &[b'0'; 33]] {
            assert_eq!(EndpointId::from_hex(text), None);
        }
    }
}
