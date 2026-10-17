use foldover_types::{
    BaselineEntry, ChangeKind, ConflictKind, EntryDecision, EntryVersion, Side, SyncMode,
};

/// Classify one fully observed path (PLAN.md §4.7). Absence must mean a
/// completed scan found no entry, never a read error or unavailable root.
/// A new pair supplies no base, so Sync cannot infer a deletion on first run.
pub fn reconcile_entry(
    mode: SyncMode,
    base: Option<BaselineEntry>,
    left: Option<EntryVersion>,
    right: Option<EntryVersion>,
) -> EntryDecision {
    if base.is_some_and(|base| left == Some(base.left()) && right == Some(base.right())) {
        return EntryDecision::Unchanged;
    }

    if left == right {
        return match left {
            Some(_) => EntryDecision::RecordBase,
            None if base.is_some() => EntryDecision::ForgetBase,
            None => EntryDecision::Unchanged,
        };
    }

    if let (Some(left), Some(right)) = (left, right)
        && left.kind() != right.kind()
    {
        return EntryDecision::Conflict(ConflictKind::TypeMismatch);
    }

    match mode {
        SyncMode::Sync => reconcile_sync(base, left, right),
        SyncMode::Mirror { source } => {
            reconcile_one_way(source, left, right, MissingSource::Archive)
        }
        SyncMode::Backup { source } => {
            // Backup propagates source edits, not independent edits on its target.
            let source_unchanged = base.is_some_and(|base| match source {
                Side::Left => left == Some(base.left()),
                Side::Right => right == Some(base.right()),
            });
            let target = match source {
                Side::Left => right,
                Side::Right => left,
            };
            if source_unchanged && target.is_some() {
                return EntryDecision::KeepTarget;
            }
            reconcile_one_way(source, left, right, MissingSource::Retain)
        }
    }
}

fn reconcile_sync(
    base: Option<BaselineEntry>,
    left: Option<EntryVersion>,
    right: Option<EntryVersion>,
) -> EntryDecision {
    let left_changed = left != base.map(BaselineEntry::left);
    let right_changed = right != base.map(BaselineEntry::right);

    if left_changed && !right_changed {
        return match left {
            Some(source) => propagate(Side::Left, source, right),
            None => EntryDecision::Archive { on: Side::Right },
        };
    }
    if right_changed && !left_changed {
        return match right {
            Some(source) => propagate(Side::Right, source, left),
            None => EntryDecision::Archive { on: Side::Left },
        };
    }

    match (left, right) {
        (Some(left), Some(right)) if left.content() == right.content() => {
            EntryDecision::Conflict(ConflictKind::MetadataOnly)
        }
        (Some(_), Some(_)) => EntryDecision::Conflict(ConflictKind::BothChanged),
        _ => EntryDecision::Conflict(ConflictKind::DeletedVersusEdited),
    }
}

enum MissingSource {
    Archive,
    Retain,
}

fn reconcile_one_way(
    source_side: Side,
    left: Option<EntryVersion>,
    right: Option<EntryVersion>,
    missing: MissingSource,
) -> EntryDecision {
    let (source, target) = match source_side {
        Side::Left => (left, right),
        Side::Right => (right, left),
    };
    match (source, target) {
        (Some(source), _) => propagate(source_side, source, target),
        (None, Some(_)) => match missing {
            MissingSource::Archive => EntryDecision::Archive {
                on: source_side.other(),
            },
            MissingSource::Retain => EntryDecision::KeepTarget,
        },
        (None, None) => EntryDecision::Unchanged,
    }
}

fn propagate(from: Side, source: EntryVersion, target: Option<EntryVersion>) -> EntryDecision {
    let change = match target {
        None => ChangeKind::Create,
        Some(target) if source.content() == target.content() => ChangeKind::UpdateMetadata,
        Some(_) => ChangeKind::ReplaceContent,
    };
    EntryDecision::Propagate { from, change }
}

#[cfg(test)]
mod tests {
    use super::*;
    use foldover_types::{Digest, EntryContent};

    fn file(content: u8, metadata: u8) -> EntryVersion {
        EntryVersion::new(
            EntryContent::File(Digest::from_bytes([content; Digest::BYTE_LEN])),
            Digest::from_bytes([metadata; Digest::BYTE_LEN]),
        )
    }

