use std::collections::{BTreeMap, HashMap};

use unicode_normalization::UnicodeNormalization;

use foldover_types::{
    BaselineEntry, CaseMatching, ChangeKind, ConflictKind, EntryContent, EntryDecision, EntryKind,
    EntryVersion, NameMatching, Resolution, ScanControl, Side, Skip, SyncMode, SyncPath,
    UnicodeMatching,
};

use crate::reconcile::resolve;
use crate::{DeletionCounts, DeletionReview, InvalidDeletionCounts, RunContext, reconcile_entry};

fn decide(
    decision: EntryDecision,
    resolution: Option<&Resolution>,
    left: Option<EntryVersion>,
    right: Option<EntryVersion>,
) -> EntryDecision {
    resolution
        .and_then(|resolution| resolve(*resolution, left, right))
        .unwrap_or(decision)
}

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
    /// Each `Move` row's path, with the path it moves from.
    moves: BTreeMap<SyncPath, SyncPath>,
    left_deletions: DeletionCounts,
    right_deletions: DeletionCounts,
}

impl CandidatePlan {
    pub fn rows(&self) -> &[PlanRow] {
        &self.rows
    }

    /// Where the entry a `Move` row at `to` puts there comes from.
    pub fn moved_from(&self, to: &SyncPath) -> Option<&SyncPath> {
        self.moves.get(to)
    }

    pub fn deletion_review(&self, target: Side, context: RunContext) -> DeletionReview {
        self.deletions(target).review(context)
    }

    pub fn deletions(&self, target: Side) -> DeletionCounts {
        match target {
            Side::Left => self.left_deletions,
            Side::Right => self.right_deletions,
        }
    }
}

