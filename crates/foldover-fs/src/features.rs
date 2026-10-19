/// A filesystem declaration, not proof that a driver preserves the feature.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DeclaredSupport {
    Supported,
    Unsupported,
    #[default]
    NotReported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemFeature {
    CaseSensitiveNames,
    CasePreservedNames,
    SymbolicLinks,
    HardLinks,
    SparseFiles,
    /// Persistent object IDs do not establish that stat inode IDs never recycle.
    PersistentObjectIds,
    NativeXattrs,
    AccessControlLists,
    NamedStreams,
    FileCloning,
    ExclusiveRename,
    SwapRename,
}

pub(super) const FEATURE_COUNT: usize = FilesystemFeature::SwapRename as usize + 1;

/// Live filesystem declarations. Naming, operation fidelity and mount/readiness
/// checks still need their own evidence before an executable plan can use them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilesystemFeatures {
    support: [DeclaredSupport; FEATURE_COUNT],
}

impl Default for FilesystemFeatures {
    fn default() -> Self {
        Self {
            support: [DeclaredSupport::NotReported; FEATURE_COUNT],
        }
    }
}

impl FilesystemFeatures {
    pub fn support(&self, feature: FilesystemFeature) -> DeclaredSupport {
        self.support[feature as usize]
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn from_reports(
        reports: impl IntoIterator<Item = (FilesystemFeature, DeclaredSupport)>,
    ) -> Self {
        let mut result = Self::default();
        for (feature, support) in reports {
            result.support[feature as usize] = support;
        }
        result
    }
}
