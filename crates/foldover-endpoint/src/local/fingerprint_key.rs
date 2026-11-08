//! Compact in-memory storage for fingerprint keys.
//!
//! A key is the exact byte string a fingerprint was built from. This module
//! keeps those bytes in a canonical encoding chosen by [`FingerprintKey::new`]
//! alone: a raw fallback (`[0] + raw`) or, for keys of at most 255 bytes with
//! enough zero bytes, a sparse form (`[len] + zero-bitmask + nonzero values`).
//! The sparse form is chosen only when it is strictly smaller, so equality on
//! the stored bytes is equality on the original key. No public constructor
//! accepts encoded bytes; untrusted input cannot reach the decoder.

use std::fmt;

/// The exact bytes of a fingerprint key in a canonical compact encoding.
///
/// The struct is one boxed slice: 16 bytes on a 64-bit target. An empty key
/// stores nothing; a non-empty raw fallback stores a leading zero byte
/// followed by the original bytes; a sparse key stores its one-byte original
/// length, a zero-bitmask with bit `i % 8` of mask byte `i / 8` set exactly
/// when original byte `i` is non-zero, and then the non-zero original bytes in
/// index order.
#[derive(Clone, Eq, PartialEq)]
pub(super) struct FingerprintKey {
    storage: Box<[u8]>,
}

impl FingerprintKey {
    /// Encodes `raw` into freshly allocated storage.
    ///
    /// The input is only read; the returned key never borrows or reuses it.
    pub(super) fn new(raw: &[u8]) -> Self {
        if raw.is_empty() {
            return Self {
                storage: Vec::new().into_boxed_slice(),
            };
        }
        if raw.len() <= u8::MAX as usize {
            let mut mask = [0u8; (u8::MAX as usize).div_ceil(8)];
            let mut nonzero = 0usize;
            for (index, &byte) in raw.iter().enumerate() {
                if byte != 0 {
                    mask[index / 8] |= 1 << (index % 8);
                    nonzero += 1;
                }
            }
            let mask_len = raw.len().div_ceil(8);
            // Sparse is smaller than the raw fallback exactly when the
            // bitmask and nonzero payload together are shorter than the raw
            // bytes; both forms carry a one-byte prefix.
            if mask_len + nonzero < raw.len() {
                let mut storage = Vec::with_capacity(1 + mask_len + nonzero);
                storage.push(raw.len() as u8);
                storage.extend_from_slice(&mask[..mask_len]);
                for &byte in raw {
                    if byte != 0 {
                        storage.push(byte);
                    }
                }
                return Self {
                    storage: storage.into_boxed_slice(),
                };
            }
        }
        let mut storage = Vec::with_capacity(raw.len() + 1);
        storage.push(0);
        storage.extend_from_slice(raw);
        Self {
            storage: storage.into_boxed_slice(),
        }
    }

    /// The original key length in bytes.
    pub(super) fn len(&self) -> usize {
        match self.storage.first().copied() {
            Some(0) => self.storage.len() - 1,
            Some(len) => len as usize,
            None => 0,
        }
    }

