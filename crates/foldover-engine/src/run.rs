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
use std::time::{SystemTime, UNIX_EPOCH};

use foldover_endpoint::{
    ApplyError, Expected, InvalidRunName, LocalEndpoint, LocalPair, Observation, ObservedEntry,
    PairRootError, RunScope, ScanError,
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
const SECONDS_PER_DAY: u64 = 86_400;
const SECONDS_PER_HOUR: u64 = 3_600;
const SECONDS_PER_MINUTE: u64 = 60;

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
    #[error(transparent)]
    RunName(#[from] InvalidRunName),
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
        run_name: &str,
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

        let scope = RunScope::new(run_name, options)?;
        let mut executor = Executor {
            run: &self,
            scope,
            report: RunReport::default(),
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
        Ok(RunOutcome::Ran(executor.report))
    }

    fn observation(&self, side: Side) -> &Observation {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }
}

/// A sortable, Finder-safe name for a run's archive folder, in UTC, e.g.
/// `2026-10-18T09-30-00.123Z`. Runs of one pair never share a millisecond.
pub fn run_name(now: SystemTime) -> String {
    let since = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = since.as_secs();
    let (year, month, day) = civil_from_days(seconds / SECONDS_PER_DAY);
    let time = seconds % SECONDS_PER_DAY;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}-{:02}-{:02}.{:03}Z",
        time / SECONDS_PER_HOUR,
        time % SECONDS_PER_HOUR / SECONDS_PER_MINUTE,
        time % SECONDS_PER_MINUTE,
        since.subsec_millis()
    )
}

/// Howard Hinnant's days-to-civil conversion for the proleptic Gregorian
/// calendar, from days since 1970-01-01.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
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
