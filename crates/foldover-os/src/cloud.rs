//! Folders another sync engine already manages (PLAN.md §3.6): iCloud
//! Drive, Desktop & Documents in iCloud, and the provider folders under
//! `~/Library/CloudStorage`. Two engines on one folder can undo each
//! other's changes, so pair creation warns about them.

use std::path::{Component, Path};

/// A sync engine that also manages a folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CloudProvider {
    /// Anything under `~/Library/Mobile Documents`.
    ICloudDrive,
    /// `~/Desktop` or `~/Documents` while iCloud syncs them.
    DesktopAndDocuments,
    /// A `~/Library/CloudStorage/<Name>-<account>` provider.
    StorageProvider(String),
}

impl CloudProvider {
    /// The provider's name as a person knows it, e.g. "Dropbox".
    pub fn name(&self) -> &str {
        match self {
            Self::ICloudDrive => "iCloud Drive",
            Self::DesktopAndDocuments => "Desktop & Documents in iCloud",
            Self::StorageProvider(name) => name,
        }
    }
}

/// The sync engine managing `root`, judged against `home`, the agent
/// user's home directory; separate so tests can inject a home.
pub fn cloud_provider(home: &Path, root: &Path) -> Option<CloudProvider> {
    let library = home.join("Library");
    if root.starts_with(library.join("Mobile Documents")) {
        return Some(CloudProvider::ICloudDrive);
    }
    if let Ok(below) = root.strip_prefix(library.join("CloudStorage")) {
        // The folder names its provider and account: "Dropbox-alice".
        if let Some(Component::Normal(domain)) = below.components().next() {
            let domain = domain.to_string_lossy();
            let name = domain.split('-').next().unwrap_or_default();
            if !name.is_empty() {
                return Some(CloudProvider::StorageProvider(name.to_owned()));
            }
        }
    }
    let documents = library.join("Mobile Documents/com~apple~CloudDocs");
    for folder in ["Desktop", "Documents"] {
        if root.starts_with(home.join(folder)) && documents.join(folder).exists() {
            return Some(CloudProvider::DesktopAndDocuments);
        }
    }
    None
}

/// `cloud_provider` against the signed-in user's home directory.
pub fn current_cloud_provider(root: &Path) -> Option<CloudProvider> {
    cloud_provider(&std::env::home_dir()?, root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn mobile_documents_is_icloud_drive() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("Library/Mobile Documents/Photos");
        assert_eq!(
            cloud_provider(home.path(), &root),
            Some(CloudProvider::ICloudDrive)
        );
        assert_eq!(CloudProvider::ICloudDrive.name(), "iCloud Drive");
        // So is iCloud's own Desktop & Documents folder.
        let root = home
            .path()
            .join("Library/Mobile Documents/com~apple~CloudDocs/Desktop");
        assert_eq!(
            cloud_provider(home.path(), &root),
            Some(CloudProvider::ICloudDrive)
        );
    }

    #[test]
    fn cloud_storage_names_the_provider_before_the_account() {
        let home = tempfile::tempdir().unwrap();
        for (folder, provider) in [
            ("Dropbox-alice", "Dropbox"),
            ("Dropbox", "Dropbox"),
            ("OneDrive-Personal", "OneDrive"),
            ("GoogleDrive-alice@example.com", "GoogleDrive"),
        ] {
            let root = home
                .path()
                .join("Library/CloudStorage")
                .join(folder)
                .join("Photos");
            assert_eq!(
                cloud_provider(home.path(), &root),
                Some(CloudProvider::StorageProvider(provider.to_owned())),
                "{folder}"
            );
        }
        // The storage folder itself, and a nameless provider, manage nothing.
        for root in [
            home.path().join("Library/CloudStorage"),
            home.path().join("Library/CloudStorage/-alice/Photos"),
            home.path().join("Library/CloudStorage2/Dropbox-alice"),
        ] {
            assert_eq!(cloud_provider(home.path(), &root), None, "{root:?}");
        }
    }

    #[test]
    fn desktop_and_documents_need_iclouds_own_folders() {
        let home = tempfile::tempdir().unwrap();
        let desktop = home.path().join("Desktop/Photos");
        let documents = home.path().join("Documents");
        assert_eq!(cloud_provider(home.path(), &desktop), None);
        let cloud = home
            .path()
            .join("Library/Mobile Documents/com~apple~CloudDocs");
        fs::create_dir_all(cloud.join("Desktop")).unwrap();
        assert_eq!(
            cloud_provider(home.path(), &desktop),
            Some(CloudProvider::DesktopAndDocuments)
        );
        assert_eq!(cloud_provider(home.path(), &documents), None);
        fs::create_dir_all(cloud.join("Documents")).unwrap();
        assert_eq!(
            cloud_provider(home.path(), &documents),
            Some(CloudProvider::DesktopAndDocuments)
        );
        assert_eq!(
            CloudProvider::DesktopAndDocuments.name(),
            "Desktop & Documents in iCloud"
        );
    }

    #[test]
    fn ordinary_folders_are_unmanaged() {
        let home = tempfile::tempdir().unwrap();
        for root in [
            home.path().join("Photos"),
            home.path().join("Library/Mobile Documents2/Photos"),
            home.path().to_path_buf(),
        ] {
            assert_eq!(cloud_provider(home.path(), &root), None, "{root:?}");
        }
    }
}