/// A choice made for a folder present on one side only — a conflict where a
/// folder was deleted on one side while something inside it changed on the
/// other, or a folder about to be copied or archived — covers the folder
/// and everything in it: the kept side wins each entry, as in a mirror from
/// it. `resolve` cannot do this per entry, because a folder's own versions
/// do not change when its contents do; without this a folder conflict
/// would block its branch forever.
///
/// ```text
/// photos/        deleted on B, kept A   ─▶ create on B
/// photos/a.jpg   edited on A            ─▶ create on B
/// photos/b.jpg   unchanged on A         ─▶ create on B   (kept B: all archived on A)
/// ```
fn settle_folder_choices(
    rows: &mut [PlanRow],
    base: &BTreeMap<SyncPath, BaselineEntry>,
    left: &BTreeMap<SyncPath, EntryVersion>,
    right: &BTreeMap<SyncPath, EntryVersion>,
    resolutions: &BTreeMap<SyncPath, Resolution>,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<(), PlanError> {
    let mut branches: BTreeMap<SyncPath, Side> = BTreeMap::new();
    for row in rows.iter_mut() {
        check_control(control)?;
        let (left, right) = (left.get(&row.path).copied(), right.get(&row.path).copied());
        let mut keep = None;
        let mut parent = row.path.parent();
        while let Some(path) = parent {
            if let Some(side) = branches.get(&path) {
                keep = Some(*side);
            }
            parent = path.parent();
        }
        if keep.is_none() {
            keep = folder_choice(resolutions.get(&row.path), left, right);
            if let Some(side) = keep {
                branches.insert(row.path.clone(), side);
            }
        }
        let Some(keep) = keep else {
            continue;
        };
        row.decision = CandidateDecision::Entry(reconcile_entry(
            SyncMode::Mirror { source: keep },
            base.get(&row.path).copied(),
            left,
            right,
        ));
    }
    Ok(())
}

/// Leave each skipped change and everything inside it as it is: no write,
/// no base change. A skip applies while both sides still hold what the
/// person saw at the skipped path.
fn skip_branches(
    rows: &mut [PlanRow],
    left: &BTreeMap<SyncPath, EntryVersion>,
    right: &BTreeMap<SyncPath, EntryVersion>,
    skips: &BTreeMap<SyncPath, Skip>,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<(), PlanError> {
    if skips.is_empty() {
        return Ok(());
    }
    let mut skipped: Vec<SyncPath> = Vec::new();
    for row in rows.iter_mut() {
        check_control(control)?;
        let inside = skipped.iter().any(|root| root.is_ancestor_of(&row.path));
        let current = skips.get(&row.path).is_some_and(|skip| {
            (left.get(&row.path).copied(), right.get(&row.path).copied())
                == (skip.left(), skip.right())
        });
        if current {
            skipped.push(row.path.clone());
        }
        if inside || current {
            row.decision = CandidateDecision::Entry(EntryDecision::KeepTarget);
        }
    }
    Ok(())
}

/// Paths whose names one of the folders would store as one file are a
/// conflict for a person to rename (PLAN.md §4.10) when they would meet
/// there: two of them written, or one written beside another that stays.
/// A spelling in conflict stays too: resolving it may write it there.
/// Copying either spelling would fail against, or silently become, the
/// other. A folder that keeps the spellings apart takes them
/// as they are, so a case-only rename on a Mac reaches a byte-exact server
/// as an archive and a copy. Folders that clash block what is inside them,
/// like any folder conflict.
///
/// ```text
/// A ignores case, B keeps spellings apart
/// A: Report.txt  B: report.txt  each would be copied to A as one file: conflict
/// A: report.txt  B: Report.txt  A renamed it: B archives Report.txt, copies
///                               report.txt
/// ```
fn mark_name_clashes(
    rows: &mut [PlanRow],
    [left, right]: [&BTreeMap<SyncPath, EntryVersion>; 2],
    left_names: NameMatching,
    right_names: NameMatching,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<(), PlanError> {
    let loose = left_names.either(right_names);
    if loose == NameMatching::EXACT {
        return Ok(());
    }
    let mut keys: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        check_control(control)?;
        keys.entry(name_key(row.path.as_bytes(), loose))
            .or_default()
            .push(index);
    }
    let mut clashes = Vec::new();
    for group in keys.into_values().filter(|group| group.len() > 1) {
        for (side, names, present) in [
            (Side::Left, left_names, left),
            (Side::Right, right_names, right),
        ] {
            // Spellings this folder stores as one file.
            let mut same: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
            for &index in &group {
                check_control(control)?;
                same.entry(name_key(rows[index].path.as_bytes(), names))
                    .or_default()
                    .push(index);
            }
            // One write is safe while no other spelling stays there; two
            // writes, or a write beside a spelling that stays, meet.
            for spellings in same.into_values().filter(|spellings| spellings.len() > 1) {
                let (writes, untouched): (Vec<usize>, Vec<usize>) = spellings
                    .iter()
                    .partition(|&&index| writes_on(&rows[index].decision, side));
                let stays = untouched.iter().any(|&index| {
                    present.contains_key(&rows[index].path)
                        || matches!(
                            rows[index].decision,
                            CandidateDecision::Entry(EntryDecision::Conflict(_))
                        )
                });
                if writes.len() > 1 || (!writes.is_empty() && stays) {
                    clashes.extend(spellings);
                }
            }
        }
    }
    for index in clashes {
        rows[index].decision =
            CandidateDecision::Entry(EntryDecision::Conflict(ConflictKind::NameClash));
    }
    Ok(())
}

/// Whether `decision` writes or archives on `side`.
fn writes_on(decision: &CandidateDecision, side: Side) -> bool {
    match decision {
        CandidateDecision::Entry(EntryDecision::Propagate { from, .. }) => from.other() == side,
        CandidateDecision::Entry(EntryDecision::Archive { on }) => *on == side,
        _ => false,
    }
}

/// The bytes two names share when `names` treats them as one: composed
/// Unicode (NFC) where normalization is ignored, lower case where case is.
/// Names that are not UTF-8 compare as bytes, ASCII case folded.
pub(crate) fn name_key(path: &[u8], names: NameMatching) -> Vec<u8> {
    let insensitive = names.case() == CaseMatching::Insensitive;
    let Ok(text) = std::str::from_utf8(path) else {
        return if insensitive {
            path.to_ascii_lowercase()
        } else {
            path.to_vec()
        };
    };
    let text: String = match names.unicode() {
        UnicodeMatching::Normalized => text.nfc().collect(),
        UnicodeMatching::Exact => text.to_owned(),
    };
    if insensitive {
        text.to_lowercase().into_bytes()
    } else {
        text.into_bytes()
    }
}

/// The side a person chose for this folder, present on one side only,
/// while both sides still match what they saw.
fn folder_choice(
    resolution: Option<&Resolution>,
    left: Option<EntryVersion>,
    right: Option<EntryVersion>,
) -> Option<Side> {
    let resolution = resolution?;
    let folder = match (left, right) {
        (Some(version), None) | (None, Some(version)) => version.kind() == EntryKind::Directory,
        _ => false,
    };
    let current = (left, right) == (resolution.left(), resolution.right());
    (folder && current).then_some(resolution.keep())
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
    plan_resolved(mode, base, left, right, &Choices::NONE, control)
}

/// What a person chose, and how the folders compare names, for planning.
pub struct Choices<'a> {
    /// The side kept for a change or conflict.
    pub resolutions: &'a BTreeMap<SyncPath, Resolution>,
    /// Changes left alone in this run.
    pub skips: &'a BTreeMap<SyncPath, Skip>,
    /// How each folder compares names.
    pub left_names: NameMatching,
    pub right_names: NameMatching,
    /// Whether each folder takes renames for what the other side moved.
    pub left_moves: Moves,
    pub right_moves: Moves,
}

/// Whether a folder takes a rename for a file or link the other side
/// moved, or a copy and an archive as for any new and removed file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Moves {
    Rename,
    Copy,
}

