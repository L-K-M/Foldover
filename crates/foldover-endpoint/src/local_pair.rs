use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use foldover_types::{LocalRoot, Side};

use crate::{LocalEndpoint, RootRelation, ScanError};

#[derive(Debug, thiserror::Error)]
pub enum PairRootError {
    #[error(transparent)]
    Endpoint(#[from] ScanError),
    #[error("Selected local roots overlap: {0:?}")]
    Overlap(RootRelation),
    /// Nothing is at that side's path, e.g. its disk is not mounted.
    #[error("The {0:?} folder is missing; connect its disk or share")]
    Missing(Side),
}

/// Two held roots checked for overlap at selection time. Volume identity,
/// readiness and execution-time revalidation still precede any sync writes.
pub struct LocalPair {
    left: LocalEndpoint,
    right: LocalEndpoint,
}

impl LocalPair {
    pub fn open(left: &LocalRoot, right: &LocalRoot) -> Result<Self, PairRootError> {
        let open = |root: &LocalRoot, side| {
            LocalEndpoint::open(Path::new(OsStr::from_bytes(root.as_bytes()))).map_err(|error| {
                match error {
                    ScanError::RootMissing => PairRootError::Missing(side),
                    error => PairRootError::Endpoint(error),
                }
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

    pub fn endpoint(&self, side: Side) -> &LocalEndpoint {
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
    use std::os::unix::fs::symlink;

    fn root(path: &Path) -> LocalRoot {
        LocalRoot::from_bytes(path.as_os_str().as_bytes().to_vec()).unwrap()
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
                LocalPair::open(&root(left), &root(right)),
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
        let pair = LocalPair::open(&root(&left), &root(&right)).unwrap();
        fs::rename(&left, temporary.path().join("moved")).unwrap();
        fs::create_dir(&left).unwrap();
        let mut paths = Vec::new();
        let stats = pair
            .endpoint(Side::Left)
            .scan_metadata(&mut |entry| {
                paths.push(entry.path().clone());
                crate::ScanControl::Continue
            })
            .unwrap();
        assert_eq!(stats.entries(), 1);
        assert_eq!(paths[0].as_bytes(), b"file");
        assert_eq!(
            pair.endpoint(Side::Right)
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
            LocalPair::open(&root(&present), &root(&missing)),
            Err(PairRootError::Missing(Side::Right))
        ));
        assert!(matches!(
            LocalPair::open(&root(&missing), &root(&present)),
            Err(PairRootError::Missing(Side::Left))
        ));
    }
}