    /// Whether the original key is empty.
    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.storage.is_empty()
    }

    /// Whether `raw` is exactly the original key.
    pub(super) fn matches(&self, raw: &[u8]) -> bool {
        if raw.len() != self.len() {
            return false;
        }
        match self.storage.first().copied() {
            None => true,
            Some(0) => &self.storage[1..] == raw,
            Some(_) => {
                let mut buffer = [0u8; u8::MAX as usize];
                self.expand_sparse(&mut buffer);
                &buffer[..raw.len()] == raw
            }
        }
    }

    /// Appends the original key bytes to `out`.
    pub(super) fn append_to(&self, out: &mut Vec<u8>) {
        match self.storage.first().copied() {
            None => {}
            Some(0) => out.extend_from_slice(&self.storage[1..]),
            Some(_) => {
                let mut buffer = [0u8; u8::MAX as usize];
                self.expand_sparse(&mut buffer);
                out.extend_from_slice(&buffer[..self.len()]);
            }
        }
    }

    /// BLAKE3 of the original key bytes.
    pub(super) fn hash(&self) -> blake3::Hash {
        match self.storage.first().copied() {
            None => blake3::hash(b""),
            Some(0) => blake3::hash(&self.storage[1..]),
            Some(_) => {
                let mut buffer = [0u8; u8::MAX as usize];
                self.expand_sparse(&mut buffer);
                blake3::hash(&buffer[..self.len()])
            }
        }
    }

    /// Fills `buffer` with the original bytes of a sparse key.
    fn expand_sparse(&self, buffer: &mut [u8; u8::MAX as usize]) {
        let len = self.storage[0] as usize;
        let mask_len = len.div_ceil(8);
        let mut value_index = 1 + mask_len;
        for (index, slot) in buffer.iter_mut().enumerate().take(len) {
            if self.storage[1 + index / 8] & (1 << (index % 8)) != 0 {
                *slot = self.storage[value_index];
                value_index += 1;
            }
        }
    }

    /// Start of the encoded storage, for tests and profiling.
    #[cfg(test)]
    pub(super) fn as_ptr(&self) -> *const u8 {
        self.storage.as_ptr()
    }

    /// Length of the encoded storage, for tests and profiling.
    #[cfg(test)]
    pub(super) fn encoded_len(&self) -> usize {
        self.storage.len()
    }
}

