use std::fmt;

const SEPARATOR: u8 = b'/';

/// A nonempty root-relative path with exact on-disk name bytes.
///
/// Syntax validation does not prove filesystem containment. Drivers must use
/// root handles, reject symlink traversal and protect Foldover's internal paths.
/// NFC/case comparison keys are separate from this lossless representation.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SyncPath(Vec<u8>);

impl fmt::Debug for SyncPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Errors show a readable filename; the path itself keeps its exact bytes.
        fmt::Debug::fmt(&String::from_utf8_lossy(&self.0), formatter)
    }
}

impl SyncPath {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn components(&self) -> impl DoubleEndedIterator<Item = &[u8]> {
        self.0.split(|byte| *byte == SEPARATOR)
    }

    pub fn file_name(&self) -> &[u8] {
        self.components()
            .next_back()
            .expect("validated paths have a final component")
    }

    /// None means the entry's parent is the endpoint root, not an empty path.
    pub fn parent(&self) -> Option<Self> {
        self.0
            .iter()
            .rposition(|byte| *byte == SEPARATOR)
            .map(|separator| Self(self.0[..separator].to_vec()))
    }

    pub fn is_ancestor_of(&self, other: &Self) -> bool {
        other
            .0
            .strip_prefix(self.as_bytes())
            .is_some_and(|suffix| suffix.first() == Some(&SEPARATOR))
    }
}

impl TryFrom<Vec<u8>> for SyncPath {
    type Error = InvalidSyncPath;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        validate(&bytes)?;
        Ok(Self(bytes))
    }
}

impl TryFrom<&[u8]> for SyncPath {
    type Error = InvalidSyncPath;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        validate(bytes)?;
        Ok(Self(bytes.to_vec()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidSyncPath {
    Empty,
    Absolute,
    NullByte,
    EmptyComponent,
    CurrentDirectory,
    ParentDirectory,
}

impl fmt::Display for InvalidSyncPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "a sync path must name an entry below its root",
            Self::Absolute => "a sync path must be relative to its root",
            Self::NullByte => "a sync path cannot contain a NUL byte",
            Self::EmptyComponent => "a sync path cannot contain empty components",
            Self::CurrentDirectory => "a sync path cannot contain '.' components",
            Self::ParentDirectory => "a sync path cannot contain '..' components",
        })
    }
}

impl std::error::Error for InvalidSyncPath {}

fn validate(bytes: &[u8]) -> Result<(), InvalidSyncPath> {
    if bytes.is_empty() {
        return Err(InvalidSyncPath::Empty);
    }
    if bytes.first() == Some(&SEPARATOR) {
        return Err(InvalidSyncPath::Absolute);
    }
    if bytes.contains(&b'\0') {
        return Err(InvalidSyncPath::NullByte);
    }
    for component in bytes.split(|byte| *byte == SEPARATOR) {
        match component {
            b"" => return Err(InvalidSyncPath::EmptyComponent),
            b"." => return Err(InvalidSyncPath::CurrentDirectory),
            b".." => return Err(InvalidSyncPath::ParentDirectory),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(bytes: &[u8]) -> SyncPath {
        SyncPath::try_from(bytes).unwrap()
    }

    #[test]
    fn rejects_roots_traversal_and_ambiguous_components() {
        let invalid: &[(&[u8], InvalidSyncPath)] = &[
            (b"", InvalidSyncPath::Empty),
            (b"/", InvalidSyncPath::Absolute),
            (b"/folder/file", InvalidSyncPath::Absolute),
            (b"folder/\0file", InvalidSyncPath::NullByte),
            (b"folder/", InvalidSyncPath::EmptyComponent),
            (b"folder//file", InvalidSyncPath::EmptyComponent),
            (b".", InvalidSyncPath::CurrentDirectory),
            (b"./file", InvalidSyncPath::CurrentDirectory),
            (b"folder/.", InvalidSyncPath::CurrentDirectory),
            (b"..", InvalidSyncPath::ParentDirectory),
            (b"folder/../file", InvalidSyncPath::ParentDirectory),
        ];
        for &(bytes, expected) in invalid {
            assert_eq!(SyncPath::try_from(bytes), Err(expected), "{bytes:?}");
            assert_eq!(SyncPath::try_from(bytes.to_vec()), Err(expected));
        }
    }

    #[test]
    fn preserves_raw_names_without_lossy_decoding_or_normalization() {
        for bytes in [
            b"\xff.txt".as_slice(),
            b"colon:name",
            b"back\\slash",
            b"...",
            b"a..b",
        ] {
            assert_eq!(path(bytes).as_bytes(), bytes);
        }
        assert_ne!(path(b"\xff.txt"), path(b"\xfe.txt"));
        assert_ne!(
            path("résumé".as_bytes()),
            path("re\u{301}sume\u{301}".as_bytes())
        );
        assert_ne!(path(b"Name"), path(b"name"));
    }

    #[test]
    fn errors_show_readable_paths_instead_of_byte_arrays() {
        assert_eq!(
            format!("{:?}", path("jan/._LICENSE-linenoise".as_bytes())),
            "\"jan/._LICENSE-linenoise\""
        );
        assert_eq!(
            format!("{:?}", path("café/notes\n.txt".as_bytes())),
            "\"café/notes\\n.txt\""
        );
    }

    #[test]
    fn parent_and_component_operations_preserve_validated_bytes() {
        let leaf = path(b"one/\xff/two");
        assert_eq!(leaf.file_name(), b"two");
        assert_eq!(
            leaf.components().collect::<Vec<_>>(),
            [b"one".as_slice(), b"\xff", b"two"]
        );
        let parent = leaf.parent().unwrap();
        assert_eq!(parent, path(b"one/\xff"));
        assert_eq!(parent.parent(), Some(path(b"one")));
        assert_eq!(path(b"one").parent(), None);
        assert!(parent < leaf);
    }

    #[test]
    fn ancestry_requires_a_component_boundary() {
        let parent = path(b"one");
        assert!(parent.is_ancestor_of(&path(b"one/two")));
        assert!(parent.is_ancestor_of(&path(b"one/two/three")));
        assert!(!parent.is_ancestor_of(&parent));
        assert!(!parent.is_ancestor_of(&path(b"ones/two")));
        assert!(!path(b"one/two").is_ancestor_of(&path(b"one/twos")));
    }

    #[test]
    fn all_other_single_byte_names_remain_lossless() {
        for byte in u8::MIN..=u8::MAX {
            let bytes = [byte];
            if matches!(byte, b'\0' | SEPARATOR | b'.') {
                assert!(SyncPath::try_from(bytes.as_slice()).is_err());
                continue;
            }
            assert_eq!(path(&bytes).as_bytes(), bytes);
        }
    }
}
