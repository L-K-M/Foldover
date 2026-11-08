use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, LazyLock};

use foldover_endpoint::{Observation, ObservedEntry};
use unicode_normalization::{UnicodeNormalization, is_nfc};

use foldover_types::{
    BaselineEntry, CaseMatching, ChangeKind, ConflictKind, EntryContent, EntryDecision, EntryKind,
    EntryVersion, NameMatching, PathMap, PathPrefixIndex, Resolution, ScanControl, Side, Skip,
    SyncMode, SyncPath, UnicodeMatching,
};

use crate::reconcile::resolve;
use crate::{DeletionCounts, DeletionReview, InvalidDeletionCounts, RunContext, reconcile_entry};

/// The planner reads ordered rows and exact paths. It accepts either public
/// tree inputs or dense observations without cloning their payloads.
pub(crate) trait Tree {
    type Value;
    fn iter(&self) -> impl Iterator<Item = (&SyncPath, &Self::Value)>;
    fn get(&self, path: &SyncPath) -> Option<&Self::Value>;
    fn len(&self) -> usize;
    fn keys(&self) -> impl Iterator<Item = &SyncPath> {
        self.iter().map(|(path, _)| path)
    }
    fn contains_key(&self, path: &SyncPath) -> bool {
        self.get(path).is_some()
    }
}

impl<T> Tree for BTreeMap<SyncPath, T> {
    type Value = T;
    fn iter(&self) -> impl Iterator<Item = (&SyncPath, &T)> {
        BTreeMap::iter(self)
    }
    fn get(&self, path: &SyncPath) -> Option<&T> {
        BTreeMap::get(self, path)
    }
    fn len(&self) -> usize {
        BTreeMap::len(self)
    }
}

impl<T> Tree for PathMap<T> {
    type Value = T;
    fn iter(&self) -> impl Iterator<Item = (&SyncPath, &T)> {
        PathMap::iter(self)
    }
    fn get(&self, path: &SyncPath) -> Option<&T> {
        PathMap::get(self, path)
    }
    fn len(&self) -> usize {
        PathMap::len(self)
    }
}

impl<T: Tree> Tree for std::sync::Arc<T> {
    type Value = T::Value;
    fn iter(&self) -> impl Iterator<Item = (&SyncPath, &Self::Value)> {
        self.as_ref().iter()
    }
    fn get(&self, path: &SyncPath) -> Option<&Self::Value> {
        self.as_ref().get(path)
    }
    fn len(&self) -> usize {
        self.as_ref().len()
    }
}

/// Planning excludes skipped rows without copying or changing the immutable
/// baseline that verified observations may share.
pub(crate) struct FilteredBase<'a> {
    base: &'a PathMap<BaselineEntry>,
    excluded: &'a [SyncPath],
}

impl<'a> FilteredBase<'a> {
    pub(crate) fn new(base: &'a PathMap<BaselineEntry>, excluded: &'a [SyncPath]) -> Self {
        debug_assert!(excluded.windows(2).all(|pair| pair[0] < pair[1]));
        Self { base, excluded }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.iter().next().is_none()
    }
}

impl Tree for FilteredBase<'_> {
    type Value = BaselineEntry;
    fn iter(&self) -> impl Iterator<Item = (&SyncPath, &BaselineEntry)> {
        self.base
            .iter()
            .filter(|(path, _)| self.excluded.binary_search(path).is_err())
    }
    fn get(&self, path: &SyncPath) -> Option<&BaselineEntry> {
        self.excluded
            .binary_search(path)
            .is_err()
            .then(|| self.base.get(path))
            .flatten()
    }
    fn len(&self) -> usize {
        self.iter().count()
    }
}

/// A borrowed value projection. Planning owns its output rows, but needs no
/// second tree of cloned paths just to read observation versions.
trait Versioned {
    fn version(&self) -> EntryVersion;
}

impl Versioned for EntryVersion {
    fn version(&self) -> EntryVersion {
        *self
    }
}

impl Versioned for ObservedEntry {
    fn version(&self) -> EntryVersion {
        ObservedEntry::version(self)
    }
}

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
    /// One canonical `Arc` per distinct decision: the finite `EntryDecision`
    /// set shares immutable singletons, and blocked rows share the Arc of
    /// their outermost blocking ancestor within the plan.
    decision: Arc<CandidateDecision>,
}

impl PlanRow {
    /// Construct a row with a canonical decision reference. Entry decisions
    /// share the immutable singleton table; a dynamic blocked path gets a
    /// fresh `Arc` here, and plan-local sharing is applied separately.
    fn new(path: SyncPath, decision: CandidateDecision) -> Self {
        Self {
            path,
            decision: canonical_decision(decision),
        }
    }

    /// Replace the decision with a canonical reference.
    fn set_decision(&mut self, decision: CandidateDecision) {
        self.decision = canonical_decision(decision);
    }

    /// Replace the decision with an already-canonical shared reference.
    fn set_shared_decision(&mut self, decision: Arc<CandidateDecision>) {
        self.decision = decision;
    }

    pub fn path(&self) -> &SyncPath {
        &self.path
    }

    pub fn decision(&self) -> &CandidateDecision {
        self.decision.as_ref()
    }
}

/// The 21 finite `EntryDecision` values, each with one immutable `Arc` that
/// every row carrying it shares. `EntryDecision` is `Copy` and has no heap
/// payload, so an exhaustive match selects the singleton without a lookup
/// key or a growing cache.
static ENTRY_DECISIONS: LazyLock<[Arc<CandidateDecision>; 21]> = LazyLock::new(|| {
    use ChangeKind::{Create, ReplaceContent, UpdateMetadata};
    use ConflictKind::{BothChanged, DeletedVersusEdited, MetadataOnly, NameClash, TypeMismatch};
    use EntryDecision::{
        Archive, Conflict, ForgetBase, KeepTarget, Move as MoveDecision, MovedAway, Propagate,
        RecordBase, Unchanged,
    };
    use Side::{Left, Right};
    [
        Arc::new(CandidateDecision::Entry(Unchanged)),
        Arc::new(CandidateDecision::Entry(RecordBase)),
        Arc::new(CandidateDecision::Entry(ForgetBase)),
        Arc::new(CandidateDecision::Entry(KeepTarget)),
        Arc::new(CandidateDecision::Entry(Propagate {
            from: Left,
            change: Create,
        })),
        Arc::new(CandidateDecision::Entry(Propagate {
            from: Left,
            change: ReplaceContent,
        })),
        Arc::new(CandidateDecision::Entry(Propagate {
            from: Left,
            change: UpdateMetadata,
        })),
        Arc::new(CandidateDecision::Entry(Propagate {
            from: Right,
            change: Create,
        })),
        Arc::new(CandidateDecision::Entry(Propagate {
            from: Right,
            change: ReplaceContent,
        })),
        Arc::new(CandidateDecision::Entry(Propagate {
            from: Right,
            change: UpdateMetadata,
        })),
        Arc::new(CandidateDecision::Entry(Archive { on: Left })),
        Arc::new(CandidateDecision::Entry(Archive { on: Right })),
        Arc::new(CandidateDecision::Entry(MoveDecision { on: Left })),
        Arc::new(CandidateDecision::Entry(MoveDecision { on: Right })),
        Arc::new(CandidateDecision::Entry(MovedAway { on: Left })),
        Arc::new(CandidateDecision::Entry(MovedAway { on: Right })),
        Arc::new(CandidateDecision::Entry(Conflict(BothChanged))),
        Arc::new(CandidateDecision::Entry(Conflict(DeletedVersusEdited))),
        Arc::new(CandidateDecision::Entry(Conflict(TypeMismatch))),
        Arc::new(CandidateDecision::Entry(Conflict(MetadataOnly))),
        Arc::new(CandidateDecision::Entry(Conflict(NameClash))),
    ]
});