impl Choices<'static> {
    const NONE: Self = Self {
        resolutions: &BTreeMap::new(),
        skips: &BTreeMap::new(),
        left_names: NameMatching::EXACT,
        right_names: NameMatching::EXACT,
        left_moves: Moves::Copy,
        right_moves: Moves::Copy,
    };
}

/// `plan_candidates`, with the side a person chose for a change or conflict
/// turned into that side's change, and changes they skipped left alone,
/// while both sides still match what they saw.
pub fn plan_resolved(
    mode: SyncMode,
    base: &BTreeMap<SyncPath, BaselineEntry>,
    left: &BTreeMap<SyncPath, EntryVersion>,
    right: &BTreeMap<SyncPath, EntryVersion>,
    choices: &Choices<'_>,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<CandidatePlan, PlanError> {
    let Choices {
        resolutions,
        skips,
        left_names,
        right_names,
        left_moves,
        right_moves,
    } = *choices;
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
            decision: CandidateDecision::Entry(decide(
                reconcile_entry(mode, baseline, left, right),
                resolutions.get(&path),
                left,
                right,
            )),
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

    settle_folder_choices(&mut rows, base, left, right, resolutions, control)?;
    skip_branches(&mut rows, left, right, skips, control)?;
    mark_name_clashes(&mut rows, [left, right], left_names, right_names, control)?;

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
    let moves = pair_moves(&mut rows, [left, right], [left_moves, right_moves]);

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
        moves,
        left_deletions,
        right_deletions,
    })
}

