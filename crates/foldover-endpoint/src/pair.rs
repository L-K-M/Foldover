use foldover_types::{PairRoot, Side};

use crate::{Endpoint, Reach, RootRelation, ScanError};

#[derive(Debug, thiserror::Error)]
pub enum PairRootError {
    #[error(transparent)]
    Endpoint(#[from] ScanError),
    #[error("Selected local roots overlap: {0:?}")]
    Overlap(RootRelation),
    /// Nothing is at that side's path, e.g. its disk is not mounted.
    #[error("The {0:?} folder is missing; connect its disk or share")]
    Missing(Side),
    /// The side's SFTP server or paired Mac could not be reached.
    #[error("The {0:?} folder's server is unreachable: {1}")]
    Unreachable(Side, String),
    #[error("{}", access_denied(*.0))]
    AccessDenied(Side),
}

/// What to do when macOS or file permissions refuse a folder.
pub fn access_denied(side: Side) -> String {
    format!(
        "Foldover may not read the {side:?} folder. If it is in Desktop, Documents or Downloads, \
         in iCloud Drive, or on an external or network volume, allow Foldover in System Settings \
         › Privacy & Security › Full Disk Access, then try again."
    )
}

/// Two held roots checked for overlap at selection time. Volume identity,
/// readiness and execution-time revalidation still precede any sync writes.
pub struct EndpointPair {
    left: Endpoint,
    right: Endpoint,
}

impl EndpointPair {
    pub fn open(left: &PairRoot, right: &PairRoot, reach: &Reach) -> Result<Self, PairRootError> {
        let open = |root: &PairRoot, side| {
            Endpoint::open(root, reach).map_err(|error| match error {
                ScanError::RootMissing => PairRootError::Missing(side),
                ScanError::Unreachable(message) | ScanError::MacUnreachable(message) => {
                    PairRootError::Unreachable(side, message)
                }
                error if error.is_access_denied() => PairRootError::AccessDenied(side),
                error => PairRootError::Endpoint(error),
            })
        };
        let left = open(left, Side::Left)?;
        let right = open(right, Side::Right)?;
        let relation = left.relation_to(&right)?;
        if relation != RootRelation::Disjoint {
            return Err(PairRootError::Overlap(relation));
        }
        Ok(Self { left, right })
    }

    /// Endpoints opened elsewhere, e.g. over an existing SFTP session.
    /// Overlap is not checked.
    pub fn new(left: Endpoint, right: Endpoint) -> Self {
        Self { left, right }
    }

    pub fn endpoint(&self, side: Side) -> &Endpoint {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    use std::path::Path;

    fn root(path: &Path) -> PairRoot {
        PairRoot::from_bytes(path.as_os_str().as_bytes().to_vec()).unwrap()
    }

    #[test]
    fn pair_selection_rejects_equal_nested_and_alias_roots() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("parent");
        let child = parent.join("child");
        let alias = temporary.path().join("alias");
        fs::create_dir_all(&child).unwrap();
        symlink(&parent, &alias).unwrap();
        let aliased_child = alias.join("child");
        for (left, right, relation) in [
            (&parent, &parent, RootRelation::Same),
            (&parent, &alias, RootRelation::Same),
            (&parent, &child, RootRelation::Contains),
            (&parent, &aliased_child, RootRelation::Contains),
            (&child, &alias, RootRelation::Within),
        ] {
            assert!(matches!(
                EndpointPair::open(&root(left), &root(right), &Reach::default()),
                Err(PairRootError::Overlap(found)) if found == relation,
            ));
        }
    }

    #[test]
    fn selected_disjoint_roots_remain_held_and_readable() {
        let temporary = tempfile::tempdir().unwrap();
        let left = temporary.path().join("one");
        let right = temporary.path().join("one-more");
        fs::create_dir(&left).unwrap();
        fs::create_dir(&right).unwrap();
        fs::write(left.join("file"), b"content").unwrap();
        let pair = EndpointPair::open(&root(&left), &root(&right), &Reach::default()).unwrap();
        fs::rename(&left, temporary.path().join("moved")).unwrap();
        fs::create_dir(&left).unwrap();
        let (Endpoint::Local(left), Endpoint::Local(right)) =
            (pair.endpoint(Side::Left), pair.endpoint(Side::Right))
        else {
            panic!("local roots open as local endpoints");
        };
        let mut paths = Vec::new();
        let stats = left
            .scan_metadata(&mut |entry| {
                paths.push(entry.path().clone());
                crate::ScanControl::Continue
            })
            .unwrap();
        assert_eq!(stats.entries(), 1);
        assert_eq!(paths[0].as_bytes(), b"file");
        assert_eq!(
            right
                .scan_metadata(&mut |_| panic!("empty target"))
                .unwrap()
                .entries(),
            0
        );
    }

    #[test]
    fn an_unavailable_selected_root_names_its_side() {
        let temporary = tempfile::tempdir().unwrap();
        let present = temporary.path().join("present");
        fs::create_dir(&present).unwrap();
        let missing = temporary.path().join("unmounted");

        assert!(matches!(
            EndpointPair::open(&root(&present), &root(&missing), &Reach::default()),
            Err(PairRootError::Missing(Side::Right))
        ));
        assert!(matches!(
            EndpointPair::open(&root(&missing), &root(&present), &Reach::default()),
            Err(PairRootError::Missing(Side::Left))
        ));
    }

    #[test]
    fn an_unreadable_folder_says_how_to_grant_access() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().unwrap();
        let readable = temporary.path().join("readable");
        let locked = temporary.path().join("locked");
        fs::create_dir(&readable).unwrap();
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let opened = EndpointPair::open(&root(&readable), &root(&locked), &Reach::default());
        // Superusers read anything; the check needs an ordinary account.
        let enforced = fs::read_dir(&locked).is_err();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        if !enforced {
            return;
        }

        let Err(error) = opened else {
            panic!("an unreadable folder opened");
        };
        assert!(
            matches!(error, PairRootError::AccessDenied(Side::Right)),
            "{error:?}"
        );
        assert!(error.to_string().contains("Full Disk Access"));
    }
}
