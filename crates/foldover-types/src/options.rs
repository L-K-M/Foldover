use crate::MetadataPolicy;

/// How far each write is flushed before a sync step counts as done.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Durability {
    /// `fsync`: survives app, agent and system crashes.
    #[default]
    Standard,
    /// Also flushes the drive's cache (`F_FULLFSYNC` on macOS), so writes
    /// survive power loss. Each file costs roughly 10–30 ms more.
    Full,
}

/// Per-pair settings beyond roots and mode.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct PairOptions {
    metadata: MetadataPolicy,
    durability: Durability,
}

impl PairOptions {
    pub const fn new(metadata: MetadataPolicy, durability: Durability) -> Self {
        Self {
            metadata,
            durability,
        }
    }

    pub const fn metadata(self) -> MetadataPolicy {
        self.metadata
    }

    pub const fn durability(self) -> Durability {
        self.durability
    }
}
