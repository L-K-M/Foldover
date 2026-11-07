use std::fmt;

/// A BLAKE3 digest of content or canonical metadata, not an mtime heuristic.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Digest([u8; Digest::BYTE_LEN]);

impl Digest {
    pub const BYTE_LEN: usize = 32;

    pub const fn from_bytes(bytes: [u8; Self::BYTE_LEN]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; Self::BYTE_LEN] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub const fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncMode {
    Sync,
    Mirror { source: Side },
    Backup { source: Side },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
}

/// A symlink digest hashes its target bytes; the target is never followed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EntryContent {
    File(Digest),
    Directory,
    Symlink(Digest),
}

/// Observed content and metadata, retaining this endpoint's stored fidelity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EntryVersion {
    content: EntryContent,
    metadata: Digest,
}

impl EntryVersion {
    pub const fn new(content: EntryContent, metadata: Digest) -> Self {
        Self { content, metadata }
    }

    pub const fn content(self) -> EntryContent {
        self.content
    }

    pub const fn metadata(self) -> Digest {
        self.metadata
    }

    pub const fn kind(self) -> EntryKind {
        match self.content {
            EntryContent::File(_) => EntryKind::File,
            EntryContent::Directory => EntryKind::Directory,
            EntryContent::Symlink(_) => EntryKind::Symlink,
        }
    }
}

/// Per-side versions prevent accepted metadata loss from causing copy loops.
/// A baseline requires equal content on both sides, so store that content once
/// and keep each side's metadata digest apart.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct BaselineEntry {
    content: EntryContent,
    left_metadata: Digest,
    right_metadata: Digest,
}

impl BaselineEntry {
    pub fn new(left: EntryVersion, right: EntryVersion) -> Result<Self, BaselineMismatch> {
        if left.content() != right.content() {
            return Err(BaselineMismatch);
        }
        Ok(Self {
            content: left.content(),
            left_metadata: left.metadata(),
            right_metadata: right.metadata(),
        })
    }

    pub const fn left(self) -> EntryVersion {
        EntryVersion::new(self.content, self.left_metadata)
    }

    pub const fn right(self) -> EntryVersion {
        EntryVersion::new(self.content, self.right_metadata)
    }
}

impl fmt::Debug for BaselineEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BaselineEntry")
            .field("left", &self.left())
            .field("right", &self.right())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BaselineMismatch;

impl fmt::Display for BaselineMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a synced baseline needs matching entry kinds and content")
    }
}

impl std::error::Error for BaselineMismatch {}

/// A person's choice of side for one previewed change or conflict. It
/// applies only while both sides still hold exactly the versions that
/// person saw.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Resolution {
    keep: Side,
    left: Option<EntryVersion>,
    right: Option<EntryVersion>,
}

impl Resolution {
    pub const fn new(keep: Side, left: Option<EntryVersion>, right: Option<EntryVersion>) -> Self {
        Self { keep, left, right }
    }

    pub const fn keep(self) -> Side {
        self.keep
    }

    pub const fn left(self) -> Option<EntryVersion> {
        self.left
    }

    pub const fn right(self) -> Option<EntryVersion> {
        self.right
    }
}

/// Leave one previewed change, and everything inside it, alone in the next
/// run, while both sides still hold exactly the versions a person saw.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Skip {
    left: Option<EntryVersion>,
    right: Option<EntryVersion>,
}

impl Skip {
    pub const fn new(left: Option<EntryVersion>, right: Option<EntryVersion>) -> Self {
        Self { left, right }
    }

    pub const fn left(self) -> Option<EntryVersion> {
        self.left
    }