impl fmt::Debug for FingerprintKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.storage.first().copied() {
            None => fmt::Debug::fmt(&self.storage[..], f),
            Some(0) => fmt::Debug::fmt(&self.storage[1..], f),
            Some(_) => {
                let mut buffer = [0u8; u8::MAX as usize];
                self.expand_sparse(&mut buffer);
                fmt::Debug::fmt(&buffer[..self.len()], f)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NATIVE_KEY_LEN: usize = 93;
    const NATIVE_NONZERO: usize = 40;
    const SFTP_KEY_LEN: usize = 28;

    fn native_style_key() -> Vec<u8> {
        let mut key = vec![0u8; NATIVE_KEY_LEN];
        for (index, byte) in key.iter_mut().enumerate() {
            if index < NATIVE_NONZERO {
                *byte = (index as u8).wrapping_add(1);
            }
        }
        key
    }

    fn sftp_style_key() -> Vec<u8> {
        let mut key = vec![0u8; SFTP_KEY_LEN];
        for (index, byte) in key.iter_mut().enumerate() {
            if index % 3 == 0 {
                *byte = (index as u8).wrapping_add(1);
            }
        }
        key
    }

    fn repeated_blocks() -> Vec<u8> {
        let mut block = [0u8; 64];
        for (index, byte) in block.iter_mut().enumerate() {
            if index % 5 == 0 {
                *byte = index as u8;
            }
        }
        block.iter().cycle().take(256).copied().collect()
    }

    #[test]
    fn layout_is_one_box() {
        assert_eq!(std::mem::size_of::<FingerprintKey>(), 16);
    }

    #[test]
    fn empty_key_stores_nothing_and_roundtrips() {
        let key = FingerprintKey::new(&[]);
        assert!(key.is_empty());
        assert_eq!(key.len(), 0);
        assert_eq!(key.encoded_len(), 0);
        assert!(key.matches(&[]));
        assert!(!key.matches(&[0]));
        assert_eq!(key.hash(), blake3::hash(b""));
        assert_eq!(format!("{:?}", key), "[]");
        let mut out = b"prefix".to_vec();
        key.append_to(&mut out);
        assert_eq!(out, b"prefix");
    }

    #[test]
    fn one_byte_keys_roundtrip() {
        for byte in 0..=255u8 {
            let raw = [byte];
            let key = FingerprintKey::new(&raw);
            assert_eq!(key.len(), 1);
            assert!(!key.is_empty());
            assert_eq!(key.encoded_len(), 2);
            assert!(key.matches(&raw));
            assert_eq!(key.hash(), blake3::hash(&raw));
            let mut out = Vec::new();
            key.append_to(&mut out);
            assert_eq!(out, raw);
        }
    }

    #[test]
    fn all_zero_keys_use_sparse_encoding() {
        for &len in &[1usize, 7, 8, 9, 127, 128, 254, 255] {
            let raw = vec![0u8; len];
            let key = FingerprintKey::new(&raw);
            assert_eq!(key.len(), len);
            let expected = if len == 1 { 2 } else { 1 + len.div_ceil(8) };
            assert_eq!(key.encoded_len(), expected, "len {len}");
            assert!(key.matches(&raw), "len {len}");
            assert_eq!(key.hash(), blake3::hash(&raw), "len {len}");
            let mut out = Vec::new();
            key.append_to(&mut out);
            assert_eq!(out, raw, "len {len}");
        }
    }

    #[test]
    fn one_hot_keys_roundtrip_at_every_position() {
        for len in 1..=255usize {
            for position in 0..len {
                let raw: Vec<u8> = (0..len)
                    .map(|index| if index == position { 0xA5 } else { 0 })
                    .collect();
                let key = FingerprintKey::new(&raw);
                assert_eq!(key.len(), len);
                let expected = if len <= 2 {
                    len + 1
                } else {
                    1 + len.div_ceil(8) + 1
                };
                assert_eq!(key.encoded_len(), expected, "len {len} position {position}");
                assert!(key.matches(&raw), "len {len} position {position}");
                assert_eq!(
                    key.hash(),
                    blake3::hash(&raw),
                    "len {len} position {position}"
                );
                let mut out = Vec::new();
                key.append_to(&mut out);
                assert_eq!(out, raw, "len {len} position {position}");
            }
        }
    }

    #[test]
    fn all_nonzero_keys_use_raw_fallback() {
        for &len in &[1usize, 2, 7, 8, 9, 93, 127, 128, 254, 255] {
            let raw: Vec<u8> = (0..len).map(|index| (index % 255 + 1) as u8).collect();
            let key = FingerprintKey::new(&raw);
            assert_eq!(key.len(), len);
            assert_eq!(key.encoded_len(), len + 1, "len {len}");
            assert!(key.matches(&raw), "len {len}");
            assert_eq!(key.hash(), blake3::hash(&raw), "len {len}");
            let mut out = Vec::new();
            key.append_to(&mut out);
            assert_eq!(out, raw, "len {len}");
        }
    }

    #[test]
    fn long_keys_use_raw_fallback() {
        for &len in &[256usize, 1024] {
            let raw: Vec<u8> = (0..len).map(|index| (index % 251) as u8).collect();
            let key = FingerprintKey::new(&raw);
            assert_eq!(key.len(), len);
            assert_eq!(key.encoded_len(), len + 1, "len {len}");
            assert!(key.matches(&raw), "len {len}");
            assert_eq!(key.hash(), blake3::hash(&raw), "len {len}");
            let mut out = Vec::new();
            key.append_to(&mut out);
            assert_eq!(out, raw, "len {len}");
        }
    }

    #[test]
    fn compact_native_style_key_fits_the_64_byte_class() {
        let raw = native_style_key();
        let key = FingerprintKey::new(&raw);
        assert_eq!(key.len(), NATIVE_KEY_LEN);
        assert_eq!(
            key.encoded_len(),
            1 + NATIVE_KEY_LEN.div_ceil(8) + NATIVE_NONZERO
        );
        assert!(key.encoded_len() <= 64);
        assert!(key.matches(&raw));
        assert_eq!(key.hash(), blake3::hash(&raw));
        let mut out = Vec::new();
        key.append_to(&mut out);
        assert_eq!(out, raw);
    }

    #[test]
    fn encoded_len_and_ptr_expose_storage_for_profiling() {
        let raw = native_style_key();
        let key = FingerprintKey::new(&raw);
        assert_eq!(key.encoded_len(), 53);
        assert!(!key.as_ptr().is_null());
        let empty = FingerprintKey::new(&[]);
        assert_eq!(empty.encoded_len(), 0);
    }

    #[test]
    fn equality_clone_and_canonical_storage() {
        let raw = native_style_key();
        let key = FingerprintKey::new(&raw);
        assert_eq!(key, key.clone());
        assert_eq!(FingerprintKey::new(&raw), FingerprintKey::new(&raw));
        let mut changed = raw.clone();
        changed[0] ^= 1;
        assert_ne!(key, FingerprintKey::new(&changed));
        let zeros = vec![0u8; 20];
        assert_eq!(FingerprintKey::new(&zeros), FingerprintKey::new(&zeros));
        assert_ne!(FingerprintKey::new(&zeros), FingerprintKey::new(&[1u8; 20]));
    }

    #[test]
    fn different_raw_keys_never_compare_equal() {
        for len in 1..=64usize {
            let a = vec![0u8; len];
            let mut b = vec![0u8; len];
            b[len - 1] = 1;
            assert_ne!(
                FingerprintKey::new(&a),
                FingerprintKey::new(&b),
                "len {len}"
            );
        }
    }

    #[test]
    fn matches_rejects_same_length_changes() {
        let raw = native_style_key();
        let key = FingerprintKey::new(&raw);
        let nonzero_index = raw.iter().position(|&byte| byte != 0).unwrap();
        let zero_index = raw.iter().position(|&byte| byte == 0).unwrap();

        let mut changed_nonzero = raw.clone();
        changed_nonzero[nonzero_index] = changed_nonzero[nonzero_index].wrapping_add(1).max(1);
        assert_ne!(changed_nonzero[nonzero_index], raw[nonzero_index]);
        assert!(!key.matches(&changed_nonzero));

        let mut added_nonzero = raw.clone();
        added_nonzero[zero_index] = 1;
        assert!(!key.matches(&added_nonzero));

        let mut removed_nonzero = raw.clone();
        removed_nonzero[nonzero_index] = 0;
        assert!(!key.matches(&removed_nonzero));

        assert!(!key.matches(&raw[..raw.len() - 1]));
        let mut longer = raw.clone();
        longer.push(0);
        assert!(!key.matches(&longer));
        assert!(key.matches(&raw));
    }

    #[test]
    fn append_to_preserves_existing_prefix() {
        let cases = vec![
            Vec::new(),
            vec![0u8; 20],
            native_style_key(),
            vec![7u8; 300],
        ];
        for raw in cases {
            let key = FingerprintKey::new(&raw);
            let mut out = b"prefix-".to_vec();
            key.append_to(&mut out);
            let mut expected = b"prefix-".to_vec();
            expected.extend_from_slice(&raw);
            assert_eq!(out, expected);
        }
    }

    #[test]
    fn hash_matches_blake3_of_raw() {
        let cases = vec![
            Vec::new(),
            native_style_key(),
            sftp_style_key(),
            repeated_blocks(),
        ];
        for raw in cases {
            let key = FingerprintKey::new(&raw);
            assert_eq!(key.hash(), blake3::hash(&raw), "len {}", raw.len());
        }
    }

    #[test]
    fn debug_prints_original_bytes() {
        let cases = vec![
            Vec::new(),
            native_style_key(),
            vec![0u8; 20],
            vec![1u8; 300],
        ];
        for raw in cases {
            let key = FingerprintKey::new(&raw);
            assert_eq!(
                format!("{:?}", key),
                format!("{:?}", raw),
                "len {}",
                raw.len()
            );
        }
    }

    #[test]
    fn roundtrips_every_byte_value() {
        for value in 0..=255u8 {
            let mut raw = vec![0u8; 255];
            raw[0] = value;
            raw[254] = value.wrapping_add(1);
            let key = FingerprintKey::new(&raw);
            assert_eq!(key.len(), raw.len());
            assert!(key.matches(&raw), "value {value}");
            assert_eq!(key.hash(), blake3::hash(&raw), "value {value}");
            let mut out = Vec::new();
            key.append_to(&mut out);
            assert_eq!(out, raw, "value {value}");
        }
    }

    #[test]
    fn roundtrips_deterministic_random_keys() {
        let mut state = 0x1234_5678_9abc_def0u64;
        for len in 0..=1024usize {
            let mut raw = vec![0u8; len];
            for byte in &mut raw {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *byte = state as u8;
            }
            let key = FingerprintKey::new(&raw);
            assert_eq!(key.len(), len);
            assert!(key.matches(&raw), "len {len}");
            assert_eq!(key.hash(), blake3::hash(&raw), "len {len}");
            let mut out = Vec::new();
            key.append_to(&mut out);
            assert_eq!(out, raw, "len {len}");
            assert!(key.encoded_len() <= len + 1, "len {len}");
        }
    }
}
