use std::collections::BTreeMap;

use foldover_types::{
    BaselineEntry, ConflictKind, EntryDecision, EntryKind, EntryVersion, ScanControl, Side,
    SyncMode, SyncPath,
};

use crate::{DeletionCounts, DeletionReview, InvalidDeletionCounts, RunContext, reconcile_entry};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TreeRole {
    Base,
    Left,
    Right,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum PlanError {
    #[error("{tree:?} tree is missing parent {parent:?} of {path:?}")]
    MissingParent {
        tree: TreeRole,
        path: SyncPath,
        parent: SyncPath,
    },
    #[error("{tree:?} tree has non-directory parent {parent:?} of {path:?}")]
    NonDirectoryParent {
        tree: TreeRole,
        path: SyncPath,
        parent: SyncPath,
    },
    #[error("Planning cancelled; no complete candidate was returned")]
    Cancelled,
    #[error("Plan path count exceeds the supported range")]
    CountOverflow,
    #[error(transparent)]
    InvalidDeletionCounts(#[from] InvalidDeletionCounts),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CandidateDecision {
    Entry(EntryDecision),
    /// The ancestor's conflict must be resolved before any work below it.
    BlockedByAncestor(SyncPath),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanRow {
    path: SyncPath,
    decision: CandidateDecision,
}

impl PlanRow {
    pub fn path(&self) -> &SyncPath {
        &self.path
    }

    pub fn decision(&self) -> &CandidateDecision {
        &self.decision
    }
}

/// Deterministic classifications, not executable operations. Inputs must come
/// from complete observations; this type proves neither freshness nor fidelity.
#[derive(Debug)]
pub struct CandidatePlan {
    rows: Vec<PlanRow>,
    left_deletions: DeletionCounts,
    right_deletions: DeletionCounts,
}

impl CandidatePlan {
    pub fn rows(&self) -> &[PlanRow] {
        &self.rows
    }

    pub fn deletion_review(&self, target: Side, context: RunContext) -> DeletionReview {
        match target {
            Side::Left => self.left_deletions,
            Side::Right => self.right_deletions,
        }
        .review(context)
    }
}

/// Merge complete logical trees in exact byte-path order. Incomplete scans,
/// excluded/unavailable entries and read errors must not be supplied as absence.
/// Confirmation binding, naming capabilities, operation ordering, readiness and
/// journaling still separate these candidates from an executable plan.
pub fn plan_candidates(
    mode: SyncMode,
    base: &BTreeMap<SyncPath, BaselineEntry>,
    left: &BTreeMap<SyncPath, EntryVersion>,
    right: &BTreeMap<SyncPath, EntryVersion>,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<CandidatePlan, PlanError> {
    validate_tree(base, TreeRole::Base, |entry| entry.left().kind(), control)?;
    validate_tree(left, TreeRole::Left, |entry| entry.kind(), control)?;
    validate_tree(right, TreeRole::Right, |entry| entry.kind(), control)?;

    let mut base_entries = base.iter().peekable();
    let mut left_entries = left.iter().peekable();
    let mut right_entries = right.iter().peekable();
    let mut rows: Vec<PlanRow> = Vec::new();
    let mut directories = BTreeMap::new();
    loop {
        check_control(control)?;
        let Some(path) = [
            base_entries.peek().map(|(path, _)| *path),
            left_entries.peek().map(|(path, _)| *path),
            right_entries.peek().map(|(path, _)| *path),
        ]
        .into_iter()
        .flatten()
        .min()
        .cloned() else {
            break;
        };
        let baseline = base_entries
            .next_if(|(key, _)| *key == &path)
            .map(|(_, entry)| *entry);
        let left = left_entries
            .next_if(|(key, _)| *key == &path)
            .map(|(_, entry)| *entry);
        let right = right_entries
            .next_if(|(key, _)| *key == &path)
            .map(|(_, entry)| *entry);
        if [left, right, baseline.map(BaselineEntry::left)]
            .into_iter()
            .flatten()
            .any(|entry| entry.kind() == EntryKind::Directory)
        {
            directories.insert(path.clone(), rows.len());
        }
        rows.push(PlanRow {
            path: path.clone(),
            decision: CandidateDecision::Entry(reconcile_entry(mode, baseline, left, right)),
        });

        if mode != SyncMode::Sync {
            continue;
        }
        // A deleted directory must not swallow a new/edited surviving child,
        // even when the directory's own metadata still matches its baseline.
        for (side, current, previous) in [
            (Side::Left, left, baseline.map(BaselineEntry::left)),
            (Side::Right, right, baseline.map(BaselineEntry::right)),
        ] {
            if current.is_none() || current == previous {
                continue;
            }
            let mut parent = path.parent();
            while let Some(path) = parent {
                check_control(control)?;
                if let Some(&index) = directories.get(&path)
                    && rows[index].decision
                        == CandidateDecision::Entry(EntryDecision::Archive { on: side })
                {
                    rows[index].decision = CandidateDecision::Entry(EntryDecision::Conflict(
                        ConflictKind::DeletedVersusEdited,
                    ));
                }
                parent = path.parent();
            }
        }
    }

    // Ancestors precede descendants. Suppression therefore propagates an outer
    // conflict through the whole branch without recursive traversal.
    for index in 0..rows.len() {
        check_control(control)?;
        let mut parent = rows[index].path.parent();
        let mut blocked = None;
        while let Some(path) = parent {
            check_control(control)?;
            if let Some(&ancestor) = directories.get(&path) {
                match &rows[ancestor].decision {
                    CandidateDecision::Entry(EntryDecision::Conflict(_)) => {
                        blocked = Some(path.clone())
                    }
                    CandidateDecision::BlockedByAncestor(root) => blocked = Some(root.clone()),
                    _ => {}
                }
            }
            parent = path.parent();
        }
        if let Some(ancestor) = blocked {
            rows[index].decision = CandidateDecision::BlockedByAncestor(ancestor);
        }
    }

    let deletion_counts = |target: Side, paths: usize| -> Result<DeletionCounts, PlanError> {
        let deletions = rows
            .iter()
            .filter(|row| {
                matches!(
                    &row.decision,
                    CandidateDecision::Entry(EntryDecision::Archive { on }) if *on == target
                )
            })
            .count();
        Ok(DeletionCounts::new(
            count_paths(paths)?,
            count_paths(deletions)?,
        )?)
    };
    let left_deletions = deletion_counts(Side::Left, left.len())?;
    let right_deletions = deletion_counts(Side::Right, right.len())?;
    check_control(control)?;
    Ok(CandidatePlan {
        rows,
        left_deletions,
        right_deletions,
    })
}

fn validate_tree<T>(
    entries: &BTreeMap<SyncPath, T>,
    tree: TreeRole,
    kind: impl Fn(&T) -> EntryKind,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<(), PlanError> {
    for path in entries.keys() {
        check_control(control)?;
        let Some(parent) = path.parent() else {
            continue;
        };
        let Some(entry) = entries.get(&parent) else {
            return Err(PlanError::MissingParent {
                tree,
                path: path.clone(),
                parent,
            });
        };
        if kind(entry) != EntryKind::Directory {
            return Err(PlanError::NonDirectoryParent {
                tree,
                path: path.clone(),
                parent,
            });
        }
    }
    Ok(())
}

fn count_paths(count: usize) -> Result<u64, PlanError> {
    u64::try_from(count).map_err(|_| PlanError::CountOverflow)
}

fn check_control(control: &mut impl FnMut() -> ScanControl) -> Result<(), PlanError> {
    if control() == ScanControl::Cancel {
        return Err(PlanError::Cancelled);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use foldover_types::{ChangeKind, Digest, EntryContent};

    fn path(bytes: &[u8]) -> SyncPath {
        SyncPath::try_from(bytes).unwrap()
    }

    fn file(content: u8) -> EntryVersion {
        EntryVersion::new(
            EntryContent::File(Digest::from_bytes([content; Digest::BYTE_LEN])),
            Digest::from_bytes([0; Digest::BYTE_LEN]),
        )
    }

    fn directory() -> EntryVersion {
        EntryVersion::new(
            EntryContent::Directory,
            Digest::from_bytes([0; Digest::BYTE_LEN]),
        )
    }

    fn tree(entries: &[(&[u8], EntryVersion)]) -> BTreeMap<SyncPath, EntryVersion> {
        entries
            .iter()
            .map(|(name, entry)| (path(name), *entry))
            .collect()
    }

    fn baseline(entries: &BTreeMap<SyncPath, EntryVersion>) -> BTreeMap<SyncPath, BaselineEntry> {
        entries
            .iter()
            .map(|(path, entry)| (path.clone(), BaselineEntry::new(*entry, *entry).unwrap()))
            .collect()
    }

    fn plan(
        mode: SyncMode,
        base: &BTreeMap<SyncPath, BaselineEntry>,
        left: &BTreeMap<SyncPath, EntryVersion>,
        right: &BTreeMap<SyncPath, EntryVersion>,
    ) -> CandidatePlan {
        plan_candidates(mode, base, left, right, &mut || ScanControl::Continue).unwrap()
    }

    fn decision(plan: &CandidatePlan, name: &[u8]) -> CandidateDecision {
        plan.rows
            .iter()
            .find(|row| row.path == path(name))
            .unwrap()
            .decision
            .clone()
    }

    #[test]
    fn surviving_child_edits_or_creations_block_ancestor_deletion() {
        for child in [file(0), file(1)] {
            let original = tree(&[(b"folder", directory()), (b"folder/file", file(0))]);
            let base = if child == file(0) {
                baseline(&tree(&[(b"folder", directory())]))
            } else {
                baseline(&original)
            };
            let surviving = tree(&[(b"folder", directory()), (b"folder/file", child)]);
            for (left, right) in [
                (BTreeMap::new(), surviving.clone()),
                (surviving.clone(), BTreeMap::new()),
            ] {
                let candidate = plan(SyncMode::Sync, &base, &left, &right);
                assert_eq!(
                    decision(&candidate, b"folder"),
                    CandidateDecision::Entry(EntryDecision::Conflict(
                        ConflictKind::DeletedVersusEdited
                    ))
                );
                assert_eq!(
                    decision(&candidate, b"folder/file"),
                    CandidateDecision::BlockedByAncestor(path(b"folder"))
                );
                for side in [Side::Left, Side::Right] {
                    assert_eq!(
                        candidate.deletion_review(side, RunContext::Unattended),
                        DeletionReview::WithinLimits
                    );
                }
            }
        }
    }

    #[test]
    fn unchanged_deleted_subtrees_and_mirror_extras_remain_archive_candidates() {
        let target = tree(&[(b"folder", directory()), (b"folder/file", file(0))]);
        for (mode, base) in [
            (SyncMode::Sync, baseline(&target)),
            (SyncMode::Mirror { source: Side::Left }, BTreeMap::new()),
        ] {
            let candidate = plan(mode, &base, &BTreeMap::new(), &target);
            assert!(candidate.rows.iter().all(|row| row.decision
                == CandidateDecision::Entry(EntryDecision::Archive { on: Side::Right })));
            assert_eq!(
                candidate.deletion_review(Side::Right, RunContext::Unattended),
                DeletionReview::StopUnattended
            );
            assert_eq!(
                candidate.deletion_review(Side::Left, RunContext::Unattended),
                DeletionReview::WithinLimits
            );
        }
    }

    #[test]
    fn type_conflicts_block_descendants_and_first_run_creates_parents() {
        let source = tree(&[(b"folder", directory()), (b"folder/file", file(0))]);
        let target = tree(&[(b"folder", file(1))]);
        let candidate = plan(SyncMode::Sync, &BTreeMap::new(), &source, &target);
        assert_eq!(
            decision(&candidate, b"folder"),
            CandidateDecision::Entry(EntryDecision::Conflict(ConflictKind::TypeMismatch))
        );
        assert_eq!(
            decision(&candidate, b"folder/file"),
            CandidateDecision::BlockedByAncestor(path(b"folder"))
        );
        let candidate = plan(SyncMode::Sync, &BTreeMap::new(), &source, &BTreeMap::new());
        assert!(candidate.rows.iter().all(|row| row.decision
            == CandidateDecision::Entry(EntryDecision::Propagate {
                from: Side::Left,
                change: ChangeKind::Create
            })));
    }

    #[test]
    fn incomplete_or_impossible_tree_shapes_are_errors() {
        let missing = tree(&[(b"folder/file", file(0))]);
        let invalid = tree(&[(b"folder", file(0)), (b"folder/file", file(0))]);
        for entries in [missing, invalid] {
            for role in [TreeRole::Base, TreeRole::Left, TreeRole::Right] {
                let empty = BTreeMap::new();
                let base = if role == TreeRole::Base {
                    baseline(&entries)
                } else {
                    BTreeMap::new()
                };
                let left = if role == TreeRole::Left {
                    &entries
                } else {
                    &empty
                };
                let right = if role == TreeRole::Right {
                    &entries
                } else {
                    &empty
                };
                assert!(
                    matches!(plan_candidates(SyncMode::Sync, &base, left, right, &mut || ScanControl::Continue), Err(PlanError::MissingParent { tree, .. } | PlanError::NonDirectoryParent { tree, .. }) if tree == role)
                );
            }
        }
    }

    #[test]
    fn raw_byte_order_is_deterministic_and_keeps_distinct_names() {
        let entries = tree(&[(b"a", directory()), (b"a/\xff", file(0)), (b"a-z", file(1))]);
        let candidate = plan(SyncMode::Sync, &BTreeMap::new(), &entries, &BTreeMap::new());
        assert_eq!(
            candidate
                .rows
                .iter()
                .map(|row| row.path.as_bytes())
                .collect::<Vec<_>>(),
            vec![b"a".as_slice(), b"a-z", b"a/\xff"]
        );
    }

    #[test]
    fn cancellation_at_each_checkpoint_never_returns_partial_candidates() {
        let left = tree(&[(b"folder", directory()), (b"folder/file", file(0))]);
        let mut checkpoints = 0;
        plan_candidates(
            SyncMode::Sync,
            &BTreeMap::new(),
            &left,
            &BTreeMap::new(),
            &mut || {
                checkpoints += 1;
                ScanControl::Continue
            },
        )
        .unwrap();
        for cancel_at in 0..checkpoints {
            let mut checkpoint = 0;
            let result = plan_candidates(
                SyncMode::Sync,
                &BTreeMap::new(),
                &left,
                &BTreeMap::new(),
                &mut || {
                    let control = if checkpoint == cancel_at {
                        ScanControl::Cancel
                    } else {
                        ScanControl::Continue
                    };
                    checkpoint += 1;
                    control
                },
            );
            assert!(
                matches!(result, Err(PlanError::Cancelled)),
                "checkpoint={cancel_at}"
            );
        }
    }

    #[test]
    fn deep_conflicts_suppress_the_whole_outer_branch_including_base_repairs() {
        let original = tree(&[
            (b"folder", directory()),
            (b"folder/sub", directory()),
            (b"folder/sub/file", file(0)),
        ]);
        let mut edited = original.clone();
        edited.insert(path(b"folder/sub/file"), file(1));
        let candidate = plan(
            SyncMode::Sync,
            &baseline(&original),
            &BTreeMap::new(),
            &edited,
        );
        for name in [b"folder/sub".as_slice(), b"folder/sub/file"] {
            assert_eq!(
                decision(&candidate, name),
                CandidateDecision::BlockedByAncestor(path(b"folder"))
            );
        }
        let changed_type = tree(&[(b"folder", file(1))]);
        let candidate = plan(
            SyncMode::Sync,
            &baseline(&original),
            &changed_type,
            &BTreeMap::new(),
        );
        assert_eq!(
            decision(&candidate, b"folder/sub/file"),
            CandidateDecision::BlockedByAncestor(path(b"folder"))
        );
    }

    #[test]
    fn finite_tree_model_preserves_side_symmetry_and_conflicted_branches() {
        let states = [
            BTreeMap::new(),
            tree(&[(b"folder", directory())]),
            tree(&[(b"folder", file(0))]),
            tree(&[(b"folder", file(1))]),
            tree(&[(b"folder", directory()), (b"folder/file", file(0))]),
            tree(&[(b"folder", directory()), (b"folder/file", file(1))]),
            tree(&[
                (b"folder", directory()),
                (b"folder/sub", directory()),
                (b"folder/sub/file", file(0)),
            ]),
        ];
        for base in &states {
            let base = baseline(base);
            for left in &states {
                for right in &states {
                    for (mode, swapped_mode) in [
                        (SyncMode::Sync, SyncMode::Sync),
                        (
                            SyncMode::Mirror { source: Side::Left },
                            SyncMode::Mirror {
                                source: Side::Right,
                            },
                        ),
                        (
                            SyncMode::Backup { source: Side::Left },
                            SyncMode::Backup {
                                source: Side::Right,
                            },
                        ),
                    ] {
                        let candidate = plan(mode, &base, left, right);
                        let swapped = plan(swapped_mode, &base, right, left);
                        assert_eq!(candidate.rows.len(), swapped.rows.len());
                        assert!(
                            candidate
                                .rows
                                .windows(2)
                                .all(|rows| rows[0].path < rows[1].path)
                        );
                        for (row, other) in candidate.rows.iter().zip(&swapped.rows) {
                            assert_eq!(row.path, other.path);
                            let expected = match &row.decision {
                                CandidateDecision::Entry(EntryDecision::Propagate {
                                    from,
                                    change,
                                }) => CandidateDecision::Entry(EntryDecision::Propagate {
                                    from: from.other(),
                                    change: *change,
                                }),
                                CandidateDecision::Entry(EntryDecision::Archive { on }) => {
                                    CandidateDecision::Entry(EntryDecision::Archive {
                                        on: on.other(),
                                    })
                                }
                                other => other.clone(),
                            };
                            assert_eq!(other.decision, expected);
                            if let CandidateDecision::BlockedByAncestor(ancestor) = &row.decision {
                                assert!(ancestor.is_ancestor_of(&row.path));
                                assert!(matches!(
                                    decision(&candidate, ancestor.as_bytes()),
                                    CandidateDecision::Entry(EntryDecision::Conflict(_))
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "500k-path planning measurement; run in release mode"]
    fn half_million_path_candidate_measurement() {
        let mut left = BTreeMap::new();
        for folder in 0..5_000 {
            let name = format!("folder-{folder:04}");
            left.insert(path(name.as_bytes()), directory());
            for child in 0..99 {
                left.insert(path(format!("{name}/file-{child:02}").as_bytes()), file(0));
            }
        }
        let started = std::time::Instant::now();
        let candidate = plan(SyncMode::Sync, &BTreeMap::new(), &left, &BTreeMap::new());
        let elapsed = started.elapsed();
        assert_eq!(candidate.rows.len(), 500_000);
        assert!(candidate.rows.iter().all(|row| row.decision
            == CandidateDecision::Entry(EntryDecision::Propagate {
                from: Side::Left,
                change: ChangeKind::Create
            })));
        eprintln!("500,000 candidate paths in {elapsed:?}");
    }
}
