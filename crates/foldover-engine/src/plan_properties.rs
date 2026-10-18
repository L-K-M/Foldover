use std::collections::{BTreeMap, BTreeSet};

use foldover_types::{
    BaselineEntry, Digest, EntryContent, EntryDecision, EntryKind, EntryVersion, ScanControl, Side,
    SyncMode, SyncPath,
};
use proptest::prelude::*;

use crate::{CandidateDecision, plan_candidates};

const MAX_MUTATIONS: usize = 32;
const MAX_DEPTH: usize = 5;

type Tree = BTreeMap<SyncPath, EntryVersion>;

#[derive(Clone, Debug)]
enum Mutation {
    Put(SyncPath, EntryVersion),
    Remove(SyncPath),
}

fn path_strategy() -> impl Strategy<Value = SyncPath> {
    prop::collection::vec(prop_oneof![b'a'..=b'd', Just(0xffu8)], 1..=MAX_DEPTH).prop_map(
        |segments| {
            let bytes = segments
                .iter()
                .flat_map(|segment| [b'/', b'n', *segment])
                .skip(1)
                .collect::<Vec<_>>();
            SyncPath::try_from(bytes).unwrap()
        },
    )
}

fn version_strategy() -> impl Strategy<Value = EntryVersion> {
    (0u8..3, 0u8..4, 0u8..4).prop_map(|(kind, content, metadata)| {
        let digest = Digest::from_bytes([content; Digest::BYTE_LEN]);
        let content = match kind {
            0 => EntryContent::Directory,
            1 => EntryContent::File(digest),
            _ => EntryContent::Symlink(digest),
        };
        EntryVersion::new(content, Digest::from_bytes([metadata; Digest::BYTE_LEN]))
    })
}

fn mutations() -> impl Strategy<Value = Vec<Mutation>> {
    prop::collection::vec(
        prop_oneof![
            (path_strategy(), version_strategy())
                .prop_map(|(path, version)| Mutation::Put(path, version)),
            path_strategy().prop_map(Mutation::Remove),
        ],
        0..=MAX_MUTATIONS,
    )
}

fn mode_strategy() -> impl Strategy<Value = SyncMode> {
    prop_oneof![
        Just(SyncMode::Sync),
        Just(SyncMode::Mirror { source: Side::Left }),
        Just(SyncMode::Mirror {
            source: Side::Right
        }),
        Just(SyncMode::Backup { source: Side::Left }),
        Just(SyncMode::Backup {
            source: Side::Right
        }),
    ]
}

fn directory() -> EntryVersion {
    EntryVersion::new(
        EntryContent::Directory,
        Digest::from_bytes([0; Digest::BYTE_LEN]),
    )
}

fn remove_branch(tree: &mut Tree, path: &SyncPath) {
    tree.retain(|existing, _| existing != path && !path.is_ancestor_of(existing));
}

// Model user edits independently of the planner. Parent creation and subtree
// removal keep generated snapshots valid while shrinking to small edit histories.
fn apply(tree: &mut Tree, mutations: &[Mutation]) {
    for mutation in mutations {
        let (path, version) = match mutation {
            Mutation::Put(path, version) => (path, version),
            Mutation::Remove(path) => {
                remove_branch(tree, path);
                continue;
            }
        };
        let mut ancestors = Vec::new();
        let mut parent = path.parent();
        while let Some(path) = parent {
            parent = path.parent();
            ancestors.push(path);
        }
        for parent in ancestors.into_iter().rev() {
            if tree
                .get(&parent)
                .is_some_and(|entry| entry.kind() == EntryKind::Directory)
            {
                continue;
            }
            remove_branch(tree, &parent);
            tree.insert(parent, directory());
        }
        if version.kind() != EntryKind::Directory {
            remove_branch(tree, path);
        }
        tree.insert(path.clone(), *version);
    }
}

