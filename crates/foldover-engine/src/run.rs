//! One sync run of a local pair. Filesystem work happens outside the engine
//! lock; only base commits go through it, bound to the pair's revision.
//!
//! ```text
//! prepare: open roots ─▶ observe both sides ─▶ plan against the base
//! execute: first-run, emptiness and deletion gates ─▶ root identities
//!          ─▶ archive leftovers
//!          ─▶ forward pass (parents first): copies, metadata, base records
//!          ─▶ reverse pass (children first): archives
//!          ─▶ base commits in batches, each checked against the revision
//!             after both roots' identities are read again
//! ```
//!
//! A crash loses at most an uncommitted batch. The next run then finds both
//! sides equal and records them, or a leftover temporary and archives it.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime};

use foldover_endpoint::{
    ApplyError, Endpoint, EndpointPair, Expected, Filter, InvalidFilter, Observation,
    ObservedEntry, PairRootError, RunScope, ScanError, access_denied,
};
use foldover_state::{BaseUpdate, MountSources, RecordedIdentities, SnapshotEntry};
use foldover_types::{
    BaselineEntry, ChangeKind, ConflictKind, Durability, EndpointId, EntryContent, EntryDecision,
    EntryVersion, PairConfig, PairId, Resolution, ReviewToken, ScanControl, Side, SyncPath,
};

use crate::{
    CandidateDecision, CandidatePlan, DeletionCounts, DeletionReview, PlanError, PlanRow,
    RunContext, plan_resolved,
};

/// Base updates per commit: bounds the work a crash can make the next run redo.
const COMMIT_BATCH: usize = 256;
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;
/// Free space a run leaves on each volume, so the system and other writers
/// are not starved by a sync.
const SPACE_RESERVE: u64 = 256 * 1024 * 1024;
/// Owner decision (PLAN.md §10): archived versions are kept for 30 days.
const ARCHIVE_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Roots(#[from] PairRootError),
    #[error("Reading the {side:?} folder failed: {source}")]
    Observe {
        side: Side,
        #[source]
        source: ScanError,
    },
    #[error("{} ({source})", access_denied(*side))]
    AccessDenied {
        side: Side,
        #[source]
        source: ScanError,
    },
    #[error(
        "The {side:?} folder is not the one this pair synced before: its identity file is missing \
         or different. Reconnect the original disk, or change the pair to start over."
    )]
    IdentityChanged { side: Side },
    #[error(
        "The {side:?} folder could not be read during the sync, so it stopped: {source}. \
         Reconnect its disk or share and sync again."
    )]
    RootLost {
        side: Side,
        #[source]
        source: ScanError,
    },
    #[error("Writing the {side:?} folder's identity failed: {source}")]
    IdentityWrite {
        side: Side,
        #[source]
        source: ApplyError,
    },
    #[error(
        "The {side:?} folder is on an SFTP server, which cannot guarantee full durability; \
         choose standard durability for this pair"
    )]
    UnsupportedDurability { side: Side },
    #[error(
        "The {side:?} folder's disk has {available} bytes free; this sync needs {needed} plus a \
         256 MiB reserve. Free space and sync again."
    )]
    InsufficientSpace {
        side: Side,
        needed: u64,
        available: u64,
    },
    #[error(transparent)]
    Filter(#[from] InvalidFilter),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error("Saving sync progress failed: {0}")]
    Commit(#[source] crate::Error),
    #[error("Run cancelled before planning finished")]
    Cancelled,
}

/// Durable progress a run hands to the state store, in order.
#[derive(Clone, Copy, Debug)]
pub enum Commit<'a> {
    /// First run only, before any other write.
    Identities {
        left: EndpointId,
        right: EndpointId,
    },
    Base(&'a [BaseUpdate]),
}

/// What the user saw and accepted in a preview.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Approval {
    left_deletions: u64,
    right_deletions: u64,
}

impl Approval {
    pub const fn new(left_deletions: u64, right_deletions: u64) -> Self {
        Self {
            left_deletions,
            right_deletions,
        }
    }