    pub const fn right(self) -> Option<EntryVersion> {
        self.right
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeKind {
    Create,
    /// The executor must archive the replaced version before changing content.
    ReplaceContent,
    UpdateMetadata,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConflictKind {
    BothChanged,
    DeletedVersusEdited,
    TypeMismatch,
    MetadataOnly,
    /// Names that differ only in case or Unicode spelling, which one of
    /// the folders would store as one file; renaming one resolves it.
    NameClash,
}

/// Classification only. Tree ordering, gates, freshness checks and journaling
/// must turn this into a verified executable plan before any filesystem write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryDecision {
    Unchanged,
    RecordBase,
    ForgetBase,
    /// Retain target-only data or edits while a Backup source is unchanged.
    /// Divergent content must not be adopted into the synced base.
    KeepTarget,
    Propagate {
        from: Side,
        change: ChangeKind,
    },
    Archive {
        on: Side,
    },
    /// Rename on `on` the file or link the plan pairs with this path
    /// to here: the other side moved it, and its content is the same, so
    /// nothing is copied or archived (PLAN.md §4.7).
    Move {
        on: Side,
    },
    /// The file or link here on `on` moves to the path the plan pairs it
    /// with; that path's row carries the move.
    MovedAway {
        on: Side,
    },
    Conflict(ConflictKind),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_layout_is_smaller_than_two_versions() {
        // One shared EntryContent plus two metadata digests: 33 + 32 + 32 = 97,
        // against 2 * 65 bytes for two EntryVersions.
        assert_eq!(size_of::<BaselineEntry>(), 97);
        assert_eq!(size_of::<EntryVersion>() * 2, 130);
        assert!(size_of::<BaselineEntry>() < size_of::<EntryVersion>() * 2);
    }

    #[test]
    fn baseline_round_trips_each_kind_with_differing_metadata() {
        let digest = Digest::from_bytes([9; Digest::BYTE_LEN]);
        for content in [
            EntryContent::File(digest),
            EntryContent::Directory,
            EntryContent::Symlink(digest),
        ] {
            let left = EntryVersion::new(content, Digest::from_bytes([2; Digest::BYTE_LEN]));
            let right = EntryVersion::new(content, Digest::from_bytes([3; Digest::BYTE_LEN]));
            let base = BaselineEntry::new(left, right).unwrap();
            assert_eq!(base.left(), left);
            assert_eq!(base.right(), right);
            assert_ne!(base.left().metadata(), base.right().metadata());
        }
    }

    #[test]
    fn baseline_debug_presents_left_and_right_versions() {
        let content = EntryContent::File(Digest::from_bytes([1; Digest::BYTE_LEN]));
        let left = EntryVersion::new(content, Digest::from_bytes([2; Digest::BYTE_LEN]));
        let right = EntryVersion::new(content, Digest::from_bytes([3; Digest::BYTE_LEN]));
        let base = BaselineEntry::new(left, right).unwrap();
        assert_eq!(
            format!("{base:?}"),
            format!("BaselineEntry {{ left: {left:?}, right: {right:?} }}")
        );
    }

    #[test]
    fn baseline_requires_equal_content_but_retains_each_sides_metadata() {
        let content = EntryContent::File(Digest::from_bytes([1; Digest::BYTE_LEN]));
        let left = EntryVersion::new(content, Digest::from_bytes([2; Digest::BYTE_LEN]));
        let right = EntryVersion::new(content, Digest::from_bytes([3; Digest::BYTE_LEN]));
        let base = BaselineEntry::new(left, right).unwrap();
        assert_eq!(base.left(), left);
        assert_eq!(base.right(), right);
        assert_ne!(base.left().metadata(), base.right().metadata());

        let changed = EntryVersion::new(
            EntryContent::File(Digest::from_bytes([4; Digest::BYTE_LEN])),
            right.metadata(),
        );
        assert_eq!(BaselineEntry::new(left, changed), Err(BaselineMismatch));
        let link = EntryVersion::new(
            EntryContent::Symlink(Digest::from_bytes([1; Digest::BYTE_LEN])),
            right.metadata(),
        );
        assert_eq!(BaselineEntry::new(left, link), Err(BaselineMismatch));
    }
}
