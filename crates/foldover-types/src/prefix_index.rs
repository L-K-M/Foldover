//! Component-boundary prefix membership over raw path bytes.
//!
//! Several hot paths ask whether a path is one of, or below, a growing set
//! of roots: an SFTP observation's unreadable subtrees, a run's failed
//! writes whose descendants are skipped, and branches a person skipped.
//! Comparing every path with every root is quadratic when errors are many.
//! A `BTreeSet` of root bytes answers a query from the path's own component
//! prefixes instead, in `O(depth · log roots)`.
//!
//! Keys are exact raw name bytes, so non-UTF8 names compare losslessly. A
//! root matches only at `/` component boundaries: `a` covers `a` and
//! `a/x`, never `a-b`, `a.txt` or `a0`.

use std::collections::BTreeSet;

use crate::SyncPath;

/// Roots that cover themselves and, depending on the query, their
/// descendants.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PathPrefixIndex {
    roots: BTreeSet<Vec<u8>>,
}

impl PathPrefixIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_paths(paths: &[SyncPath]) -> Self {
        Self {
            roots: paths.iter().map(|path| path.as_bytes().to_vec()).collect(),
        }
    }

    /// Add a root. Adding one already present changes nothing.
    pub fn insert(&mut self, path: &SyncPath) {
        self.roots.insert(path.as_bytes().to_vec());
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    pub fn len(&self) -> usize {
        self.roots.len()
    }

    /// Whether `path` is a root or lies below one. This is the rule an
    /// unavailable subtree suppresses its own path and its descendants.
    pub fn contains(&self, path: &SyncPath) -> bool {
        self.roots.contains(path.as_bytes()) || self.contains_ancestor(path)
    }

    /// Whether a path above `path` is a root. `path` itself does not
    /// count, so a failed write does not mark its own row skipped.
    pub fn contains_ancestor(&self, path: &SyncPath) -> bool {
        if self.roots.is_empty() {
            return false;
        }
        self.probes(path).0
    }

    /// `contains_ancestor`, also reporting how many component prefixes
    /// were probed. Tests use this to hold the index to its depth-bound
    /// work instead of a wall-clock budget.
    fn probes(&self, path: &SyncPath) -> (bool, usize) {
        let bytes = path.as_bytes();
        let mut probes = 0;
        for (index, byte) in bytes.iter().enumerate() {
            if *byte == b'/' {
                probes += 1;
                if self.roots.contains(&bytes[..index]) {
                    return (true, probes);
                }
            }
        }
        (false, probes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn path(bytes: &[u8]) -> SyncPath {
        SyncPath::try_from(bytes).unwrap()
    }

    /// A root only covers itself and descendants past a `/` boundary;
    /// names that merely share leading bytes stay independent.
    #[test]
    fn membership_respects_component_boundaries() {
        let index =
            PathPrefixIndex::from_paths(&[path(b"a"), path(b"a/nested"), path(b"\xff/tree")]);
        for (bytes, expected) in [
            (b"a".as_slice(), true),
            (b"a/child", true),
            (b"a/nested/child", true),
            (b"a!".as_slice(), false),
            (b"a!/child".as_slice(), false),
            (b"a-else/child".as_slice(), false),
            (b"a.txt".as_slice(), false),
            (b"a.txt/child".as_slice(), false),
            (b"a0".as_slice(), false),
            (b"a0/child".as_slice(), false),
            (b"ab/child".as_slice(), false),
            (b"\xff/tree".as_slice(), true),
            (b"\xff/tree/\xfe".as_slice(), true),
            (b"\xff/trees/\xfe".as_slice(), false),
            (b"\xfe/tree".as_slice(), false),
        ] {
            assert_eq!(index.contains(&path(bytes)), expected, "{bytes:?}");
        }
        assert!(!PathPrefixIndex::default().contains(&path(b"a/child")));
    }

    /// The ancestor-only query excludes the path itself, so a failure
    /// suppresses what is below it, not its own row.
    #[test]
    fn ancestor_only_query_excludes_the_path_itself() {
        let index = PathPrefixIndex::from_paths(&[path(b"a")]);
        for (bytes, ancestor) in [
            (b"a".as_slice(), false),
            (b"a/b".as_slice(), true),
            (b"a/b/c".as_slice(), true),
            (b"a-b".as_slice(), false),
            (b"a.txt".as_slice(), false),
            (b"a0".as_slice(), false),
        ] {
            assert_eq!(index.contains_ancestor(&path(bytes)), ancestor, "{bytes:?}");
        }
        assert!(!PathPrefixIndex::default().contains_ancestor(&path(b"a/b")));
    }

    /// Roots arrive as failures do; a later insert covers paths that
    /// were queried before it, and never widens to a shared byte prefix.
    #[test]
    fn dynamically_arriving_roots_widen_coverage() {
        let mut index = PathPrefixIndex::new();
        assert!(!index.contains(&path(b"a/x")));
        index.insert(&path(b"a"));
        assert!(index.contains(&path(b"a/x")));
        assert!(!index.contains(&path(b"a-b/x")));
        index.insert(&path(b"a-b"));
        assert!(index.contains(&path(b"a-b/x")));
        assert!(index.contains(&path(b"a/x")));
        assert!(!index.contains(&path(b"a.txt/x")));
    }

    /// The index probes one prefix per component above the path, not one
    /// per root: 100k paths over 1k roots stay at depth-bound work.
    #[test]
    fn membership_work_scales_with_depth_not_roots() {
        const ROOTS: usize = 1_000;
        const QUERIES: usize = 100_000;
        let roots: Vec<SyncPath> = (0..ROOTS)
            .map(|index| path(format!("root-{index:04}/branch").as_bytes()))
            .collect();
        let index = PathPrefixIndex::from_paths(&roots);
        let mut probes = 0;
        let mut hits = 0;
        for query in 0..QUERIES {
            let root = query % ROOTS;
            let path = path(format!("root-{root:04}/branch/file-{query:06}").as_bytes());
            let (hit, count) = index.probes(&path);
            hits += usize::from(hit);
            probes += count;
        }
        assert_eq!(hits, QUERIES);
        assert_eq!(probes, QUERIES * 2, "two components above each file");
        // A linear scan would compare against all 1_000 roots per query.
        assert!(probes < QUERIES * ROOTS / 100);
    }

    fn valid_path() -> impl Strategy<Value = SyncPath> {
        let component = prop::collection::vec(
            any::<u8>().prop_filter("ordinary name byte", |byte| !matches!(*byte, 0 | b'/')),
            1..12,
        )
        .prop_filter("not dot traversal", |name| name != b"." && name != b"..");
        prop::collection::vec(component, 1..6).prop_map(|parts| {
            let bytes = parts.join(&b'/');
            path(&bytes)
        })
    }

    proptest! {
        #[test]
        fn indexed_membership_matches_the_linear_rule(
            roots in prop::collection::vec(valid_path(), 0..30),
            queries in prop::collection::vec(valid_path(), 0..30),
        ) {
            let index = PathPrefixIndex::from_paths(&roots);
            for query in &queries {
                prop_assert_eq!(
                    index.contains(query),
                    roots.iter().any(|root| root == query || root.is_ancestor_of(query))
                );
                prop_assert_eq!(
                    index.contains_ancestor(query),
                    roots.iter().any(|root| root.is_ancestor_of(query))
                );
            }
        }

        #[test]
        fn dynamically_inserted_membership_matches_the_linear_rule(
            roots in prop::collection::vec(valid_path(), 0..30),
            queries in prop::collection::vec(valid_path(), 0..30),
        ) {
            let mut index = PathPrefixIndex::new();
            let mut inserted = Vec::new();
            for root in roots {
                index.insert(&root);
                inserted.push(root.clone());
                let mut descendant = root.as_bytes().to_vec();
                descendant.extend_from_slice(b"/child");
                for query in queries.iter().chain([&root, &path(&descendant)]) {
                    prop_assert_eq!(
                        index.contains(query),
                        inserted.iter().any(|known| known == query || known.is_ancestor_of(query))
                    );
                    prop_assert_eq!(
                        index.contains_ancestor(query),
                        inserted.iter().any(|known| known.is_ancestor_of(query))
                    );
                }
            }
        }
    }
}