    /// Any accepted deletion count at all; such approvals must bind to a
    /// reviewed plan.
    pub fn approves_anything(self) -> bool {
        self.left_deletions > 0 || self.right_deletions > 0
    }

    fn deletions(self, side: Side) -> u64 {
        match side {
            Side::Left => self.left_deletions,
            Side::Right => self.right_deletions,
        }
    }
}

/// A change a run would make, as shown in a preview.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Copy {
        to: Side,
        change: ChangeKind,
    },
    Archive {
        on: Side,
    },
    Conflict(ConflictKind),
    /// An ancestor's conflict must be resolved first.
    Blocked,
    /// Backup keeps a target-only entry or edit.
    KeepTarget,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviewItem {
    path: SyncPath,
    action: Action,
    left: Option<EntryVersion>,
    right: Option<EntryVersion>,
    bytes: u64,
}

impl PreviewItem {
    pub fn path(&self) -> &SyncPath {
        &self.path
    }

    pub fn action(&self) -> Action {
        self.action
    }

    /// What each side held when previewed; a resolution is bound to these.
    pub fn versions(&self) -> (Option<EntryVersion>, Option<EntryVersion>) {
        (self.left, self.right)
    }

    /// File data the item copies; 0 for everything else.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Preview {
    pair: PairId,
    revision: u64,
    items: Vec<PreviewItem>,
    left_deletions: DeletionCounts,
    right_deletions: DeletionCounts,
}

impl Preview {
    /// The pair this preview was planned for.
    pub fn pair(&self) -> PairId {
        self.pair
    }

    /// The pair revision this preview was planned for.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Every path with a visible change, conflict or block, in path order.
    pub fn items(&self) -> &[PreviewItem] {
        &self.items
    }

    pub fn deletions(&self, side: Side) -> DeletionCounts {
        match side {
            Side::Left => self.left_deletions,
            Side::Right => self.right_deletions,
        }
    }