/// The singleton index for one of the finite `EntryDecision` values.
fn entry_decision_index(decision: EntryDecision) -> usize {
    use ChangeKind::{Create, ReplaceContent, UpdateMetadata};
    use ConflictKind::{BothChanged, DeletedVersusEdited, MetadataOnly, NameClash, TypeMismatch};
    use EntryDecision::{
        Archive, Conflict, ForgetBase, KeepTarget, Move as MoveDecision, MovedAway, Propagate,
        RecordBase, Unchanged,
    };
    use Side::{Left, Right};
    match decision {
        Unchanged => 0,
        RecordBase => 1,
        ForgetBase => 2,
        KeepTarget => 3,
        Propagate {
            from: Left,
            change: Create,
        } => 4,
        Propagate {
            from: Left,
            change: ReplaceContent,
        } => 5,
        Propagate {
            from: Left,
            change: UpdateMetadata,
        } => 6,
        Propagate {
            from: Right,
            change: Create,
        } => 7,
        Propagate {
            from: Right,
            change: ReplaceContent,
        } => 8,
        Propagate {
            from: Right,
            change: UpdateMetadata,
        } => 9,
        Archive { on: Left } => 10,
        Archive { on: Right } => 11,
        MoveDecision { on: Left } => 12,
        MoveDecision { on: Right } => 13,
        MovedAway { on: Left } => 14,
        MovedAway { on: Right } => 15,
        Conflict(BothChanged) => 16,
        Conflict(DeletedVersusEdited) => 17,
        Conflict(TypeMismatch) => 18,
        Conflict(MetadataOnly) => 19,
        Conflict(NameClash) => 20,
    }
}

/// The one immutable `Arc` shared by every row carrying `decision`.
fn entry_decision(decision: EntryDecision) -> Arc<CandidateDecision> {
    ENTRY_DECISIONS[entry_decision_index(decision)].clone()
}

