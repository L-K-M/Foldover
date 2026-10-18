//! One sync run of a local pair. Filesystem work happens outside the engine
//! lock; only base commits go through it, bound to the pair's revision.
//!
//! ```text
//! prepare: open roots ─▶ observe both sides ─▶ plan against the base
//! execute: deletion gate ─▶ archive leftovers
//!          ─▶ forward pass (parents first): copies, metadata, base records
//!          ─▶ reverse pass (children first): archives
//!          ─▶ base commits in batches, each checked against the revision
//! ```
//!
//! A crash loses at most an uncommitted batch. The next run then finds both
//! sides equal and records them, or a leftover temporary and archives it.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use foldover_endpoint::{
    ApplyError, Expected, LocalEndpoint, LocalPair, Observation, ObservedEntry, PairRootError,
    RunScope, ScanError,
};
use foldover_state::{BaseUpdate, RecordedIdentities};
use foldover_types::{
    BaselineEntry, ChangeKind, ConflictKind, EndpointId, EntryDecision, EntryVersion, PairConfig,
    ScanControl, Side, SyncPath,
};

use crate::{
    CandidateDecision, CandidatePlan, DeletionCounts, DeletionReview, PlanError, RunContext,
    plan_candidates,
};

/// Base updates per commit: bounds the work a crash can make the next run redo.
const COMMIT_BATCH: usize = 256;
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
    #[error(
        "The {side:?} folder is not the one this pair synced before: its identity file is missing \
         or different. Reconnect the original disk, or change the pair to start over."
    )]
    IdentityChanged { side: Side },
    #[error("Writing the {side:?} folder's identity failed: {source}")]
    IdentityWrite {
        side: Side,
        #[source]
        source: ApplyError,
    },
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
}

impl PreviewItem {
    pub fn path(&self) -> &SyncPath {
        &self.path
    }

    pub fn action(&self) -> Action {
        self.action
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Preview {
    items: Vec<PreviewItem>,
    left_deletions: DeletionCounts,
    right_deletions: DeletionCounts,
}

impl Preview {
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

/// Both sides observed and planned, ready to preview or execute.
pub struct PairRun {
    pair: PairConfig,
    identities: RecordedIdentities,
    endpoints: LocalPair,
    left: Observation,
    right: Observation,
    plan: CandidatePlan,
}

impl PairRun {
    /// Recorded identities must match the roots' identity files; until the
    /// first run records them, any folder is accepted.
    pub fn prepare(
        pair: PairConfig,
        base: &BTreeMap<SyncPath, BaselineEntry>,
        identities: RecordedIdentities,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<Self, RunError> {
        let draft = pair.draft();
        let endpoints = LocalPair::open(draft.left(), draft.right())?;
        for side in [Side::Left, Side::Right] {
            let Some(recorded) = identities.side(side) else {
                continue;
            };
            let observed = endpoints
                .endpoint(side)
                .identity()
                .map_err(|source| RunError::Observe { side, source })?;
            if observed != Some(recorded) {
                return Err(RunError::IdentityChanged { side });
            }
        }
        let policy = draft.options().metadata();
        let observe = |side: Side, control: &mut dyn FnMut() -> ScanControl| {
            endpoints
                .endpoint(side)
                .observe(policy, &mut || control())
                .map_err(|source| match source {
                    ScanError::Cancelled => RunError::Cancelled,
                    source => RunError::Observe { side, source },
                })
        };
        let left = observe(Side::Left, control)?;
        let right = observe(Side::Right, control)?;
        let plan = plan_candidates(
            draft.mode(),
            base,
            &left.versions(),
            &right.versions(),
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
            plan,
        })
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
                Some(PreviewItem {
                    path: row.path().clone(),
                    action,
                })
            })
            .collect();
        Preview {
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

        let revision = self.pair.revision();
        let options = self.pair.draft().options();
        if self.identities.side(Side::Left).is_none() || self.identities.side(Side::Right).is_none()
        {
            let ensure = |side: Side| {
                self.endpoints
                    .endpoint(side)
                    .ensure_identity(options.durability())
                    .map_err(|source| RunError::IdentityWrite { side, source })
            };
            let identities = Commit::Identities {
                left: ensure(Side::Left)?,
                right: ensure(Side::Right)?,
            };
            commit(revision, identities).map_err(RunError::Commit)?;
        }

        let scope = RunScope::new(started, options);
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
        for row in forward.chain(reverse) {
            if control() == ScanControl::Cancel {
                executor.report.completion = Completion::Cancelled;
                break;
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

    fn observation(&self, side: Side) -> &Observation {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }
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
        let from: &LocalEndpoint = endpoints.endpoint(target.other());
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