    /// A deterministic fingerprint a client presents back to prove it
    /// reviewed this exact plan (ADR 0013): the pair, its revision, the
    /// deletion counts and every item's path, action, both versions and
    /// bytes — including items beyond a response page. The encoding is
    /// length-prefixed, so distinct plans cannot collide structurally.
    pub fn token(&self) -> ReviewToken {
        fn version(hasher: &mut blake3::Hasher, version: Option<EntryVersion>) {
            match version {
                None => {
                    hasher.update(&[0]);
                }
                Some(version) => {
                    hasher.update(&[1]);
                    match version.content() {
                        EntryContent::File(digest) => {
                            hasher.update(&[0]).update(digest.as_bytes());
                        }
                        EntryContent::Directory => {
                            hasher.update(&[1]);
                        }
                        EntryContent::Symlink(digest) => {
                            hasher.update(&[2]).update(digest.as_bytes());
                        }
                    }
                    hasher.update(version.metadata().as_bytes());
                }
            };
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"foldover:sync-preview:v1\0");
        hasher.update(self.pair.as_bytes());
        hasher.update(&self.revision.to_le_bytes());
        for counts in [self.left_deletions, self.right_deletions] {
            hasher.update(&counts.paths().to_le_bytes());
            hasher.update(&counts.deletions().to_le_bytes());
        }
        hasher.update(&(self.items.len() as u64).to_le_bytes());
        for item in &self.items {
            hasher.update(&(item.path.as_bytes().len() as u64).to_le_bytes());
            hasher.update(item.path.as_bytes());
            match item.action {
                Action::Copy { to, change } => {
                    hasher.update(&[0]);
                    hasher.update(&[to as u8]);
                    hasher.update(&[match change {
                        ChangeKind::Create => 0,
                        ChangeKind::ReplaceContent => 1,
                        ChangeKind::UpdateMetadata => 2,
                    }]);
                }
                Action::Archive { on } => {
                    hasher.update(&[1]);
                    hasher.update(&[on as u8]);
                }
                Action::Conflict(kind) => {
                    hasher.update(&[2]);
                    hasher.update(&[match kind {
                        ConflictKind::BothChanged => 0,
                        ConflictKind::DeletedVersusEdited => 1,
                        ConflictKind::TypeMismatch => 2,
                        ConflictKind::MetadataOnly => 3,
                    }]);
                }
                Action::Blocked => {
                    hasher.update(&[3]);
                }
                Action::KeepTarget => {
                    hasher.update(&[4]);
                }
            }
            version(&mut hasher, item.left);
            version(&mut hasher, item.right);
            hasher.update(&item.bytes.to_le_bytes());
        }
        ReviewToken::from_bytes(*hasher.finalize().as_bytes())
            .expect("a BLAKE3 digest is not all zero")
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Completion {
    #[default]
    Finished,
    Cancelled,
}

/// A change that failed; the run went on with independent paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Failure {
    path: SyncPath,
    message: String,
}

impl Failure {
    pub fn path(&self) -> &SyncPath {
        &self.path
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RunReport {
    completion: Completion,
    archive: String,
    pruned: u64,
    copied: u64,
    updated: u64,
    archived: u64,
    recovered: u64,
    conflicts: Vec<(SyncPath, ConflictKind)>,
    failures: Vec<Failure>,
}

impl RunReport {
    pub fn completion(&self) -> Completion {
        self.completion
    }
    /// This run's folder under `.foldover/archive` on each side.
    pub fn archive(&self) -> &str {
        &self.archive
    }
    /// Archived runs past retention, deleted after this run.
    pub fn pruned(&self) -> u64 {
        self.pruned
    }
    pub fn copied(&self) -> u64 {
        self.copied
    }
    pub fn updated(&self) -> u64 {
        self.updated
    }
    pub fn archived(&self) -> u64 {
        self.archived
    }
    /// Leftover temporaries from interrupted runs, moved to the archive.
    pub fn recovered(&self) -> u64 {
        self.recovered
    }
    pub fn conflicts(&self) -> &[(SyncPath, ConflictKind)] {
        &self.conflicts
    }
    pub fn failures(&self) -> &[Failure] {
        &self.failures
    }
}

#[derive(Debug)]
pub enum RunOutcome {
    Ran(RunReport),
    /// Too many deletions for this context; nothing was written.
    NeedsApproval(Preview),
}

/// Everything a run reads from the state store, taken under the engine lock.
pub struct RunInputs {
    pair: PairConfig,
    base: BTreeMap<SyncPath, BaselineEntry>,
    identities: RecordedIdentities,
    resolutions: BTreeMap<SyncPath, Resolution>,
    left_snapshot: BTreeMap<SyncPath, ObservedEntry>,
    right_snapshot: BTreeMap<SyncPath, ObservedEntry>,
}

impl RunInputs {
    pub fn new(
        pair: PairConfig,
        base: BTreeMap<SyncPath, BaselineEntry>,
        identities: RecordedIdentities,
        resolutions: BTreeMap<SyncPath, Resolution>,
    ) -> Self {
        Self {
            pair,
            base,
            identities,
            resolutions,
            left_snapshot: BTreeMap::new(),
            right_snapshot: BTreeMap::new(),
        }
    }

    /// Previous observations, whose versions are reused for entries with an
    /// unchanged stat. They must come from the same roots and policy.
    pub fn with_snapshots(
        self,
        left: BTreeMap<SyncPath, ObservedEntry>,
        right: BTreeMap<SyncPath, ObservedEntry>,
    ) -> Self {
        Self {
            left_snapshot: left,
            right_snapshot: right,
            ..self
        }
    }
}

/// Both sides observed and planned, ready to preview or execute.
pub struct PairRun {
    pair: PairConfig,
    identities: RecordedIdentities,
    endpoints: EndpointPair,
    left: Observation,
    right: Observation,
    /// Observations the filters excluded, kept for the next run's cache.
    left_cache: BTreeMap<SyncPath, ObservedEntry>,
    right_cache: BTreeMap<SyncPath, ObservedEntry>,
    plan: CandidatePlan,
    /// Whether the base remembered any entry the filters still include.
    remembers: Remembers,
}

/// Whether a run's base held entries, which makes an empty side news.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Remembers {
    Nothing,
    Entries,
}

impl PairRun {
    /// Recorded identities must match the roots' identity files; until the
    /// first run records them, any folder is accepted.
    pub fn prepare(
        inputs: RunInputs,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<Self, RunError> {
        let draft = inputs.pair.draft();
        let endpoints = EndpointPair::open(draft.left(), draft.right())?;
        Self::prepare_on(inputs, endpoints, control)
    }

    /// `prepare` on endpoints opened elsewhere.
    pub(crate) fn prepare_on(
        inputs: RunInputs,
        endpoints: EndpointPair,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<Self, RunError> {
        let RunInputs {
            pair,
            base,
            identities,
            resolutions,
            left_snapshot,
            right_snapshot,
        } = inputs;
        let draft = pair.draft();
        for side in [Side::Left, Side::Right] {
            let Some(recorded) = identities.side(side) else {
                continue;
            };
            let observed = endpoints
                .endpoint(side)
                .identity()
                .map_err(|source| observe_error(side, source))?;
            if observed != Some(recorded) {
                return Err(RunError::IdentityChanged { side });
            }
        }
        let policy = draft.options().metadata();
        // One filter for both sides, so an exclusion never looks like a deletion.
        let mut rules: Vec<String> = draft.options().filters().rules().to_vec();
        for side in [Side::Left, Side::Right] {
            rules.extend(
                endpoints
                    .endpoint(side)
                    .ignore_rules()
                    .map_err(|source| observe_error(side, source))?,
            );
        }
        let filters = draft.options().filters();
        let cutoff = filters.age_limit().and_then(|days| {
            SystemTime::now().checked_sub(Duration::from_secs(u64::from(days) * SECONDS_PER_DAY))
        });
        let filter = Filter::new(rules.iter().map(String::as_str))?
            .with_size_limit(filters.size_limit())
            .with_cutoff(cutoff);
        let observe = |side: Side, control: &mut dyn FnMut() -> ScanControl| {
            let previous = match side {
                Side::Left => &left_snapshot,
                Side::Right => &right_snapshot,
            };
            endpoints
                .endpoint(side)
                .observe_from(policy, &filter, previous, &mut || control())
                .map_err(|source| match source {
                    ScanError::Cancelled => RunError::Cancelled,
                    source => observe_error(side, source),
                })
        };
        let mut left = observe(Side::Left, control)?;
        let mut right = observe(Side::Right, control)?;
        // Size and age differ between folders. A file too large in either
        // folder, or old or absent in both, is skipped in both, and hidden
        // from the base too: its base row stays, so a later edit syncs as
        // an edit rather than looking new on both sides.
        let skipped: BTreeSet<SyncPath> = {
            let left_old: BTreeSet<&SyncPath> = left.old().iter().collect();
            let right_old: BTreeSet<&SyncPath> = right.old().iter().collect();
            let old_or_absent = |old: &BTreeSet<&SyncPath>, seen: &Observation, path| {
                old.contains(path) || !seen.entries().contains_key(*path)
            };
            left_old
                .iter()
                .chain(&right_old)
                .filter(|path| {
                    old_or_absent(&left_old, &left, path) && old_or_absent(&right_old, &right, path)
                })
                .copied()
                .chain(left.oversized().iter().chain(right.oversized()))
                .cloned()
                .collect()
        };
        let skipped: Vec<SyncPath> = skipped.into_iter().collect();
        // Excluded entries leave the plan, but unchanged ones should not be
        // reread every run: their observations stay cached for the next one.
        let cache = |observation: &Observation| {
            skipped
                .iter()
                .filter_map(|path| observation.entries().get_key_value(path))
                .map(|(path, entry)| (path.clone(), entry.clone()))
                .collect()
        };
        let left_cache = cache(&left);
        let right_cache = cache(&right);
        left.exclude(&skipped);
        right.exclude(&skipped);
        let mut base = base;
        for path in &skipped {
            base.remove(path);
        }
        let plan = plan_resolved(
            draft.mode(),
            &base,
            &left.versions(),
            &right.versions(),
            &resolutions,
            control,
        )
        .map_err(|error| match error {
            PlanError::Cancelled => RunError::Cancelled,
            error => RunError::Plan(error),
        })?;
        Ok(Self {
            pair,
            identities,
            endpoints,
            left,
            right,
            left_cache,
            right_cache,
            plan,
            remembers: if base.is_empty() {
                Remembers::Nothing
            } else {
                Remembers::Entries
            },
        })
    }

    pub fn revision(&self) -> u64 {
        self.pair.revision()
    }

    /// Where each side's network share is mounted from, to mount it again
    /// later. Informational: a side that cannot tell reports none.
    pub fn mount_sources(&self) -> MountSources {
        let source = |side| {
            self.endpoints
                .endpoint(side)
                .network_source()
                .ok()
                .flatten()
        };
        MountSources {
            left: source(Side::Left),
            right: source(Side::Right),
        }
    }

    /// This run's observation of one side, for the next run to reuse,
    /// filtered entries included: skipping them in the plan must not make
    /// the next run reread unchanged data.
    pub fn snapshot(&self, side: Side) -> Vec<SnapshotEntry> {
        let cache = match side {
            Side::Left => &self.left_cache,
            Side::Right => &self.right_cache,
        };
        let entries: BTreeMap<_, _> = cache
            .iter()
            .chain(self.observation(side).entries())
            .collect();
        entries
            .into_iter()
            .map(|(path, entry)| SnapshotEntry {
                path: path.clone(),
                fingerprint: entry.fingerprint_bytes(),
                version: entry.version(),
            })
            .collect()
    }

    pub fn preview(&self) -> Preview {
        let items = self
            .plan
            .rows()
            .iter()
            .filter_map(|row| {
                let action = match row.decision() {
                    CandidateDecision::BlockedByAncestor(_) => Action::Blocked,
                    CandidateDecision::Entry(decision) => match *decision {
                        EntryDecision::Unchanged
                        | EntryDecision::RecordBase
                        | EntryDecision::ForgetBase => return None,
                        EntryDecision::KeepTarget => Action::KeepTarget,
                        EntryDecision::Propagate { from, change } => Action::Copy {
                            to: from.other(),
                            change,
                        },
                        EntryDecision::Archive { on } => Action::Archive { on },
                        EntryDecision::Conflict(kind) => Action::Conflict(kind),
                    },
                };
                let version = |side: Side| {
                    self.observation(side)
                        .entries()
                        .get(row.path())
                        .map(ObservedEntry::version)
                };
                Some(PreviewItem {
                    path: row.path().clone(),
                    action,
                    left: version(Side::Left),
                    right: version(Side::Right),
                    bytes: self.copied_bytes(row).map_or(0, |(_, bytes)| bytes),
                })
            })
            .collect();
        Preview {
            pair: self.pair.id(),
            revision: self.pair.revision(),
            items,
            left_deletions: self.plan.deletions(Side::Left),
            right_deletions: self.plan.deletions(Side::Right),
        }
    }

    /// Execute the plan. `commit` stores progress for the planned pair
    /// revision and fails if the pair changed; the run then stops.
    pub fn execute(
        self,
        approval: Approval,
        context: RunContext,
        started: SystemTime,
        commit: &mut impl FnMut(u64, Commit<'_>) -> Result<(), crate::Error>,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<RunOutcome, RunError> {
        let options = self.pair.draft().options();
        // Full durability is a promise a generic SFTP server cannot
        // keep; refuse before anything — identities included — mutates
        // either folder.
        for side in [Side::Left, Side::Right] {
            if options.durability() == Durability::Full
                && matches!(self.endpoints.endpoint(side), Endpoint::Sftp(_))
            {
                return Err(RunError::UnsupportedDurability { side });
            }
        }

        // PLAN.md §3.2, §3.6: the first run of every pair is previewed,
        // which also covers a non-empty target's confirmation. Only the
        // exact plan a person saw (`RunContext::Reviewed`) records the
        // identities and writes; anything else returns the plan for review.
        let first_run = self.identities.side(Side::Left).is_none()
            || self.identities.side(Side::Right).is_none();
        if first_run && context != RunContext::Reviewed {
            return Ok(RunOutcome::NeedsApproval(self.preview()));
        }
        // PLAN.md §3.6: a folder that is suddenly empty — wiped, or a stale
        // copy put in its place — must not empty the other side unseen,
        // even below the mass-deletion fuse.
        if self.emptied_side_archives() && context != RunContext::Reviewed {
            return Ok(RunOutcome::NeedsApproval(self.preview()));
        }
        for side in [Side::Left, Side::Right] {
            let approved = self.plan.deletions(side).deletions() <= approval.deletions(side);
            let allowed = match self.plan.deletion_review(side, context) {
                DeletionReview::WithinLimits => true,
                DeletionReview::ConfirmationRequired => approved,
                DeletionReview::StopUnattended => false,
            };
            if !allowed {
                return Ok(RunOutcome::NeedsApproval(self.preview()));
            }
        }

        // Replaced versions stay in the archive on the same volume, so a
        // copy needs its full size whether it creates or replaces.
        for side in [Side::Left, Side::Right] {
            let needed = self.bytes_to_copy(side);
            if needed == 0 {
                continue;
            }
            let available = self
                .endpoints
                .endpoint(side)
                .available_space()
                .map_err(|source| observe_error(side, source))?;
            require_space(side, needed, available)?;
        }

        let revision = self.pair.revision();
        let identities = match (
            self.identities.side(Side::Left),
            self.identities.side(Side::Right),
        ) {
            // The scan may have taken hours; both roots must still be the
            // recorded ones before the first write.
            (Some(left), Some(right)) => {
                let identities = [left, right];
                self.check_roots(identities)?;
                identities
            }
            _ => {
                // A side whose identity is recorded must still hold it,
                // as when only the other root changed; only an unrecorded
                // side gets one written or adopted.
                let ensure = |side: Side| {
                    let endpoint = self.endpoints.endpoint(side);
                    let Some(recorded) = self.identities.side(side) else {
                        return endpoint
                            .ensure_identity(options.durability())
                            .map_err(|source| RunError::IdentityWrite { side, source });
                    };
                    let observed = endpoint
                        .identity()
                        .map_err(|source| RunError::RootLost { side, source })?;
                    if observed != Some(recorded) {
                        return Err(RunError::IdentityChanged { side });
                    }
                    Ok(recorded)
                };
                let (left, right) = (ensure(Side::Left)?, ensure(Side::Right)?);
                commit(revision, Commit::Identities { left, right }).map_err(RunError::Commit)?;
                [left, right]
            }
        };

        let scope = RunScope::new(started, options.clone());
        let mut executor = Executor {
            run: &self,
            report: RunReport {
                archive: scope.name().to_owned(),
                ..RunReport::default()
            },
            scope,
            pending: Vec::new(),
            failed: Vec::new(),
        };
        let mut commit = |updates: &[BaseUpdate]| {
            commit(revision, Commit::Base(updates)).map_err(RunError::Commit)
        };

        for side in [Side::Left, Side::Right] {
            let observation = self.observation(side);
            for leftover in observation.leftovers() {
                match self
                    .endpoints
                    .endpoint(side)
                    .recover_leftover(leftover, &executor.scope)
                {
                    Ok(()) => executor.report.recovered += 1,
                    Err(error) => executor.fail(leftover, &error),
                }
            }
        }

        let rows = self.plan.rows();
        let forward = rows.iter().filter(|row| !is_archive(row.decision()));
        let reverse = rows.iter().rev().filter(|row| is_archive(row.decision()));
        for (index, row) in forward.chain(reverse).enumerate() {
            if control() == ScanControl::Cancel {
                executor.report.completion = Completion::Cancelled;
                break;
            }
            // PLAN.md §4.7: writes go through handles held on the roots, so
            // an unmounted disk fails rather than taking writes at its old
            // path. Reading both identities every batch stops the run at the
            // first sign of that instead of failing entry by entry.
            if index > 0
                && index % COMMIT_BATCH == 0
                && let Err(error) = self.check_roots(identities)
            {
                commit(&executor.pending)?;
                return Err(error);
            }
            executor.apply(row.path(), row.decision(), control);
            if executor.pending.len() >= COMMIT_BATCH {
                commit(&executor.pending)?;
                executor.pending.clear();
            }
        }
        commit(&executor.pending)?;

        // Retention deletes data, so it runs only after a complete run.
        if executor.report.completion == Completion::Finished {
            for side in [Side::Left, Side::Right] {
                let endpoint = self.endpoints.endpoint(side);
                match endpoint.prune_archive(ARCHIVE_RETENTION, started) {
                    Ok(pruned) => executor.report.pruned += pruned,
                    Err(error) => executor.fail_message(
                        &SyncPath::try_from(b".foldover/archive".as_slice())
                            .expect("the archive path is valid"),
                        &error.to_string(),
                    ),
                }
            }
        }
        Ok(RunOutcome::Ran(executor.report))
    }

    /// Both roots still hold `identities`, in left, right order.
    fn check_roots(&self, identities: [EndpointId; 2]) -> Result<(), RunError> {
        for (side, identity) in [Side::Left, Side::Right].into_iter().zip(identities) {
            let observed = self
                .endpoints
                .endpoint(side)
                .identity()
                .map_err(|source| RunError::RootLost { side, source })?;
            if observed != Some(identity) {
                return Err(RunError::IdentityChanged { side });
            }
        }
        Ok(())
    }

    /// One side holds nothing although the base remembers entries, and the
    /// plan archives on the other side because of it.
    fn emptied_side_archives(&self) -> bool {
        if self.remembers == Remembers::Nothing {
            return false;
        }
        [Side::Left, Side::Right].into_iter().any(|side| {
            self.observation(side).entries().is_empty() && self.plan.rows().iter().any(|row| {
                matches!(
                    row.decision(),
                    CandidateDecision::Entry(EntryDecision::Archive { on }) if *on == side.other()
                )
            })
        })
    }

    /// Data bytes the plan copies to `side`.
    fn bytes_to_copy(&self, side: Side) -> u64 {
        self.plan
            .rows()
            .iter()
            .filter_map(|row| self.copied_bytes(row))
            .filter(|(to, _)| *to == side)
            .map(|(_, bytes)| bytes)
            .fold(0, u64::saturating_add)
    }

    /// The side a row copies file data to, and how many bytes.
    fn copied_bytes(&self, row: &PlanRow) -> Option<(Side, u64)> {
        let CandidateDecision::Entry(EntryDecision::Propagate {
            from,
            change: ChangeKind::Create | ChangeKind::ReplaceContent,
        }) = row.decision()
        else {
            return None;
        };
        let source = self.observation(*from).entries().get(row.path())?;
        (source.version().kind() == foldover_types::EntryKind::File)
            .then(|| (from.other(), source.size()))
    }

    fn observation(&self, side: Side) -> &Observation {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }
}

/// A failed read of one side, with guidance when access was refused.
fn observe_error(side: Side, source: ScanError) -> RunError {
    if source.is_access_denied() {
        return RunError::AccessDenied { side, source };
    }
    RunError::Observe { side, source }
}

fn require_space(side: Side, needed: u64, available: u64) -> Result<(), RunError> {
    // Subtracting avoids overflow: needed + reserve could exceed u64::MAX.
    if needed > available.saturating_sub(SPACE_RESERVE) {
        return Err(RunError::InsufficientSpace {
            side,
            needed,
            available,
        });
    }
    Ok(())
}

fn is_archive(decision: &CandidateDecision) -> bool {
    matches!(
        decision,
        CandidateDecision::Entry(EntryDecision::Archive { .. })
    )
}

struct Executor<'a> {
    run: &'a PairRun,
    scope: RunScope,
    report: RunReport,
    pending: Vec<BaseUpdate>,
    /// Paths whose change failed; their descendants are skipped.
    failed: Vec<SyncPath>,
}

impl Executor<'_> {
    fn apply(
        &mut self,
        path: &SyncPath,
        decision: &CandidateDecision,
        control: &mut impl FnMut() -> ScanControl,
    ) {
        let decision = match decision {
            CandidateDecision::BlockedByAncestor(_) => return,
            CandidateDecision::Entry(decision) => *decision,
        };
        if self.failed.iter().any(|failed| failed.is_ancestor_of(path)) {
            return;
        }
        let entry = |side: Side| self.run.observation(side).entries().get(path);
        match decision {
            EntryDecision::Unchanged | EntryDecision::KeepTarget => {}
            EntryDecision::Conflict(kind) => self.report.conflicts.push((path.clone(), kind)),
            EntryDecision::ForgetBase => self.pending.push(BaseUpdate::Forget(path.clone())),
            EntryDecision::RecordBase => {
                let versions = entry(Side::Left).zip(entry(Side::Right));
                match versions
                    .map(|(left, right)| BaselineEntry::new(left.version(), right.version()))
                {
                    Some(Ok(base)) => self.pending.push(BaseUpdate::Record(path.clone(), base)),
                    _ => self.fail_message(path, "Planned base record has no matching sides"),
                }
            }
            EntryDecision::Propagate { from, change } => {
                let Some(source) = entry(from) else {
                    return self.fail_message(path, "Planned source entry is missing");
                };
                let target = from.other();
                match self.propagate(path, source, target, entry(target), change, control) {
                    Ok(stored) => self.record(path, from, source.version(), stored, change),
                    Err(error) => self.fail(path, &error),
                }
            }
            EntryDecision::Archive { on } => {
                let Some(current) = entry(on) else {
                    return self.fail_message(path, "Planned archive entry is missing");
                };
                match self
                    .run
                    .endpoints
                    .endpoint(on)
                    .remove(path, current, &self.scope, control)
                {
                    Ok(()) => {
                        self.report.archived += 1;
                        self.pending.push(BaseUpdate::Forget(path.clone()));
                    }
                    Err(error) => self.fail(path, &error),
                }
            }
        }
    }

    fn propagate(
        &self,
        path: &SyncPath,
        source: &ObservedEntry,
        target: Side,
        current: Option<&ObservedEntry>,
        change: ChangeKind,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<EntryVersion, ApplyError> {
        let endpoints = &self.run.endpoints;
        let from: &Endpoint = endpoints.endpoint(target.other());
        let to = endpoints.endpoint(target);
        match (change, current) {
            (ChangeKind::UpdateMetadata, Some(current)) => {
                to.update_metadata_from(from, path, source, current, &self.scope, control)
            }
            (_, current) => {
                let current = current.map_or(Expected::Absent, Expected::Present);
                to.copy_from(from, path, source, current, &self.scope, control)
            }
        }
    }

    fn record(
        &mut self,
        path: &SyncPath,
        from: Side,
        source: EntryVersion,
        stored: EntryVersion,
        change: ChangeKind,
    ) {
        let (left, right) = match from {
            Side::Left => (source, stored),
            Side::Right => (stored, source),
        };
        match BaselineEntry::new(left, right) {
            Ok(base) => {
                match change {
                    ChangeKind::UpdateMetadata => self.report.updated += 1,
                    ChangeKind::Create | ChangeKind::ReplaceContent => self.report.copied += 1,
                }
                self.pending.push(BaseUpdate::Record(path.clone(), base));
            }
            Err(_) => self.fail_message(path, "Stored content differs from the source"),
        }
    }

    fn fail(&mut self, path: &SyncPath, error: &ApplyError) {
        self.fail_message(path, &error.to_string());
    }

    fn fail_message(&mut self, path: &SyncPath, message: &str) {
        self.report.failures.push(Failure {
            path: path.clone(),
            message: message.to_owned(),
        });
        self.failed.push(path.clone());
    }
}

#[cfg(test)]
mod tests;