/// The canonical `Arc` for a decision: the immutable singleton for a finite
/// `EntryDecision`, or a fresh `Arc` for a dynamic blocked path. Plan-local
/// blocked decisions are shared separately, so this only runs where no
/// sharing is required.
fn canonical_decision(decision: CandidateDecision) -> Arc<CandidateDecision> {
    match decision {
        CandidateDecision::Entry(entry) => entry_decision(entry),
        CandidateDecision::BlockedByAncestor(_) => Arc::new(decision),
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

    /// Unavailable paths were removed from both observations and the planning
    /// base. Their surviving ancestors must not remove them indirectly. Keep
    /// destructive ancestor branches untouched, including their baseline rows,
    /// while safe creates and metadata writes on other ancestors can proceed.
    /// File/folder replacements are already type conflicts; directory versions
    /// have no payload to replace, so only archives can displace a subtree.
    pub(crate) fn protect_unavailable(
        &mut self,
        roots: &[SyncPath],
        names: NameMatching,
    ) -> Result<(), PlanError> {
        if roots.is_empty() {
            return Ok(());
        }
        let mut ancestors = BTreeSet::new();
        for root in roots {
            let key = name_key(root.as_bytes(), names);
            for (index, &byte) in key.iter().enumerate() {
                if byte == b'/' {
                    ancestors.insert(key[..index].to_vec());
                }
            }
            ancestors.insert(key.into_owned());
        }
        let blocked: BTreeSet<_> = self
            .rows
            .iter()
            .filter(|row| {
                matches!(
                    row.decision.as_ref(),
                    CandidateDecision::Entry(EntryDecision::Archive { .. })
                )
            })
            .map(|row| name_key(row.path.as_bytes(), names).into_owned())
            .filter(|key| ancestors.contains(key))
            .collect();
        if blocked.is_empty() {
            return Ok(());
        }
        let inside = |path: &SyncPath| {
            let key = name_key(path.as_bytes(), names);
            blocked.contains(key.as_ref())
                || key
                    .iter()
                    .enumerate()
                    .any(|(index, &byte)| byte == b'/' && blocked.contains(&key[..index]))
        };
        // A move's source row carries no operation; its destination commits
        // both baseline changes. Disarm both ends when either is protected.
        let mut disarmed = BTreeSet::new();
        self.moves.retain(|to, from| {
            if inside(to) || inside(from) {
                disarmed.insert(to.clone());
                disarmed.insert(from.clone());
                false
            } else {
                true
            }
        });
        for row in &mut self.rows {
            if inside(&row.path) || disarmed.contains(&row.path) {
                row.set_decision(CandidateDecision::Entry(EntryDecision::KeepTarget));
            }
        }
        self.recount_deletions()
    }

    /// An incomplete listing cannot prove a deletion, conflict with deletion,
    /// or rename. Creates remain safe: their write checks require absence and
    /// report an occupied destination without replacing it.
    pub(crate) fn protect_uncertain_absences(
        &mut self,
        paths: &[SyncPath],
        names: NameMatching,
    ) -> Result<(), PlanError> {
        if paths.is_empty() {
            return Ok(());
        }
        let paths: BTreeSet<_> = paths
            .iter()
            .map(|path| name_key(path.as_bytes(), names))
            .collect();
        let uncertain = |path: &SyncPath| paths.contains(&name_key(path.as_bytes(), names));
        let mut disarmed = BTreeSet::new();
        self.moves.retain(|to, from| {
            if uncertain(to) || uncertain(from) {
                disarmed.insert(to.clone());
                disarmed.insert(from.clone());
                false
            } else {
                true
            }
        });
        for row in &mut self.rows {
            let safe_create = matches!(
                row.decision.as_ref(),
                CandidateDecision::Entry(EntryDecision::Propagate {
                    change: ChangeKind::Create,
                    ..
                })
            );
            if disarmed.contains(&row.path) {
                row.set_decision(match row.decision.as_ref() {
                    CandidateDecision::Entry(EntryDecision::Move { on }) => {
                        CandidateDecision::Entry(EntryDecision::Propagate {
                            from: on.other(),
                            change: ChangeKind::Create,
                        })
                    }
                    _ => CandidateDecision::Entry(EntryDecision::KeepTarget),
                });
            } else if uncertain(&row.path) && !safe_create {
                row.set_decision(CandidateDecision::Entry(EntryDecision::KeepTarget));
            }
        }
        self.recount_deletions()
    }

    fn recount_deletions(&mut self) -> Result<(), PlanError> {
        let deletion_counts = |target: Side| -> Result<DeletionCounts, PlanError> {
            let deletions = self
                .rows
                .iter()
                .filter(|row| {
                    matches!(
                        row.decision.as_ref(),
                        CandidateDecision::Entry(EntryDecision::Archive { on }) if *on == target
                    )
                })
                .count();
            Ok(DeletionCounts::new(
                self.deletions(target).paths(),
                count_paths(deletions)?,
            )?)
        };
        let left_deletions = deletion_counts(Side::Left)?;
        let right_deletions = deletion_counts(Side::Right)?;
        self.left_deletions = left_deletions;
        self.right_deletions = right_deletions;
        Ok(())
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
fn settle_folder_choices<B: Tree<Value = BaselineEntry>, T: Tree>(
    rows: &mut [PlanRow],
    base: &B,
    left: &T,
    right: &T,
    resolutions: &BTreeMap<SyncPath, Resolution>,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<(), PlanError>
where
    T::Value: Versioned,
{
    let mut branches: BTreeMap<SyncPath, Side> = BTreeMap::new();
    for row in rows.iter_mut() {
        check_control(control)?;
        let (left, right) = (
            left.get(&row.path).map(Versioned::version),
            right.get(&row.path).map(Versioned::version),
        );
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
        row.set_decision(CandidateDecision::Entry(reconcile_entry(
            SyncMode::Mirror { source: keep },
            base.get(&row.path).copied(),
            left,
            right,
        )));
    }
    Ok(())
}

/// Leave each skipped change and everything inside it as it is: no write,
/// no base change. A skip applies while both sides still hold what the
/// person saw at the skipped path.
fn skip_branches<T: Tree>(
    rows: &mut [PlanRow],
    left: &T,
    right: &T,
    skips: &BTreeMap<SyncPath, Skip>,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<(), PlanError>
where
    T::Value: Versioned,
{
    if skips.is_empty() {
        return Ok(());
    }
    let mut skipped = PathPrefixIndex::new();
    for row in rows.iter_mut() {
        check_control(control)?;
        let inside = skipped.contains_ancestor(&row.path);
        let current = skips.get(&row.path).is_some_and(|skip| {
            (
                left.get(&row.path).map(Versioned::version),
                right.get(&row.path).map(Versioned::version),
            ) == (skip.left(), skip.right())
        });
        if current {
            skipped.insert(&row.path);
        }
        if inside || current {
            row.set_decision(CandidateDecision::Entry(EntryDecision::KeepTarget));
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
fn mark_name_clashes<T: Tree>(
    rows: &mut [PlanRow],
    [left, right]: [&T; 2],
    left_names: NameMatching,
    right_names: NameMatching,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<(), PlanError>
where
    T::Value: Versioned,
{
    let loose = left_names.either(right_names);
    if loose == NameMatching::EXACT {
        return Ok(());
    }
    // Raw rows are already sorted. When normalization leaves their keys
    // strictly increasing, there cannot be a clash and no index is needed.
    let mut previous: Option<Cow<'_, [u8]>> = None;
    let mut ordered_unique = true;
    for row in rows.iter() {
        check_control(control)?;
        let key = name_key(row.path.as_bytes(), loose);
        if previous
            .as_deref()
            .is_some_and(|known| known >= key.as_ref())
        {
            ordered_unique = false;
            break;
        }
        previous = Some(key);
    }
    if ordered_unique {
        return Ok(());
    }
    drop(previous);
    // A flat vector avoids a hash bucket and singleton ordinal allocation
    // per path. Only groups that actually collide need the per-side index.
    let mut keys = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        check_control(control)?;
        keys.push((name_key(row.path.as_bytes(), loose), index));
    }
    keys.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    check_control(control)?;
    let mut clashes = Vec::new();
    for group in keys
        .chunk_by(|left, right| left.0 == right.0)
        .filter(|group| group.len() > 1)
    {
        for (side, names, present) in [
            (Side::Left, left_names, left),
            (Side::Right, right_names, right),
        ] {
            // Spellings this folder stores as one file.
            let mut same: HashMap<Cow<'_, [u8]>, Vec<usize>> = HashMap::new();
            for &(_, index) in group {
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
                    .partition(|&&index| writes_on(rows[index].decision.as_ref(), side));
                let stays = untouched.iter().any(|&index| {
                    present.contains_key(&rows[index].path)
                        || matches!(
                            rows[index].decision.as_ref(),
                            CandidateDecision::Entry(EntryDecision::Conflict(_))
                        )
                });
                if writes.len() > 1 || (!writes.is_empty() && stays) {
                    clashes.extend(spellings);
                }
            }
        }
    }
    drop(keys);
    for index in clashes {
        rows[index].set_decision(CandidateDecision::Entry(EntryDecision::Conflict(
            ConflictKind::NameClash,
        )));
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
///
/// The key borrows the path bytes whenever the comparison leaves them
/// unchanged: exact matching, ASCII under normalization (NFC is the
/// identity), text already in lower case, and invalid UTF-8 with no ASCII
/// upper case to fold. Only an actual recomposition or case change
/// allocates, so the common ASCII path pays nothing.
pub(crate) fn name_key(path: &[u8], names: NameMatching) -> Cow<'_, [u8]> {
    let insensitive = names.case() == CaseMatching::Insensitive;
    let Ok(text) = std::str::from_utf8(path) else {
        return if insensitive && path.iter().any(u8::is_ascii_uppercase) {
            Cow::Owned(path.to_ascii_lowercase())
        } else {
            Cow::Borrowed(path)
        };
    };
    let composed = match names.unicode() {
        UnicodeMatching::Exact => None,
        // NFC is the identity for ASCII and for text already composed.
        UnicodeMatching::Normalized if text.is_ascii() || is_nfc(text) => None,
        UnicodeMatching::Normalized => Some(text.nfc().collect::<String>()),
    };
    let text = composed.as_deref().unwrap_or(text);
    if insensitive && !already_lowercase(text) {
        Cow::Owned(text.to_lowercase().into_bytes())
    } else if let Some(composed) = composed {
        Cow::Owned(composed.into_bytes())
    } else {
        Cow::Borrowed(path)
    }
}

/// Whether Unicode lower-casing leaves `text` exactly as it is, character
/// for character. `str::to_lowercase` maps each character independently, so
/// this decides whether the owned allocation is needed at all.
fn already_lowercase(text: &str) -> bool {
    text.chars()
        .all(|character| character.to_lowercase().eq([character]))
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
    plan_trees(mode, base, left, right, choices, control)
}

pub(crate) fn plan_observations(
    mode: SyncMode,
    base: &FilteredBase<'_>,
    left: &Observation,
    right: &Observation,
    choices: &Choices<'_>,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<CandidatePlan, PlanError> {
    plan_trees(
        mode,
        base,
        left.entries(),
        right.entries(),
        choices,
        control,
    )
}

fn plan_trees<B: Tree<Value = BaselineEntry>, T: Tree>(
    mode: SyncMode,
    base: &B,
    left: &T,
    right: &T,
    choices: &Choices<'_>,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<CandidatePlan, PlanError>
where
    T::Value: Versioned,
{
    let Choices {
        resolutions,
        skips,
        left_names,
        right_names,
        left_moves,
        right_moves,
    } = *choices;
    validate_tree(base, TreeRole::Base, |entry| entry.left().kind(), control)?;
    validate_tree(
        left,
        TreeRole::Left,
        |entry| entry.version().kind(),
        control,
    )?;
    validate_tree(
        right,
        TreeRole::Right,
        |entry| entry.version().kind(),
        control,
    )?;

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
            .map(|(_, entry)| entry.version());
        let right = right_entries
            .next_if(|(key, _)| *key == &path)
            .map(|(_, entry)| entry.version());
        if [left, right, baseline.map(BaselineEntry::left)]
            .into_iter()
            .flatten()
            .any(|entry| entry.kind() == EntryKind::Directory)
        {
            directories.insert(path.clone(), rows.len());
        }
        rows.push(PlanRow::new(
            path.clone(),
            CandidateDecision::Entry(decide(
                reconcile_entry(mode, baseline, left, right),
                resolutions.get(&path),
                left,
                right,
            )),
        ));

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
                    && rows[index].decision.as_ref()
                        == &CandidateDecision::Entry(EntryDecision::Archive { on: side })
                {
                    rows[index].set_decision(CandidateDecision::Entry(EntryDecision::Conflict(
                        ConflictKind::DeletedVersusEdited,
                    )));
                }
                parent = path.parent();
            }
        }
    }

    settle_folder_choices(&mut rows, base, left, right, resolutions, control)?;
    skip_branches(&mut rows, left, right, skips, control)?;
    mark_name_clashes(&mut rows, [left, right], left_names, right_names, control)?;

    // Ancestors precede descendants. Suppression therefore propagates an outer
    // conflict through the whole branch without recursive traversal. One
    // `Arc` per outermost blocking ancestor is shared by every row it covers;
    // the table is local to this plan, so no path-keyed global cache grows.
    let mut blocked_decisions: BTreeMap<SyncPath, Arc<CandidateDecision>> = BTreeMap::new();
    for index in 0..rows.len() {
        check_control(control)?;
        let mut parent = rows[index].path.parent();
        let mut blocked = None;
        while let Some(path) = parent {
            check_control(control)?;
            if let Some(&ancestor) = directories.get(&path) {
                match rows[ancestor].decision.as_ref() {
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
            let decision = blocked_decisions
                .entry(ancestor.clone())
                .or_insert_with(|| Arc::new(CandidateDecision::BlockedByAncestor(ancestor)))
                .clone();
            rows[index].set_shared_decision(decision);
        }
    }
    let moves = pair_moves(&mut rows, [left, right], [left_moves, right_moves]);

    let deletion_counts = |target: Side, paths: usize| -> Result<DeletionCounts, PlanError> {
        let deletions = rows
            .iter()
            .filter(|row| {
                matches!(
                    row.decision.as_ref(),
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
fn pair_moves<T: Tree>(
    rows: &mut [PlanRow],
    trees: [&T; 2],
    moves: [Moves; 2],
) -> BTreeMap<SyncPath, SyncPath>
where
    T::Value: Versioned,
{
    let takes = |side: Side| moves[side as usize] == Moves::Rename;
    let movable = |side: Side, path: &SyncPath| {
        trees[side as usize]
            .get(path)
            .map(|version| version.version().content())
            .filter(|content| matches!(content, EntryContent::File(_) | EntryContent::Symlink(_)))
    };
    #[derive(Clone, Copy, PartialEq)]
    enum Role {
        Archive,
        Create,
    }
    let candidate = |row: &PlanRow| match row.decision.as_ref() {
        CandidateDecision::Entry(EntryDecision::Archive { on }) => Some((*on, Role::Archive, *on)),
        CandidateDecision::Entry(EntryDecision::Propagate {
            from,
            change: ChangeKind::Create,
        }) => Some((from.other(), Role::Create, *from)),
        _ => None,
    };
    let mut archives = [0usize; 2];
    let mut creates = [0usize; 2];
    for row in rows.iter() {
        match candidate(row) {
            Some((on, Role::Archive, _)) if takes(on) => archives[on as usize] += 1,
            Some((on, Role::Create, _)) if takes(on) => creates[on as usize] += 1,
            _ => {}
        }
    }
    let mut paired = BTreeMap::new();
    for on in [Side::Left, Side::Right] {
        if archives[on as usize] == 0 || creates[on as usize] == 0 {
            continue;
        }
        // A move needs both roles. Index only the smaller role and consume
        // its paths in order while scanning the other, preserving the old
        // per-content zip without indexing every unmatched create/archive.
        let indexed = if archives[on as usize] <= creates[on as usize] {
            Role::Archive
        } else {
            Role::Create
        };
        let mut waiting: HashMap<EntryContent, VecDeque<usize>> = HashMap::new();
        for (index, row) in rows.iter().enumerate() {
            if let Some((target, role, source)) = candidate(row)
                && target == on
                && role == indexed
                && let Some(content) = movable(source, &row.path)
            {
                waiting.entry(content).or_default().push_back(index);
            }
        }
        if waiting.is_empty() {
            continue;
        }
        let mut matches = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            if let Some((target, role, source)) = candidate(row)
                && target == on
                && role != indexed
                && let Some(content) = movable(source, &row.path)
                && let Some(other) = waiting.get_mut(&content).and_then(VecDeque::pop_front)
            {
                matches.push(match indexed {
                    Role::Archive => (other, index),
                    Role::Create => (index, other),
                });
            }
        }
        for (from, to) in matches {
            rows[from].set_decision(CandidateDecision::Entry(EntryDecision::MovedAway { on }));
            rows[to].set_decision(CandidateDecision::Entry(EntryDecision::Move { on }));
            paired.insert(rows[to].path.clone(), rows[from].path.clone());
        }
    }
    paired
}

fn validate_tree<T: Tree>(
    entries: &T,
    tree: TreeRole,
    kind: impl Fn(&T::Value) -> EntryKind,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<(), PlanError> {
    for (path, _) in entries.iter() {
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
            .as_ref()
            .clone()
    }

    #[test]
    fn uncertain_missing_files_preserve_their_base_and_independent_creates() {
        let base = baseline(&tree(&[(b"missing", file(1)), (b"forgotten", file(2))]));
        let left = tree(&[(b"healthy", file(3))]);
        let right = tree(&[(b"missing", file(1))]);
        let mut candidate = plan(SyncMode::Sync, &base, &left, &right);
        assert_eq!(
            decision(&candidate, b"missing"),
            CandidateDecision::Entry(EntryDecision::Archive { on: Side::Right })
        );
        assert_eq!(
            decision(&candidate, b"forgotten"),
            CandidateDecision::Entry(EntryDecision::ForgetBase)
        );
        candidate
            .protect_uncertain_absences(
                &[path(b"missing"), path(b"forgotten")],
                NameMatching::EXACT,
            )
            .unwrap();
        for name in [b"missing".as_slice(), b"forgotten"] {
            assert_eq!(
                decision(&candidate, name),
                CandidateDecision::Entry(EntryDecision::KeepTarget)
            );
        }
        assert_eq!(
            decision(&candidate, b"healthy"),
            CandidateDecision::Entry(EntryDecision::Propagate {
                from: Side::Left,
                change: ChangeKind::Create,
            })
        );
        assert_eq!(candidate.deletions(Side::Right).paths(), 1);
        assert_eq!(candidate.deletions(Side::Right).deletions(), 0);
    }

    #[test]
    fn uncertain_target_absence_keeps_mirror_and_backup_creates() {
        let source = tree(&[(b"missing", file(1))]);
        let base = baseline(&source);
        for mode in [
            SyncMode::Mirror { source: Side::Left },
            SyncMode::Backup { source: Side::Left },
        ] {
            let mut candidate = plan(mode, &base, &source, &BTreeMap::new());
            candidate
                .protect_uncertain_absences(&[path(b"missing")], NameMatching::EXACT)
                .unwrap();
            assert_eq!(
                decision(&candidate, b"missing"),
                CandidateDecision::Entry(EntryDecision::Propagate {
                    from: Side::Left,
                    change: ChangeKind::Create,
                })
            );
        }
    }

    #[test]
    fn uncertain_moves_keep_the_old_file_and_copy_to_the_new_path() {
        let left = tree(&[(b"new", file(1)), (b"renamed", file(2))]);
        let right = tree(&[(b"old", file(1)), (b"original", file(2))]);
        let base = baseline(&right);
        for uncertain in [b"old".as_slice(), b"new"] {
            let mut candidate = plan_moving(SyncMode::Sync, &base, &left, &right, Moves::Rename);
            assert_eq!(candidate.moved_from(&path(b"new")), Some(&path(b"old")));
            candidate
                .protect_uncertain_absences(&[path(uncertain)], NameMatching::EXACT)
                .unwrap();
            assert_eq!(
                decision(&candidate, b"old"),
                CandidateDecision::Entry(EntryDecision::KeepTarget)
            );
            assert_eq!(
                decision(&candidate, b"new"),
                CandidateDecision::Entry(EntryDecision::Propagate {
                    from: Side::Left,
                    change: ChangeKind::Create,
                })
            );
            assert_eq!(candidate.moved_from(&path(b"new")), None);
            assert_eq!(candidate.moves.len(), 1);
            assert_eq!(
                candidate.moved_from(&path(b"renamed")),
                Some(&path(b"original"))
            );
            assert_eq!(
                decision(&candidate, b"original"),
                CandidateDecision::Entry(EntryDecision::MovedAway { on: Side::Right })
            );
            assert_eq!(
                decision(&candidate, b"renamed"),
                CandidateDecision::Entry(EntryDecision::Move { on: Side::Right })
            );
            assert_eq!(candidate.deletions(Side::Right).paths(), 2);
            assert_eq!(candidate.deletions(Side::Right).deletions(), 0);
        }
    }

    #[test]
    fn uncertain_descendants_do_not_reopen_a_protected_ancestor_branch() {
        let left = tree(&[(b"healthy", file(3))]);
        let right = tree(&[(b"folder", directory()), (b"folder/kept", file(1))]);
        let mut base = baseline(&right);
        base.insert(
            path(b"folder/missing"),
            BaselineEntry::new(file(2), file(2)).unwrap(),
        );
        let mut candidate = plan(
            SyncMode::Mirror { source: Side::Left },
            &base,
            &left,
            &right,
        );
        let uncertain = [path(b"folder/missing")];
        candidate
            .protect_unavailable(&uncertain, NameMatching::EXACT)
            .unwrap();
        candidate
            .protect_uncertain_absences(&uncertain, NameMatching::EXACT)
            .unwrap();
        for name in [b"folder".as_slice(), b"folder/kept", b"folder/missing"] {
            assert_eq!(
                decision(&candidate, name),
                CandidateDecision::Entry(EntryDecision::KeepTarget)
            );
        }
        assert_eq!(
            decision(&candidate, b"healthy"),
            CandidateDecision::Entry(EntryDecision::Propagate {
                from: Side::Left,
                change: ChangeKind::Create,
            })
        );
        assert_eq!(candidate.deletions(Side::Right).deletions(), 0);
    }

    #[test]
    fn unavailable_child_protects_archived_branch_and_preserves_independent_work() {
        let left = tree(&[(b"sibling", file(1))]);
        let right = tree(&[
            (b"folder", directory()),
            (b"folder/readable", file(2)),
            (b"unrelated", file(3)),
        ]);
        let mut base = baseline(&right);
        base.insert(
            path(b"folder/removed"),
            BaselineEntry::new(file(4), file(4)).unwrap(),
        );
        let mut candidate = plan(
            SyncMode::Mirror { source: Side::Left },
            &base,
            &left,
            &right,
        );
        candidate
            .protect_unavailable(&[path(b"folder/unreadable")], NameMatching::EXACT)
            .unwrap();
        for name in [b"folder".as_slice(), b"folder/readable", b"folder/removed"] {
            assert_eq!(
                decision(&candidate, name),
                CandidateDecision::Entry(EntryDecision::KeepTarget)
            );
        }
        assert_eq!(
            decision(&candidate, b"sibling"),
            CandidateDecision::Entry(EntryDecision::Propagate {
                from: Side::Left,
                change: ChangeKind::Create,
            })
        );
        assert_eq!(
            decision(&candidate, b"unrelated"),
            CandidateDecision::Entry(EntryDecision::Archive { on: Side::Right })
        );
        assert_eq!(candidate.deletions(Side::Right).paths(), 3);
        assert_eq!(candidate.deletions(Side::Right).deletions(), 1);
    }

    #[test]
    fn unavailable_children_preserve_existing_type_conflicts() {
        let left = tree(&[(b"folder", file(1))]);
        let right = tree(&[(b"folder", directory())]);
        let mut candidate = plan(
            SyncMode::Mirror { source: Side::Left },
            &BTreeMap::new(),
            &left,
            &right,
        );
        candidate
            .protect_unavailable(&[path(b"folder/unreadable")], NameMatching::EXACT)
            .unwrap();
        assert_eq!(
            decision(&candidate, b"folder"),
            CandidateDecision::Entry(EntryDecision::Conflict(ConflictKind::TypeMismatch))
        );
    }

    #[test]
    fn unavailable_child_preserves_safe_ancestor_creates_and_metadata_changes() {
        let left = tree(&[(b"folder", directory()), (b"folder/readable", file(2))]);
        for right in [
            BTreeMap::new(),
            tree(&[(
                b"folder",
                EntryVersion::new(
                    EntryContent::Directory,
                    Digest::from_bytes([1; Digest::BYTE_LEN]),
                ),
            )]),
        ] {
            let mut candidate = plan(
                SyncMode::Mirror { source: Side::Left },
                &BTreeMap::new(),
                &left,
                &right,
            );
            let expected = candidate.rows.clone();
            candidate
                .protect_unavailable(&[path(b"folder/unreadable")], NameMatching::EXACT)
                .unwrap();
            assert_eq!(candidate.rows, expected);
        }
    }

    #[test]
    fn unavailable_ancestry_uses_case_and_unicode_rules_and_component_boundaries() {
        let right = tree(&[
            (b"CAF\xc3\x89", directory()),
            (b"CAF\xc3\x89/readable", file(1)),
            (b"CAF\xc3\x89-extra", directory()),
            (b"CAF\xc3\x89-extra/readable", file(2)),
        ]);
        let mut candidate = plan(
            SyncMode::Mirror { source: Side::Left },
            &BTreeMap::new(),
            &BTreeMap::new(),
            &right,
        );
        candidate
            .protect_unavailable(
                &[path(b"cafe\xcc\x81/unreadable")],
                NameMatching::new(CaseMatching::Insensitive, UnicodeMatching::Normalized),
            )
            .unwrap();
        for name in [b"CAF\xc3\x89".as_slice(), b"CAF\xc3\x89/readable"] {
            assert_eq!(
                decision(&candidate, name),
                CandidateDecision::Entry(EntryDecision::KeepTarget)
            );
        }
        for name in [
            b"CAF\xc3\x89-extra".as_slice(),
            b"CAF\xc3\x89-extra/readable",
        ] {
            assert_eq!(
                decision(&candidate, name),
                CandidateDecision::Entry(EntryDecision::Archive { on: Side::Right })
            );
        }
    }

    #[test]
    fn unavailable_branch_disarms_both_ends_of_crossing_moves() {
        let left = tree(&[(b"outside", file(1)), (b"renamed", file(2))]);
        let right = tree(&[
            (b"folder", directory()),
            (b"folder/moved", file(1)),
            (b"original", file(2)),
        ]);
        let mut candidate = plan_moving(
            SyncMode::Mirror { source: Side::Left },
            &baseline(&right),
            &left,
            &right,
            Moves::Rename,
        );
        assert_eq!(candidate.moves.len(), 2);
        candidate
            .protect_unavailable(&[path(b"folder/unreadable")], NameMatching::EXACT)
            .unwrap();
        assert_eq!(candidate.moves.len(), 1);
        for name in [b"folder/moved".as_slice(), b"outside"] {
            assert_eq!(
                decision(&candidate, name),
                CandidateDecision::Entry(EntryDecision::KeepTarget)
            );
        }
        assert_eq!(candidate.moved_from(&path(b"outside")), None);
        assert_eq!(
            candidate.moved_from(&path(b"renamed")),
            Some(&path(b"original"))
        );
        assert_eq!(
            decision(&candidate, b"renamed"),
            CandidateDecision::Entry(EntryDecision::Move { on: Side::Right })
        );
        assert_eq!(candidate.deletions(Side::Right).paths(), 3);
        assert_eq!(candidate.deletions(Side::Right).deletions(), 0);
    }

    #[test]
    fn skipped_branches_keep_component_boundaries_and_live_versions() {
        let left = tree(&[
            (b"a", directory()),
            (b"a/x", file(1)),
            (b"a-b", directory()),
            (b"a-b/y", file(2)),
            (b"a.txt", directory()),
            (b"a.txt/z", file(3)),
            (b"a0", directory()),
            (b"a0/w", file(4)),
        ]);
        let skips = BTreeMap::from([
            // Matching versions: `a` covers `a/x` but no shared-prefix sibling.
            (path(b"a"), Skip::new(Some(directory()), None)),
            (path(b"a.txt"), Skip::new(Some(directory()), None)),
            // The versions changed since the person skipped it: no longer current.
            (path(b"a0"), Skip::new(Some(file(9)), None)),
        ]);
        let resolutions = BTreeMap::new();
        let choices = Choices {
            resolutions: &resolutions,
            skips: &skips,
            ..Choices::NONE
        };
        let candidate = plan_resolved(
            SyncMode::Mirror { source: Side::Left },
            &BTreeMap::new(),
            &left,
            &BTreeMap::new(),
            &choices,
            &mut || ScanControl::Continue,
        )
        .unwrap();
        let keep = CandidateDecision::Entry(EntryDecision::KeepTarget);
        let create = CandidateDecision::Entry(EntryDecision::Propagate {
            from: Side::Left,
            change: ChangeKind::Create,
        });
        for name in [b"a".as_slice(), b"a/x", b"a.txt", b"a.txt/z"] {
            assert_eq!(decision(&candidate, name), keep, "{name:?}");
        }
        for name in [b"a-b".as_slice(), b"a-b/y", b"a0", b"a0/w"] {
            assert_eq!(decision(&candidate, name), create, "{name:?}");
        }
        // A skip applies only while both sides still hold its versions: an
        // edited folder no longer matches, so its branch is planned again.
        let edited = EntryVersion::new(
            EntryContent::Directory,
            Digest::from_bytes([9; Digest::BYTE_LEN]),
        );
        let mut lapsed = left.clone();
        lapsed.insert(path(b"a0"), edited);
        let choices = Choices {
            skips: &BTreeMap::from([(path(b"a0"), Skip::new(Some(edited), None))]),
            resolutions: &resolutions,
            ..Choices::NONE
        };
        let candidate = plan_resolved(
            SyncMode::Mirror { source: Side::Left },
            &BTreeMap::new(),
            &lapsed,
            &BTreeMap::new(),
            &choices,
            &mut || ScanControl::Continue,
        )
        .unwrap();
        assert_eq!(decision(&candidate, b"a0"), keep);
        assert_eq!(decision(&candidate, b"a0/w"), keep);
    }

    /// The indexed pass decides exactly what the original linear scan
    /// decided, at the 100k-row, 1k-prefix scale the planner can meet.
    #[test]
    fn skip_branches_match_the_linear_rule_at_scale() {
        const FOLDERS: usize = 1_000;
        const FILES: usize = 99;
        let mut rows = Vec::new();
        let mut left = BTreeMap::new();
        let mut right = BTreeMap::new();
        let mut skips = BTreeMap::new();
        for folder in 0..FOLDERS {
            let name = format!("d-{folder:04}");
            let folder_path = path(name.as_bytes());
            rows.push(PlanRow::new(
                folder_path.clone(),
                CandidateDecision::Entry(EntryDecision::Unchanged),
            ));
            left.insert(folder_path.clone(), directory());
            right.insert(folder_path.clone(), directory());
            skips.insert(folder_path, Skip::new(Some(directory()), Some(directory())));
            for index in 0..FILES {
                let file_name = format!("{name}/f-{index:02}");
                let file_path = path(file_name.as_bytes());
                let version = file((folder * FILES + index) as u8);
                rows.push(PlanRow::new(
                    file_path.clone(),
                    CandidateDecision::Entry(EntryDecision::Unchanged),
                ));
                left.insert(file_path.clone(), version);
                right.insert(file_path, version);
            }
        }
        assert_eq!(rows.len(), 100_000);

        // The original linear rule: every row against every active root.
        let mut expected = rows.clone();
        let mut skipped: Vec<SyncPath> = Vec::new();
        for row in &mut expected {
            let inside = skipped.iter().any(|root| root.is_ancestor_of(&row.path));
            let current = skips.get(&row.path).is_some_and(|skip| {
                (left.get(&row.path).copied(), right.get(&row.path).copied())
                    == (skip.left(), skip.right())
            });
            if current {
                skipped.push(row.path.clone());
            }
            if inside || current {
                row.set_decision(CandidateDecision::Entry(EntryDecision::KeepTarget));
            }
        }

        skip_branches(&mut rows, &left, &right, &skips, &mut || {
            ScanControl::Continue
        })
        .unwrap();
        assert_eq!(rows, expected);
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
            assert!(candidate.rows.iter().all(|row| row.decision.as_ref()
                == &CandidateDecision::Entry(EntryDecision::Archive { on: Side::Right })));
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
        assert!(candidate.rows.iter().all(|row| row.decision.as_ref()
            == &CandidateDecision::Entry(EntryDecision::Propagate {
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
                            let expected = match row.decision.as_ref() {
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
                            assert_eq!(other.decision.as_ref(), &expected);
                            if let CandidateDecision::BlockedByAncestor(ancestor) =
                                row.decision.as_ref()
                            {
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
        assert!(candidate.rows.iter().all(|row| row.decision.as_ref()
            == &CandidateDecision::Entry(EntryDecision::Propagate {
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

    #[test]
    fn move_pairing_preserves_path_order_when_either_role_is_smaller() {
        for on in [Side::Left, Side::Right] {
            for (archived, created) in [(2, 4), (4, 2), (3, 3)] {
                let old: Vec<_> = (0..archived)
                    .map(|i| path(format!("old-{i}").as_bytes()))
                    .collect();
                let new: Vec<_> = (0..created)
                    .map(|i| path(format!("new-{i}").as_bytes()))
                    .collect();
                let target: BTreeMap<_, _> = old.iter().map(|p| (p.clone(), file(1))).collect();
                let source: BTreeMap<_, _> = new.iter().map(|p| (p.clone(), file(1))).collect();
                let mut rows: Vec<_> = old
                    .iter()
                    .map(|p| {
                        PlanRow::new(
                            p.clone(),
                            CandidateDecision::Entry(EntryDecision::Archive { on }),
                        )
                    })
                    .chain(new.iter().map(|p| {
                        PlanRow::new(
                            p.clone(),
                            CandidateDecision::Entry(EntryDecision::Propagate {
                                from: on.other(),
                                change: ChangeKind::Create,
                            }),
                        )
                    }))
                    .collect();
                rows.sort_by(|left, right| left.path.cmp(&right.path));
                let trees = match on {
                    Side::Left => [&target, &source],
                    Side::Right => [&source, &target],
                };
                let paired = pair_moves(&mut rows, trees, [Moves::Rename; 2]);
                let expected: BTreeMap<_, _> = new.into_iter().zip(old).collect();
                assert_eq!(paired, expected);
                assert_eq!(
                    rows.iter()
                        .filter(|row| matches!(
                            row.decision.as_ref(),
                            CandidateDecision::Entry(EntryDecision::Move { .. })
                        ))
                        .count(),
                    archived.min(created)
                );
            }
        }
    }

    #[test]
    fn move_pairing_without_both_roles_does_not_index_versions() {
        use std::cell::Cell;
        struct Counted<'a>(&'a Cell<usize>);
        impl Versioned for Counted<'_> {
            fn version(&self) -> EntryVersion {
                self.0.set(self.0.get() + 1);
                file(1)
            }
        }
        let reads = Cell::new(0);
        let tree: BTreeMap<_, _> = (0..10_000)
            .map(|index| (path(format!("file-{index:05}").as_bytes()), Counted(&reads)))
            .collect();
        for decision in [
            EntryDecision::Archive { on: Side::Left },
            EntryDecision::Propagate {
                from: Side::Right,
                change: ChangeKind::Create,
            },
        ] {
            reads.set(0);
            let mut rows: Vec<_> = tree
                .keys()
                .map(|path| PlanRow::new(path.clone(), CandidateDecision::Entry(decision)))
                .collect();
            assert!(pair_moves(&mut rows, [&tree, &tree], [Moves::Rename; 2]).is_empty());
            assert_eq!(reads.get(), 0, "one role alone cannot form a move");
        }
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

    /// The pre-Cow key, kept here as the equivalence oracle for the borrowed
    /// and owned fast paths.
    fn owned_name_key(path: &[u8], names: NameMatching) -> Vec<u8> {
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

    /// Every matching mode, over UTF-8 and invalid names, must produce the
    /// same key bytes the owned form always produced.
    #[test]
    fn name_keys_match_the_owned_form_for_every_matching_mode() {
        let paths: &[&[u8]] = &[
            b"",
            b"a",
            b"A",
            b"REPORT.TXT",
            b"Report.txt",
            b"report.txt",
            b"a-b",
            b"a.txt",
            b"a/x",
            b"a0",
            b"Finder/File.TXT",
            "caf\u{e9}".as_bytes(),
            "CAF\u{c9}".as_bytes(),
            "cafe\u{301}".as_bytes(),
            "CAFE\u{301}".as_bytes(),
            "cafe\u{301}/readable".as_bytes(),
            "\u{1e69}".as_bytes(),
            "\u{1e63}\u{307}".as_bytes(),
            "\u{1c5}".as_bytes(),
            "\u{1c6}".as_bytes(),
            "\u{130}".as_bytes(),
            "\u{3a3}\u{3a3}".as_bytes(),
            "\u{41f}\u{440}\u{438}\u{432}\u{435}\u{442}".as_bytes(),
            b"\xff.txt",
            b"\xc3\x28",
            b"\xff.TXT",
            b"caf\xff",
            b"cafe\xcc\x81\xff",
        ];
        let modes = [
            NameMatching::EXACT,
            NameMatching::new(CaseMatching::Insensitive, UnicodeMatching::Exact),
            NameMatching::new(CaseMatching::Exact, UnicodeMatching::Normalized),
            NameMatching::new(CaseMatching::Insensitive, UnicodeMatching::Normalized),
        ];
        for path in paths {
            for names in modes {
                assert_eq!(
                    name_key(path, names).as_ref(),
                    owned_name_key(path, names).as_slice(),
                    "{path:?} {names:?}"
                );
            }
        }
    }

    /// The key borrows the path bytes exactly when the comparison leaves
    /// them unchanged; every actual case fold or recomposition allocates.
    #[test]
    fn name_keys_borrow_the_path_bytes_when_nothing_changes() {
        let case_insensitive =
            NameMatching::new(CaseMatching::Insensitive, UnicodeMatching::Normalized);
        let case_sensitive = NameMatching::new(CaseMatching::Exact, UnicodeMatching::Normalized);
        // Exact comparison always borrows, UTF-8 or not.
        for path in [
            b"\xff.txt".as_slice(),
            b"Report.txt",
            "cafe\u{301}".as_bytes(),
        ] {
            assert!(
                matches!(
                    name_key(path, NameMatching::EXACT),
                    std::borrow::Cow::Borrowed(_)
                ),
                "{path:?}"
            );
        }
        // ASCII is already NFC: normalization alone borrows.
        assert!(matches!(
            name_key(b"CAF\xc3\x89", case_sensitive),
            std::borrow::Cow::Borrowed(_)
        ));
        // Already lower case borrows under case folding; invalid UTF-8 with
        // no upper case byte to fold borrows too.
        for path in [
            b"report.txt".as_slice(),
            b"\xff.txt",
            "caf\u{e9}".as_bytes(),
        ] {
            assert!(
                matches!(
                    name_key(path, case_insensitive),
                    std::borrow::Cow::Borrowed(_)
                ),
                "{path:?}"
            );
        }
        // An actual ASCII fold allocates.
        assert!(matches!(
            name_key(b"Report.txt", case_insensitive),
            std::borrow::Cow::Owned(_)
        ));
        // Invalid UTF-8 with an upper case byte allocates the folded copy.
        assert!(matches!(
            name_key(b"\xff.TXT", case_insensitive),
            std::borrow::Cow::Owned(_)
        ));
        // An actual recomposition allocates, with or without case folding.
        assert!(matches!(
            name_key("cafe\u{301}".as_bytes(), case_sensitive),
            std::borrow::Cow::Owned(_)
        ));
        assert!(matches!(
            name_key("CAFE\u{301}".as_bytes(), case_insensitive),
            std::borrow::Cow::Owned(_)
        ));
    }

    /// The borrow decision must agree with the owned mapping wherever
    /// lower-casing is not obviously the identity.
    #[test]
    fn already_lowercase_agrees_with_unicode_lowercase() {
        let names = [
            "\u{130}",
            "\u{1c5}",
            "\u{1c6}",
            "\u{3a3}",
            "\u{3c2}",
            "\u{1e9e}",
            "\u{130}\u{300}",
            "\u{1e9e}\u{3a3}",
            "\u{fb00}",
            "\u{2170}",
            "\u{2c65}",
            "\u{a7b5}",
            "\u{2126}",
            "Report.txt",
            "report.txt",
        ];
        for name in names {
            assert_eq!(
                already_lowercase(name),
                name.to_lowercase() == name,
                "{name:?}"
            );
        }
    }

    /// The planner's row is one path and one shared decision reference:
    /// 16 bytes of `SyncPath` plus an 8-byte `Arc`, not the 24-byte
    /// `CandidateDecision` inline.
    #[test]
    fn plan_row_is_one_path_and_one_shared_decision_reference() {
        assert_eq!(size_of::<usize>(), 8);
        assert_eq!(
            size_of::<std::sync::Arc<CandidateDecision>>(),
            size_of::<usize>()
        );
        assert_eq!(size_of::<PlanRow>(), 24);
    }

    /// Independently enumerate every finite `EntryDecision` combination and
    /// verify both its semantic value and that all rows carrying it share one
    /// canonical `Arc`.
    #[test]
    fn every_entry_decision_shares_one_canonical_reference() {
        let mut all = vec![
            EntryDecision::Unchanged,
            EntryDecision::RecordBase,
            EntryDecision::ForgetBase,
            EntryDecision::KeepTarget,
        ];
        for from in [Side::Left, Side::Right] {
            for change in [
                ChangeKind::Create,
                ChangeKind::ReplaceContent,
                ChangeKind::UpdateMetadata,
            ] {
                all.push(EntryDecision::Propagate { from, change });
            }
        }
        for on in [Side::Left, Side::Right] {
            all.push(EntryDecision::Archive { on });
        }
        for on in [Side::Left, Side::Right] {
            all.push(EntryDecision::Move { on });
        }
        for on in [Side::Left, Side::Right] {
            all.push(EntryDecision::MovedAway { on });
        }
        for kind in [
            ConflictKind::BothChanged,
            ConflictKind::DeletedVersusEdited,
            ConflictKind::TypeMismatch,
            ConflictKind::MetadataOnly,
            ConflictKind::NameClash,
        ] {
            all.push(EntryDecision::Conflict(kind));
        }
        assert_eq!(all.len(), 21);
        assert_eq!(ENTRY_DECISIONS.len(), 21);
        for (index, decision) in all.iter().enumerate() {
            let expected = CandidateDecision::Entry(*decision);
            let first = PlanRow::new(path(b"first"), expected.clone());
            let second = PlanRow::new(path(b"second"), expected.clone());
            assert_eq!(first.decision(), &expected, "decision {index}");
            assert_eq!(
                Arc::as_ptr(&first.decision),
                Arc::as_ptr(&second.decision),
                "decision {index} must share its singleton"
            );
            assert!(
                ENTRY_DECISIONS
                    .iter()
                    .any(|canonical| Arc::as_ptr(canonical) == Arc::as_ptr(&first.decision)),
                "decision {index} must use the canonical table"
            );
            for other in all.iter().skip(index + 1) {
                let other_row = PlanRow::new(path(b"other"), CandidateDecision::Entry(*other));
                assert_ne!(
                    Arc::as_ptr(&first.decision),
                    Arc::as_ptr(&other_row.decision),
                    "decisions {index} and {other:?} must not share a singleton"
                );
            }
        }
    }

    /// Many rows blocked by one ancestor share a single `Arc`, while each
    /// still reports the exact original ancestor path.
    #[test]
    fn many_blocked_children_share_one_ancestor_reference() {
        const CHILDREN: usize = 10_000;
        let mut left = BTreeMap::new();
        let mut right = BTreeMap::new();
        left.insert(path(b"folder"), directory());
        right.insert(path(b"folder"), file(0));
        for index in 0..CHILDREN {
            left.insert(
                path(format!("folder/file-{index:05}").as_bytes()),
                file(index as u8),
            );
        }
        let candidate = plan(SyncMode::Sync, &BTreeMap::new(), &left, &right);
        assert_eq!(candidate.rows.len(), CHILDREN + 1);
        assert_eq!(
            candidate.rows[0].decision(),
            &CandidateDecision::Entry(EntryDecision::Conflict(ConflictKind::TypeMismatch))
        );
        let expected = CandidateDecision::BlockedByAncestor(path(b"folder"));
        let mut shared = None;
        for row in &candidate.rows[1..] {
            assert_eq!(row.decision(), &expected);
            let pointer = Arc::as_ptr(&row.decision);
            match shared {
                None => shared = Some(pointer),
                Some(first) => assert_eq!(pointer, first),
            }
        }
        assert!(shared.is_some());
    }

    /// Ten thousand no-op rows keep the one-million-row no-op concept at a
    /// fraction of the fixture memory: every row shares the `Unchanged`
    /// singleton and the row count is unchanged.
    #[test]
    fn ten_thousand_noop_rows_share_one_decision_reference() {
        const ROWS: usize = 10_000;
        let mut left = BTreeMap::new();
        let mut base = BTreeMap::new();
        for index in 0..ROWS {
            let version = file(index as u8);
            let entry_path = path(format!("file-{index:05}").as_bytes());
            left.insert(entry_path.clone(), version);
            base.insert(entry_path, BaselineEntry::new(version, version).unwrap());
        }
        let candidate = plan(SyncMode::Sync, &base, &left, &left);
        assert_eq!(candidate.rows.len(), ROWS);
        let expected = CandidateDecision::Entry(EntryDecision::Unchanged);
        let shared = Arc::as_ptr(&candidate.rows[0].decision);
        assert_eq!(candidate.rows[0].decision(), &expected);
        assert!(
            candidate
                .rows
                .iter()
                .all(|row| Arc::as_ptr(&row.decision) == shared),
            "every no-op row must share the Unchanged singleton"
        );
    }

    /// `Arc` equality and `Debug` follow the decision's content, not the
    /// pointer, so two independently allocated blocked decisions compare and
    /// print identically.
    #[test]
    fn arc_rows_compare_and_debug_by_content_not_pointer() {
        let decision = CandidateDecision::BlockedByAncestor(path(b"a"));
        let left = PlanRow::new(path(b"a/x"), decision.clone());
        let right = PlanRow::new(path(b"a/x"), decision.clone());
        assert_ne!(
            Arc::as_ptr(&left.decision),
            Arc::as_ptr(&right.decision),
            "the constructor allocates a fresh blocked Arc"
        );
        assert_eq!(left.decision(), right.decision());
        assert_eq!(left, right);
        assert_eq!(format!("{:?}", left), format!("{:?}", right));
        assert_eq!(
            format!("{:?}", left.decision()),
            format!("{:?}", right.decision())
        );
    }

    /// Blocking follows component boundaries: `a-b` is not inside `a`, and
    /// `a/x` reports the exact outermost blocking ancestor `a`.
    #[test]
    fn blocking_uses_component_boundaries_and_the_outermost_ancestor() {
        let mut left = BTreeMap::new();
        let mut right = BTreeMap::new();
        left.insert(path(b"a"), directory());
        left.insert(path(b"a/x"), file(1));
        left.insert(path(b"a-b"), directory());
        left.insert(path(b"a-b/y"), file(2));
        right.insert(path(b"a"), file(0));
        right.insert(path(b"a-b"), directory());
        right.insert(path(b"a-b/y"), file(2));
        let candidate = plan(SyncMode::Sync, &BTreeMap::new(), &left, &right);
        assert_eq!(
            decision(&candidate, b"a"),
            CandidateDecision::Entry(EntryDecision::Conflict(ConflictKind::TypeMismatch))
        );
        assert_eq!(
            decision(&candidate, b"a/x"),
            CandidateDecision::BlockedByAncestor(path(b"a"))
        );
        for name in [b"a-b".as_slice(), b"a-b/y"] {
            assert_ne!(
                decision(&candidate, name),
                CandidateDecision::BlockedByAncestor(path(b"a")),
                "{name:?} must not be blocked by a shared-prefix sibling"
            );
        }
    }

    /// The planner's control-check cadence and row count are unchanged: a
    /// small two-row plan checks control exactly twelve times and returns
    /// exactly two rows.
    #[test]
    fn control_cadence_and_row_count_are_unchanged() {
        let left = tree(&[(b"folder", directory()), (b"folder/file", file(0))]);
        let mut checks = 0;
        let candidate = plan_candidates(
            SyncMode::Sync,
            &BTreeMap::new(),
            &left,
            &BTreeMap::new(),
            &mut || {
                checks += 1;
                ScanControl::Continue
            },
        )
        .unwrap();
        assert_eq!(candidate.rows.len(), 2);
        assert_eq!(checks, 12);
    }

    /// The outermost conflict wins: an inner type conflict under a metadata
    /// conflict is suppressed and reports the outer ancestor.
    #[test]
    fn outermost_conflict_suppresses_an_inner_conflict() {
        let metadata = |byte| {
            EntryVersion::new(
                EntryContent::Directory,
                Digest::from_bytes([byte; Digest::BYTE_LEN]),
            )
        };
        let base = baseline(&tree(&[(b"a", metadata(0)), (b"a/x", metadata(0))]));
        let left = tree(&[(b"a", metadata(1)), (b"a/x", metadata(0))]);
        let right = tree(&[(b"a", metadata(2)), (b"a/x", file(0))]);
        let candidate = plan(SyncMode::Sync, &base, &left, &right);
        assert_eq!(
            decision(&candidate, b"a"),
            CandidateDecision::Entry(EntryDecision::Conflict(ConflictKind::MetadataOnly))
        );
        assert_eq!(
            decision(&candidate, b"a/x"),
            CandidateDecision::BlockedByAncestor(path(b"a"))
        );
    }
}

#[cfg(test)]
mod filtered_base_tests {
    use super::*;
    use foldover_types::Digest;

    fn path(name: &[u8]) -> SyncPath {
        SyncPath::try_from(name).unwrap()
    }
    fn version(kind: EntryContent) -> EntryVersion {
        EntryVersion::new(kind, Digest::from_bytes([3; 32]))
    }

    #[test]
    fn a_filtered_view_plans_like_removing_the_same_owned_rows() {
        let directory = version(EntryContent::Directory);
        let file = version(EntryContent::File(Digest::from_bytes([1; 32])));
        let current: BTreeMap<_, _> = [
            (b"a".as_slice(), directory),
            (b"a/x", file),
            (b"a-b", file),
            (b"a0", file),
            (b"other", file),
        ]
        .into_iter()
        .map(|(name, entry)| (path(name), entry))
        .collect();
        let base: BTreeMap<_, _> = current
            .iter()
            .map(|(path, entry)| (path.clone(), BaselineEntry::new(*entry, *entry).unwrap()))
            .collect();
        let dense: PathMap<_> = base.clone().into_iter().collect();
        for names in [
            vec![],
            vec![b"a-b".as_slice()],
            vec![b"a".as_slice(), b"a/x"],
            vec![b"missing".as_slice(), b"other"],
        ] {
            let mut excluded: Vec<_> = names.into_iter().map(path).collect();
            excluded.sort();
            let mut owned = base.clone();
            owned.retain(|path, _| excluded.binary_search(path).is_err());
            let filtered = FilteredBase::new(&dense, &excluded);
            assert_eq!(
                filtered.iter().collect::<Vec<_>>(),
                owned.iter().collect::<Vec<_>>()
            );
            for key in base.keys().chain(excluded.iter()) {
                assert_eq!(filtered.get(key), owned.get(key));
            }
            for mode in [
                SyncMode::Sync,
                SyncMode::Mirror { source: Side::Left },
                SyncMode::Backup {
                    source: Side::Right,
                },
            ] {
                let expected = plan_trees(
                    mode,
                    &owned,
                    &current,
                    &current,
                    &Choices::NONE,
                    &mut || ScanControl::Continue,
                )
                .unwrap();
                let actual = plan_trees(
                    mode,
                    &filtered,
                    &current,
                    &current,
                    &Choices::NONE,
                    &mut || ScanControl::Continue,
                )
                .unwrap();
                assert_eq!(actual.rows(), expected.rows());
                assert_eq!(actual.moves, expected.moves);
                assert_eq!(actual.left_deletions, expected.left_deletions);
                assert_eq!(actual.right_deletions, expected.right_deletions);
            }
        }
        assert!(
            dense.iter().eq(base.iter()),
            "the shared baseline stays intact"
        );
    }

    #[test]
    fn filtering_a_parent_preserves_validation_error_precedence() {
        let directory = version(EntryContent::Directory);
        let file = version(EntryContent::File(Digest::from_bytes([1; 32])));
        let base: PathMap<_> = [
            (
                path(b"a"),
                BaselineEntry::new(directory, directory).unwrap(),
            ),
            (path(b"a/x"), BaselineEntry::new(file, file).unwrap()),
        ]
        .into_iter()
        .collect();
        let excluded = [path(b"a")];
        let filtered = FilteredBase::new(&base, &excluded);
        let empty = BTreeMap::<SyncPath, EntryVersion>::new();
        let error = plan_trees(
            SyncMode::Sync,
            &filtered,
            &empty,
            &empty,
            &Choices::NONE,
            &mut || ScanControl::Continue,
        )
        .unwrap_err();
        assert_eq!(
            error,
            PlanError::MissingParent {
                tree: TreeRole::Base,
                path: path(b"a/x"),
                parent: path(b"a")
            }
        );
    }
}
