/// Whether a pair keeps one metadata category in sync. An ignored category
/// never causes an update or conflict; new copies get the target's defaults.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Propagation {
    Sync,
    Ignore,
}

/// Metadata a pair syncs beyond content. Resource forks are content and
/// always sync. Ownership, lock flags, creation dates and attributes macOS
/// maintains itself never do.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MetadataPolicy {
    modification_times: Propagation,
    extended_attributes: Propagation,
    permissions: Propagation,
}

impl MetadataPolicy {
    pub const fn new(
        modification_times: Propagation,
        extended_attributes: Propagation,
        permissions: Propagation,
    ) -> Self {
        Self {
            modification_times,
            extended_attributes,
            permissions,
        }
    }

    /// File modification times. Folder and link times never sync.
    pub const fn modification_times(self) -> Propagation {
        self.modification_times
    }

    /// Tags, Finder info, comments, quarantine and other xattrs, plus the
    /// hidden and no-dump flags.
    pub const fn extended_attributes(self) -> Propagation {
        self.extended_attributes
    }

    /// Mode bits and ACLs. When ignored, a file's executable bit still syncs.
    pub const fn permissions(self) -> Propagation {
        self.permissions
    }
}

impl Default for MetadataPolicy {
    fn default() -> Self {
        Self::new(Propagation::Sync, Propagation::Sync, Propagation::Ignore)
    }
}