proptest! {
    #[test]
    fn generated_trees_preserve_plan_safety(
        original in mutations(), left_edits in mutations(), right_edits in mutations(),
        mode in mode_strategy(),
    ) {
        let mut original_tree = Tree::new();
        apply(&mut original_tree, &original);
        let base = original_tree.iter().map(|(path, entry)| {
            (path.clone(), BaselineEntry::new(*entry, *entry).unwrap())
        }).collect::<BTreeMap<_, _>>();
        let mut left = original_tree.clone();
        let mut right = original_tree.clone();
        apply(&mut left, &left_edits);
        apply(&mut right, &right_edits);
        let candidate = plan_candidates(mode, &base, &left, &right, &mut || ScanControl::Continue)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let union = base.keys().chain(left.keys()).chain(right.keys()).collect::<BTreeSet<_>>();
        let paths = candidate.rows().iter().map(|row| row.path()).collect::<Vec<_>>();
        prop_assert_eq!(paths, union.into_iter().collect::<Vec<_>>());
        let rows = candidate.rows().iter().map(|row| (row.path(), row.decision())).collect::<BTreeMap<_, _>>();
        for row in candidate.rows() {
            match row.decision() {
                CandidateDecision::Entry(EntryDecision::Archive { on }) => {
                    let target = match on { Side::Left => &left, Side::Right => &right };
                    prop_assert!(target.contains_key(row.path()));
                    prop_assert!(!matches!(mode, SyncMode::Backup { .. }), "Backup must not archive");
                    if mode == SyncMode::Sync {
                        for (path, version) in target {
                            if path != row.path() && !row.path().is_ancestor_of(path) { continue; }
                            let previous = base.get(path).map(|entry| match on { Side::Left => entry.left(), Side::Right => entry.right() });
                            prop_assert_eq!(previous, Some(*version), "archive would swallow a changed survivor at {:?}", path);
                        }
                    }
                }
                CandidateDecision::Entry(EntryDecision::Propagate { from, .. }) => {
                    let source = match from { Side::Left => &left, Side::Right => &right };
                    prop_assert!(source.contains_key(row.path()));
                    if let SyncMode::Mirror { source } | SyncMode::Backup { source } = mode {
                        prop_assert_eq!(*from, source);
                    }
                }
                CandidateDecision::BlockedByAncestor(ancestor) => {
                    prop_assert!(ancestor.is_ancestor_of(row.path()));
                    prop_assert!(matches!(rows.get(ancestor), Some(CandidateDecision::Entry(EntryDecision::Conflict(_)))));
                }
                _ => {}
            }
            if let CandidateDecision::Entry(EntryDecision::Conflict(_)) = row.decision() {
                for child in candidate.rows().iter().filter(|child| row.path().is_ancestor_of(child.path())) {
                    prop_assert_eq!(child.decision(), &CandidateDecision::BlockedByAncestor(row.path().clone()));
                }
            }
        }

        let swapped_mode = match mode {
            SyncMode::Sync => SyncMode::Sync,
            SyncMode::Mirror { source } => SyncMode::Mirror { source: source.other() },
            SyncMode::Backup { source } => SyncMode::Backup { source: source.other() },
        };
        let swapped = plan_candidates(swapped_mode, &base, &right, &left, &mut || ScanControl::Continue)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(candidate.rows().len(), swapped.rows().len());
        for (row, other) in candidate.rows().iter().zip(swapped.rows()) {
            prop_assert_eq!(row.path(), other.path());
            let expected = match row.decision() {
                CandidateDecision::Entry(EntryDecision::Propagate { from, change }) => CandidateDecision::Entry(EntryDecision::Propagate { from: from.other(), change: *change }),
                CandidateDecision::Entry(EntryDecision::Archive { on }) => CandidateDecision::Entry(EntryDecision::Archive { on: on.other() }),
                other => other.clone(),
            };
            prop_assert_eq!(other.decision(), &expected);
        }
    }
}