    fn base(version: EntryVersion) -> Option<BaselineEntry> {
        Some(BaselineEntry::new(version, version).unwrap())
    }

    #[test]
    fn three_way_table_preserves_changes_and_detects_conflicts() {
        let original = file(0, 0);
        let edited = file(1, 0);
        let other_edit = file(2, 0);
        let cases = [
            (Some(original), Some(original), EntryDecision::Unchanged),
            (
                Some(edited),
                Some(original),
                EntryDecision::Propagate {
                    from: Side::Left,
                    change: ChangeKind::ReplaceContent,
                },
            ),
            (Some(edited), Some(edited), EntryDecision::RecordBase),
            (
                Some(edited),
                Some(other_edit),
                EntryDecision::Conflict(ConflictKind::BothChanged),
            ),
            (
                None,
                Some(original),
                EntryDecision::Archive { on: Side::Right },
            ),
            (
                None,
                Some(edited),
                EntryDecision::Conflict(ConflictKind::DeletedVersusEdited),
            ),
            (None, None, EntryDecision::ForgetBase),
        ];
        for (left, right, expected) in cases {
            assert_eq!(
                reconcile_entry(SyncMode::Sync, base(original), left, right),
                expected,
                "left={left:?}, right={right:?}"
            );
        }
    }

    #[test]
    fn first_run_creates_missing_entries_and_never_propagates_deletion() {
        let entry = file(0, 0);
        assert_eq!(
            reconcile_entry(SyncMode::Sync, None, Some(entry), None),
            EntryDecision::Propagate {
                from: Side::Left,
                change: ChangeKind::Create,
            }
        );
        assert_eq!(
            reconcile_entry(SyncMode::Sync, None, None, Some(entry)),
            EntryDecision::Propagate {
                from: Side::Right,
                change: ChangeKind::Create,
            }
        );
        assert_eq!(
            reconcile_entry(SyncMode::Sync, None, Some(entry), Some(file(1, 0))),
            EntryDecision::Conflict(ConflictKind::BothChanged)
        );
    }

    #[test]
    fn accepted_metadata_loss_does_not_cause_a_recopy_loop() {
        let left = file(0, 1);
        let right = file(0, 2);
        let base = Some(BaselineEntry::new(left, right).unwrap());
        for mode in modes() {
            assert_eq!(
                reconcile_entry(mode, base, Some(left), Some(right)),
                EntryDecision::Unchanged
            );
        }
        assert_eq!(
            reconcile_entry(SyncMode::Sync, base, Some(file(0, 0)), Some(right)),
            EntryDecision::Propagate {
                from: Side::Left,
                change: ChangeKind::UpdateMetadata,
            }
        );
        assert_eq!(
            reconcile_entry(SyncMode::Sync, None, Some(left), Some(right)),
            EntryDecision::Conflict(ConflictKind::MetadataOnly)
        );
    }

    #[test]
    fn one_way_modes_keep_source_authority_and_backup_extras() {
        let source = file(0, 0);
        let previous_source = file(2, 0);
        let changed_target = file(1, 0);
        for mode in [
            SyncMode::Mirror { source: Side::Left },
            SyncMode::Backup { source: Side::Left },
        ] {
            assert_eq!(
                reconcile_entry(
                    mode,
                    base(previous_source),
                    Some(source),
                    Some(changed_target)
                ),
                EntryDecision::Propagate {
                    from: Side::Left,
                    change: ChangeKind::ReplaceContent,
                }
            );
        }
        assert_eq!(
            reconcile_entry(
                SyncMode::Backup { source: Side::Left },
                None,
                None,
                Some(source)
            ),
            EntryDecision::KeepTarget
        );
        assert_eq!(
            reconcile_entry(
                SyncMode::Mirror { source: Side::Left },
                None,
                None,
                Some(source)
            ),
            EntryDecision::Archive { on: Side::Right }
        );
    }

