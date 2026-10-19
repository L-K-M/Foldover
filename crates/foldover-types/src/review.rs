use std::{fmt, str::FromStr};

/// A semantic fingerprint of one pair's exact preview: its identity,
/// revision, every planned item's action and versions, and its counts.
/// The engine computes it; clients hold it opaquely and present it back to
/// prove which plan they reviewed. Wire form is 64 lowercase hex digits.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReviewToken([u8; Self::BYTE_LEN]);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidReviewToken;

impl ReviewToken {
    pub const BYTE_LEN: usize = 32;

    /// An all-zero token never matches a computed fingerprint, so it is
    /// rejected rather than acting as a wildcard or default.
    pub fn from_bytes(bytes: [u8; Self::BYTE_LEN]) -> Result<Self, InvalidReviewToken> {
        if bytes == [0; Self::BYTE_LEN] {
            return Err(InvalidReviewToken);
        }
        Ok(Self(bytes))
    }

    pub const fn as_bytes(&self) -> &[u8; Self::BYTE_LEN] {
        &self.0
    }
}

impl fmt::Display for ReviewToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for ReviewToken {
    type Err = InvalidReviewToken;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != Self::BYTE_LEN * 2 {
            return Err(InvalidReviewToken);
        }
        let mut bytes = [0; Self::BYTE_LEN];
        let (digits, _) = value.as_bytes().as_chunks::<2>();
        for (byte, digits) in bytes.iter_mut().zip(digits) {
            let digit = |value: u8| match value {
                b'0'..=b'9' => Ok(value - b'0'),
                b'a'..=b'f' => Ok(value - b'a' + 10),
                _ => Err(InvalidReviewToken),
            };
            *byte = digit(digits[0])? * 16 + digit(digits[1])?;
        }
        Self::from_bytes(bytes)
    }
}

impl fmt::Display for InvalidReviewToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("A review token must be 64 lowercase hex digits")
    }
}

impl std::error::Error for InvalidReviewToken {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_text_round_trips_and_rejects_noncanonical_input() {
        assert!(ReviewToken::from_bytes([0; ReviewToken::BYTE_LEN]).is_err());
        let token = ReviewToken::from_bytes([0xab; ReviewToken::BYTE_LEN]).unwrap();
        assert_eq!(token.to_string().parse(), Ok(token));
        for text in [
            "",
            "00",
            &"0".repeat(64),
            &"A".repeat(64),
            &"é".repeat(32),
            &"0".repeat(66),
        ] {
            assert!(text.parse::<ReviewToken>().is_err(), "{text}");
        }
    }
}