/// PLAN.md §4.7: a file or link one side moved arrives as a rename on the
/// other, not as an archive and a copy. Each archive on a folder pairs
/// with a create on that folder of the same content, one to one in path
/// order. Rows in conflicts or blocked by one were settled before, and
/// folders carry no content to pair, so their contents pair instead.
///
/// ```text
/// left moved a/x.jpg to b/x.jpg; right still holds a/x.jpg
/// a/x.jpg  Archive { right }              ─▶ MovedAway { right }
/// b/x.jpg  Propagate { left, Create }     ─▶ Move { right }: rename a/x.jpg
/// ```
fn pair_moves(
    rows: &mut [PlanRow],
    trees: [&BTreeMap<SyncPath, EntryVersion>; 2],
    moves: [Moves; 2],
) -> BTreeMap<SyncPath, SyncPath> {
    let takes = |side: Side| moves[side as usize] == Moves::Rename;
    let movable = |side: Side, path: &SyncPath| {
        trees[side as usize]
            .get(path)
            .map(|version| version.content())
            .filter(|content| matches!(content, EntryContent::File(_) | EntryContent::Symlink(_)))
    };
    let mut gone: HashMap<(Side, EntryContent), Vec<usize>> = HashMap::new();
    let mut came: HashMap<(Side, EntryContent), Vec<usize>> = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        let CandidateDecision::Entry(decision) = &row.decision else {
            continue;
        };
        match *decision {
            EntryDecision::Archive { on } if takes(on) => {
                if let Some(content) = movable(on, &row.path) {
                    gone.entry((on, content)).or_default().push(index);
                }
            }
            EntryDecision::Propagate {
                from,
                change: ChangeKind::Create,
            } if takes(from.other()) => {
                if let Some(content) = movable(from, &row.path) {
                    came.entry((from.other(), content)).or_default().push(index);
                }
            }
            _ => {}
        }
    }
    let mut paired = BTreeMap::new();
    for (&(on, content), sources) in &gone {
        let Some(targets) = came.get(&(on, content)) else {
            continue;
        };
        for (&from, &to) in sources.iter().zip(targets) {
            rows[from].decision = CandidateDecision::Entry(EntryDecision::MovedAway { on });
            rows[to].decision = CandidateDecision::Entry(EntryDecision::Move { on });
            paired.insert(rows[to].path.clone(), rows[from].path.clone());
        }
    }
    paired
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

    /// A plan where both folders take renames.
    fn plan_moving(
        mode: SyncMode,
        base: &BTreeMap<SyncPath, BaselineEntry>,
        left: &BTreeMap<SyncPath, EntryVersion>,
        right: &BTreeMap<SyncPath, EntryVersion>,
        right_moves: Moves,
    ) -> CandidatePlan {
        let choices = Choices {
            left_moves: Moves::Rename,
            right_moves,
            ..Choices::NONE
        };
        plan_resolved(mode, base, left, right, &choices, &mut || {
            ScanControl::Continue
        })
        .unwrap()
    }

    /// PLAN.md §4.7: what one folder moved, the other renames: an archive
    /// and a create of the same content on a folder become one move.
    #[test]
    fn a_moved_file_pairs_its_archive_with_its_copy() {
        let base = baseline(&tree(&[
            (b"a", directory()),
            (b"a/x", file(1)),
            (b"a/y", file(2)),
        ]));
        let left = tree(&[
            (b"a", directory()),
            (b"a/y", file(2)),
            (b"b", directory()),
            (b"b/x", file(1)),
            (b"b/z", file(2)),
        ]);
        let right = tree(&[(b"a", directory()), (b"a/x", file(1)), (b"a/y", file(2))]);
        let moved = plan_moving(SyncMode::Sync, &base, &left, &right, Moves::Rename);
        let entry = |decision| CandidateDecision::Entry(decision);
        assert_eq!(
            decision(&moved, b"a/x"),
            entry(EntryDecision::MovedAway { on: Side::Right })
        );
        assert_eq!(
            decision(&moved, b"b/x"),
            entry(EntryDecision::Move { on: Side::Right })
        );
        assert_eq!(moved.moved_from(&path(b"b/x")), Some(&path(b"a/x")));
        // A copy of content that stays where it was is a copy.
        assert_eq!(
            decision(&moved, b"b/z"),
            entry(EntryDecision::Propagate {
                from: Side::Left,
                change: ChangeKind::Create,
            })
        );
        assert_eq!(moved.deletions(Side::Right).deletions(), 0);

        // A folder that takes no renames copies and archives.
        let copied = plan_moving(SyncMode::Sync, &base, &left, &right, Moves::Copy);
        assert_eq!(
            decision(&copied, b"a/x"),
            entry(EntryDecision::Archive { on: Side::Right })
        );
        assert_eq!(copied.moved_from(&path(b"b/x")), None);
        // A Backup never removes, so it never moves.
        let backup = plan_moving(
            SyncMode::Backup { source: Side::Left },
            &base,
            &left,
            &right,
            Moves::Rename,
        );
        assert_eq!(backup.moved_from(&path(b"b/x")), None);
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

    /// Both folders compare names as `names`.
    fn plan_with_names(
        left: &BTreeMap<SyncPath, EntryVersion>,
        right: &BTreeMap<SyncPath, EntryVersion>,
        names: NameMatching,
    ) -> Vec<(SyncPath, CandidateDecision)> {
        plan_between(&BTreeMap::new(), left, right, [names, names])
    }

    fn plan_between(
        base: &BTreeMap<SyncPath, BaselineEntry>,
        left: &BTreeMap<SyncPath, EntryVersion>,
        right: &BTreeMap<SyncPath, EntryVersion>,
        [left_names, right_names]: [NameMatching; 2],
    ) -> Vec<(SyncPath, CandidateDecision)> {
        let choices = Choices {
            resolutions: &BTreeMap::new(),
            skips: &BTreeMap::new(),
            left_names,
            right_names,
            left_moves: Moves::Copy,
            right_moves: Moves::Copy,
        };
        plan_resolved(SyncMode::Sync, base, left, right, &choices, &mut || {
            ScanControl::Continue
        })
        .unwrap()
        .rows()
        .iter()
        .map(|row| (row.path().clone(), row.decision().clone()))
        .collect()
    }

    const CLASH: CandidateDecision =
        CandidateDecision::Entry(EntryDecision::Conflict(ConflictKind::NameClash));

    #[test]
    fn names_one_folder_stores_as_one_file_are_a_conflict_not_two_copies() {
        let insensitive = NameMatching::new(CaseMatching::Insensitive, UnicodeMatching::Normalized);
        let left = tree(&[(b"Report.txt", file(1)), (b"cafe\xcc\x81", file(2))]);
        let right = tree(&[(b"report.txt", file(3)), (b"caf\xc3\xa9", file(4))]);

        let rows = plan_with_names(&left, &right, insensitive);
        assert!(
            rows.iter().all(|(_, decision)| *decision == CLASH),
            "{rows:?}"
        );
        assert_eq!(rows.len(), 4);

        // Byte-exact folders keep all four as separate files.
        let rows = plan_with_names(&left, &right, NameMatching::EXACT);
        assert!(rows.iter().all(|(_, decision)| *decision != CLASH));
    }

    #[test]
    fn a_clashing_folder_blocks_what_is_inside_it() {
        let insensitive = NameMatching::new(CaseMatching::Insensitive, UnicodeMatching::Exact);
        let left = tree(&[(b"Photos", directory()), (b"Photos/a", file(1))]);
        let right = tree(&[(b"photos", directory()), (b"photos/b", file(2))]);

        let rows = plan_with_names(&left, &right, insensitive);
        let decision = |name: &[u8]| {
            rows.iter()
                .find(|(path, _)| path.as_bytes() == name)
                .unwrap()
                .1
                .clone()
        };
        assert_eq!(decision(b"Photos"), CLASH);
        assert_eq!(decision(b"photos"), CLASH);
        assert_eq!(
            decision(b"Photos/a"),
            CandidateDecision::BlockedByAncestor(path(b"Photos"))
        );
        assert_eq!(
            decision(b"photos/b"),
            CandidateDecision::BlockedByAncestor(path(b"photos"))
        );
    }

    /// Review: a case-only rename on a Mac wedged its pair with a server
    /// that keeps both spellings apart, though that server can take the
    /// rename as an archive and a copy.
    #[test]
    fn a_case_only_rename_syncs_where_the_target_keeps_spellings_apart() {
        let insensitive = NameMatching::new(CaseMatching::Insensitive, UnicodeMatching::Normalized);
        let base = BTreeMap::from([(
            path(b"Report.txt"),
            BaselineEntry::new(file(1), file(1)).unwrap(),
        )]);
        let left = tree(&[(b"report.txt", file(1))]);
        let right = tree(&[(b"Report.txt", file(1))]);
        let rows = plan_between(&base, &left, &right, [insensitive, NameMatching::EXACT]);
        assert_eq!(
            rows,
            [
                (
                    path(b"Report.txt"),
                    CandidateDecision::Entry(EntryDecision::Archive { on: Side::Right })
                ),
                (
                    path(b"report.txt"),
                    CandidateDecision::Entry(EntryDecision::Propagate {
                        from: Side::Left,
                        change: ChangeKind::Create
                    })
                ),
            ]
        );

        // Two Macs: the copy would land on the file being archived.
        let rows = plan_between(&base, &left, &right, [insensitive, insensitive]);
        assert!(
            rows.iter().all(|(_, decision)| *decision == CLASH),
            "{rows:?}"
        );

        // Renamed on the server instead: the Mac would be written.
        let rows = plan_between(&base, &left, &right, [NameMatching::EXACT, insensitive]);
        assert!(
            rows.iter().all(|(_, decision)| *decision == CLASH),
            "{rows:?}"
        );
    }

    /// Review: a spelling deleted on a Mac and a new one created on a
    /// byte-exact server never meet on either side, so they sync.
    #[test]
    fn a_write_meets_no_clash_where_the_other_spelling_is_gone() {
        let insensitive = NameMatching::new(CaseMatching::Insensitive, UnicodeMatching::Normalized);
        let base = BTreeMap::from([(
            path(b"Report.txt"),
            BaselineEntry::new(file(1), file(1)).unwrap(),
        )]);
        let left = tree(&[]);
        let right = tree(&[(b"Report.txt", file(1)), (b"report.txt", file(2))]);
        let rows = plan_between(&base, &left, &right, [insensitive, NameMatching::EXACT]);
        assert_eq!(
            rows,
            [
                (
                    path(b"Report.txt"),
                    CandidateDecision::Entry(EntryDecision::Archive { on: Side::Right })
                ),
                (
                    path(b"report.txt"),
                    CandidateDecision::Entry(EntryDecision::Propagate {
                        from: Side::Right,
                        change: ChangeKind::Create
                    })
                ),
            ]
        );
    }

    /// Simulation: a byte-exact folder held both spellings of `café`; the
    /// decomposed one was edited there and deleted on an HFS+ disk, a
    /// conflict, and the composed one was copied onto the disk, which
    /// stored it over the conflicted name. The next run archived a file
    /// and found both sides changed.
    #[test]
    fn a_spelling_in_conflict_holds_its_name_against_another() {
        let hfs = NameMatching::new(CaseMatching::Insensitive, UnicodeMatching::Normalized);
        let base = BTreeMap::from([(
            path(b"cafe\xcc\x81"),
            BaselineEntry::new(file(1), file(1)).unwrap(),
        )]);
        let left = tree(&[(b"cafe\xcc\x81", file(2)), (b"caf\xc3\xa9", file(3))]);
        let right = tree(&[]);
        let rows = plan_between(&base, &left, &right, [NameMatching::EXACT, hfs]);
        assert!(
            rows.iter().all(|(_, decision)| *decision == CLASH),
            "{rows:?}"
        );
    }

    #[test]
    fn case_only_matching_keeps_unicode_spellings_apart() {
        let case_only = NameMatching::new(CaseMatching::Insensitive, UnicodeMatching::Exact);
        let left = tree(&[(b"cafe\xcc\x81", file(1))]);
        let right = tree(&[(b"caf\xc3\xa9", file(2))]);
        let rows = plan_with_names(&left, &right, case_only);
        assert!(rows.iter().all(|(_, decision)| *decision != CLASH));
    }
}