    #[test]
    fn backup_preserves_target_edits_when_its_source_has_not_changed() {
        let source = file(0, 0);
        assert_eq!(
            reconcile_entry(
                SyncMode::Backup { source: Side::Left },
                base(source),
                Some(source),
                Some(file(1, 0)),
            ),
            EntryDecision::KeepTarget
        );
    }

    fn modes() -> [SyncMode; 5] {
        [
            SyncMode::Sync,
            SyncMode::Mirror { source: Side::Left },
            SyncMode::Mirror {
                source: Side::Right,
            },
            SyncMode::Backup { source: Side::Left },
            SyncMode::Backup {
                source: Side::Right,
            },
        ]
    }

    fn versions() -> Vec<Option<EntryVersion>> {
        let mut versions = vec![None];
        for metadata in 0..3 {
            let metadata = Digest::from_bytes([metadata; Digest::BYTE_LEN]);
            versions.push(Some(EntryVersion::new(EntryContent::Directory, metadata)));
            for content in 0..3 {
                let content = Digest::from_bytes([content; Digest::BYTE_LEN]);
                for kind in [EntryContent::File(content), EntryContent::Symlink(content)] {
                    versions.push(Some(EntryVersion::new(kind, metadata)));
                }
            }
        }
        versions
    }

    fn swapped(decision: EntryDecision) -> EntryDecision {
        match decision {
            EntryDecision::Propagate { from, change } => EntryDecision::Propagate {
                from: from.other(),
                change,
            },
            EntryDecision::Archive { on } => EntryDecision::Archive { on: on.other() },
            other => other,
        }
    }

    #[test]
    fn finite_model_checks_symmetry_deletion_and_one_way_safety() {
        let versions = versions();
        let mut bases = vec![None];
        for left in versions.iter().flatten() {
            for right in versions.iter().flatten() {
                if let Ok(base) = BaselineEntry::new(*left, *right) {
                    bases.push(Some(base));
                }
            }
        }
        for base in bases {
            for &left in &versions {
                for &right in &versions {
                    for mode in modes() {
                        let decision = reconcile_entry(mode, base, left, right);
                        if mode == SyncMode::Sync
                            && left != right
                            && left != base.map(BaselineEntry::left)
                            && right != base.map(BaselineEntry::right)
                        {
                            assert!(matches!(decision, EntryDecision::Conflict(_)));
                        }
                        if let (Some(left), Some(right)) = (left, right)
                            && left.kind() != right.kind()
                        {
                            assert_eq!(
                                decision,
                                EntryDecision::Conflict(ConflictKind::TypeMismatch)
                            );
                        }
                        if let EntryDecision::Propagate { from, change } = decision {
                            let (source, target) = match from {
                                Side::Left => (left, right),
                                Side::Right => (right, left),
                            };
                            let source = source.expect("propagation needs an observed source");
                            if change == ChangeKind::Create {
                                assert!(target.is_none());
                            } else {
                                assert_eq!(Some(source.kind()), target.map(EntryVersion::kind));
                            }
                            if let SyncMode::Mirror { source } | SyncMode::Backup { source } = mode
                            {
                                assert_eq!(from, source);
                            }
                        }
                        if let EntryDecision::Archive { on } = decision {
                            assert!(!matches!(mode, SyncMode::Backup { .. }));
                            if mode == SyncMode::Sync {
                                let base = base.expect("never archive an untracked Sync entry");
                                match on {
                                    Side::Left => {
                                        assert_eq!(left, Some(base.left()));
                                        assert!(right.is_none());
                                    }
                                    Side::Right => {
                                        assert_eq!(right, Some(base.right()));
                                        assert!(left.is_none());
                                    }
                                }
                            }
                        }
                        let reverse_mode = match mode {
                            SyncMode::Sync => SyncMode::Sync,
                            SyncMode::Mirror { source } => SyncMode::Mirror {
                                source: source.other(),
                            },
                            SyncMode::Backup { source } => SyncMode::Backup {
                                source: source.other(),
                            },
                        };
                        let reverse_base =
                            base.map(|base| BaselineEntry::new(base.right(), base.left()).unwrap());
                        assert_eq!(
                            reconcile_entry(reverse_mode, reverse_base, right, left),
                            swapped(decision)
                        );
                    }
                }
            }
        }
    }
}
