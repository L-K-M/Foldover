//! Previews and runs as background jobs, at most one per pair. Clients start
//! a job, then poll its status; the result stays until the pair's next job.
//!
//! ```text
//! Idle ─start─▶ Working ─▶ Previewed | NeedsApproval | Finished | Undone | Failed
//!                  └─cancel─▶ Idle (before writing) or Finished (cancelled)
//! ```
//!
//! Jobs hold the engine lock only to read inputs and commit progress, so
//! configuration requests stay responsive during long runs.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(test)]
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use foldover_engine::{
    Action, Approval, Commit, Completion, DeletionReview, Engine, KeepBothError, ObserveStep,
    PairRun, Preview, PreviewItem, RunContext, RunError, RunOutcome, RunProgress, RunReport,
    UndoReport, missing_side, reconnect,
};
use foldover_proto::local::RpcError;
use foldover_proto::local::{
    INTERNAL_ERROR, INVALID_PARAMS, PREVIEW_REQUIRED, ROOT_UNAVAILABLE, SYNC_BUSY, SYNC_ITEM_LIMIT,
    SyncAction, SyncConflict, SyncCounts, SyncDeletions, SyncFailure, SyncItem, SyncItemKind,
    SyncItemPage, SyncJob, SyncPreview, SyncProgress, SyncResult, SyncStatus, UndoResult,
};
use foldover_types::{
    ChangeKind, ConflictKind, EntryVersion, PairId, Resolution, ReviewToken, ScanControl, Side,
    Skip, SyncPath,
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Work {
    Preview,
    /// Interactive run; `ReviewToken` proves which preview the caller saw.
    Run(Approval, Option<ReviewToken>),
    /// Unattended: stops instead of asking when deletions need approval.
    Scheduled,
    /// Undo the pair's newest writing run (ADR 0029).
    Undo,
}

impl Work {
    fn job(self) -> SyncJob {
        match self {
            Self::Preview => SyncJob::Preview,
            Self::Run(..) | Self::Scheduled => SyncJob::Run,
            Self::Undo => SyncJob::Undo,
        }
    }
}

/// Each side's version of a conflicted path; `None` when absent.
type ConflictVersions = (Option<EntryVersion>, Option<EntryVersion>);

/// The plan a person last saw: every item, for paging and for the
/// versions a choice binds to, and the whole preview a nonzero approval
/// must match (ADRs 0013, 0020, 0023).
struct Reviewed {
    revision: u64,
    /// Fingerprint of `plan`; clients present it to bind to this preview.
    token: ReviewToken,
    items: Vec<SyncItem>,
    plan: Preview,
}

impl Reviewed {
    fn from(preview: &Preview) -> Self {
        Self {
            revision: preview.revision(),
            token: preview.token(),
            items: preview.items().iter().map(wire_item).collect(),
            plan: preview.clone(),
        }
    }

    /// The item at `path`; items are in path order.
    fn item(&self, path: &SyncPath) -> Option<&PreviewItem> {
        let items = self.plan.items();
        let index = items.binary_search_by(|item| item.path().cmp(path)).ok()?;
        Some(&items[index])
    }
}

struct Job {
    status: SyncStatus,
    cancel: Arc<AtomicBool>,
    reviewed: Option<Reviewed>,
    /// The working job's counters; status requests read them live.
    progress: Arc<Progress>,
}

/// How far a job has got, written by its run and read by status requests.
#[derive(Default)]
pub(crate) struct Progress {
    listed: AtomicU64,
    checked: AtomicU64,
    changes: AtomicU64,
    applied: AtomicU64,
    bytes: AtomicU64,
    copied: AtomicU64,
    waiting_for: Mutex<Option<String>>,
}

impl RunProgress for Progress {
    fn observed(&self, _: Side, step: ObserveStep) {
        let counter = match step {
            ObserveStep::Listed => &self.listed,
            ObserveStep::Checked => &self.checked,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn planned(&self, changes: u64, bytes: u64) {
        self.changes.store(changes, Ordering::Relaxed);
        self.bytes.store(bytes, Ordering::Relaxed);
    }

    fn applied(&self, bytes: u64) {
        self.applied.fetch_add(1, Ordering::Relaxed);
        self.copied.fetch_add(bytes, Ordering::Relaxed);
    }

    fn waiting(&self, holder: Option<&str>) {
        *self
            .waiting_for
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = holder.map(str::to_owned);
    }
}

impl Progress {
    fn wire(&self) -> SyncProgress {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        SyncProgress {
            listed: read(&self.listed),
            checked: read(&self.checked),
            changes: read(&self.changes),
            applied: read(&self.applied),
            bytes: read(&self.bytes),
            copied: read(&self.copied),
            waiting_for: self
                .waiting_for
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        }
    }
}

/// What a job start hands its run.
struct Claim {
    /// The review the replaced job held.
    held: Option<Reviewed>,
    skips: BTreeMap<SyncPath, Skip>,
    progress: Arc<Progress>,
    /// The status stored under the lock, to answer the start request
    /// with: a fast job can publish its result before `status(id)` is
    /// re-read, so the acknowledgment is the claim-time value.
    acknowledged: SyncStatus,
}

#[derive(Default)]
struct State {
    jobs: HashMap<PairId, Job>,
    /// Changes a person chose to leave alone in the pair's next run; spent
    /// by the next run that executes.
    skips: HashMap<PairId, BTreeMap<SyncPath, Skip>>,
    /// Pairs whose configuration is being replaced refuse new job claims.
    mutating: HashSet<PairId>,
}

#[derive(Default)]
pub(crate) struct Jobs {
    state: Mutex<State>,
    /// Test-only rendezvous inside a mutating pair command.
    #[cfg(test)]
    mutation_gate: Mutex<Option<(Sender<()>, Receiver<()>)>>,
}

impl Jobs {
    pub(crate) fn status(&self, id: PairId) -> SyncStatus {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(job) = state.jobs.get(&id) else {
            return SyncStatus::Idle;
        };
        match &job.status {
            SyncStatus::Working { job: work, .. } => SyncStatus::Working {
                job: *work,
                cancelling: job.cancel.load(Ordering::Relaxed),
                progress: job.progress.wire(),
            },
            status => status.clone(),
        }
    }

    /// Record a choice for a conflict in the pair's latest preview. The
    /// pair's mutation reservation spans the check and the write, so a
    /// job or configuration change cannot start in between.
    pub(crate) fn resolve(
        self: &Arc<Self>,
        engine: &Mutex<Engine>,
        id: PairId,
        path: &SyncPath,
        keep: Side,
        token: Option<ReviewToken>,
    ) -> Result<(), RpcError> {
        let _mutation = self.mutation(id)?;
        #[cfg(test)]
        self.wait_mutation_gate();
        self.resolve_reviewed(engine, id, path, keep, token)
    }

    /// `resolve` with the pair's mutation reservation already held.
    fn resolve_reviewed(
        &self,
        engine: &Mutex<Engine>,
        id: PairId,
        path: &SyncPath,
        keep: Side,
        token: Option<ReviewToken>,
    ) -> Result<(), RpcError> {
        let (revision, _, (left, right)) = self.choice(id, path, token)?;
        engine
            .lock()
            .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))?
            .record_resolution(id, revision, path, Resolution::new(keep, left, right))
            .map_err(crate::engine_error)
    }

    /// Save the right folder's version of a both-changed conflict beside
    /// it, then keep the left folder's at the path; the next run syncs both.
    pub(crate) fn keep_both(
        self: &Arc<Self>,
        engine: &Mutex<Engine>,
        id: PairId,
        path: &SyncPath,
        token: Option<ReviewToken>,
    ) -> Result<(), RpcError> {
        let _mutation = self.mutation(id)?;
        #[cfg(test)]
        self.wait_mutation_gate();
        let (revision, action, (_, right)) = self.choice(id, path, token)?;
        let (Action::Conflict(ConflictKind::BothChanged), Some(right)) = (action, right) else {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "Keep both applies to files changed in both folders",
            ));
        };
        let pair = engine
            .lock()
            .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))?
            .pair(id)
            .map_err(crate::engine_error)?;
        // The token still matches the held preview after a configuration
        // change — the review is kept until the next job — so the revision
        // must be checked before keep-both writes into the new roots. The
        // reservation already held stops a change racing this copy.
        if pair.revision() != revision {
            return Err(RpcError::new(
                PREVIEW_REQUIRED,
                "The pair changed since that preview; preview again",
            ));
        }
        let reach = engine
            .lock()
            .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))?
            .reach();
        foldover_engine::keep_both(&pair, &reach, path, right, &mut || ScanControl::Continue)
            .map_err(|error| match error {
                KeepBothError::Changed(_) => RpcError::new(PREVIEW_REQUIRED, error.to_string()),
                error => RpcError::new(ROOT_UNAVAILABLE, error.to_string()),
            })?;
        // The reservation is already held: resolve through it, not a
        // second `mutation` call, which would refuse its own marker.
        self.resolve_reviewed(engine, id, path, Side::Left, token)
    }

    /// Leave a previewed change alone in the pair's next run, while both
    /// sides still hold what the person saw (PLAN.md §3.8, skip once).
    pub(crate) fn skip(
        self: &Arc<Self>,
        id: PairId,
        path: &SyncPath,
        token: Option<ReviewToken>,
    ) -> Result<(), RpcError> {
        let _mutation = self.mutation(id)?;
        let (_, _, (left, right)) = self.choice(id, path, token)?;
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .skips
            .entry(id)
            .or_default()
            .insert(path.clone(), Skip::new(left, right));
        Ok(())
    }

    /// A change or conflict from the pair's latest preview, while no job
    /// works. `token` must name that preview, so a choice cannot follow a
    /// newer plan the caller never saw. Blocked items wait for the
    /// conflict above them.
    fn choice(
        &self,
        id: PairId,
        path: &SyncPath,
        token: Option<ReviewToken>,
    ) -> Result<(u64, Action, ConflictVersions), RpcError> {
        let stale = || {
            RpcError::new(
                PREVIEW_REQUIRED,
                "This change is not in the pair's latest preview; preview again",
            )
        };
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let job = state.jobs.get(&id).ok_or_else(stale)?;
        if matches!(job.status, SyncStatus::Working { .. }) {
            return Err(RpcError::new(
                SYNC_BUSY,
                "Wait for the current preview or sync to finish",
            ));
        }
        let reviewed = job.reviewed.as_ref().ok_or_else(stale)?;
        if token != Some(reviewed.token) {
            return Err(stale());
        }
        let item = reviewed.item(path).ok_or_else(stale)?;
        if item.action() == Action::Blocked {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "Resolve the conflict above this item first",
            ));
        }
        // Keeping a side would only collide again; a rename resolves it.
        if item.action() == Action::Conflict(ConflictKind::NameClash) {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "Rename one of the items whose names clash, then preview again",
            ));
        }
        Ok((reviewed.revision, item.action(), item.versions()))
    }

    /// A page of the latest preview's items after `after`, optionally of
    /// one kind. `token` must name that preview, and the preview must be
    /// for the pair's current revision; the engine lock is taken before
    /// the jobs lock, never inside it.
    pub(crate) fn items(
        &self,
        engine: &Mutex<Engine>,
        id: PairId,
        token: Option<ReviewToken>,
        after: Option<&[u8]>,
        kind: Option<SyncItemKind>,
    ) -> Result<SyncItemPage, RpcError> {
        let stale = || RpcError::new(PREVIEW_REQUIRED, "Preview the pair first");
        let revision = engine
            .lock()
            .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))?
            .pair(id)
            .map_err(crate::engine_error)?
            .revision();
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let reviewed = state
            .jobs
            .get(&id)
            .and_then(|job| job.reviewed.as_ref())
            .ok_or_else(stale)?;
        if token != Some(reviewed.token) || reviewed.revision != revision {
            return Err(RpcError::new(
                PREVIEW_REQUIRED,
                "The pair changed since that preview; preview again",
            ));
        }
        // Items are in path order, so the cursor is a binary search.
        let start = after.map_or(0, |after| {
            reviewed
                .items
                .partition_point(|item| item.path.as_slice() <= after)
        });
        let mut matching = reviewed.items[start..]
            .iter()
            .filter(|item| kind.is_none_or(|kind| kind == SyncItemKind::of(&item.action)));
        let items: Vec<_> = matching.by_ref().take(SYNC_ITEM_LIMIT).cloned().collect();
        Ok(SyncItemPage {
            items,
            more: matching.next().is_some(),
        })
    }

    /// Show that an unattended run waits for a missing folder. A working
    /// job, a result a person is reviewing, and a pair whose
    /// configuration is changing keep their state; earlier conflicts
    /// stay resolvable.
    pub(crate) fn wait_for(&self, id: PairId, side: Side) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.mutating.contains(&id) {
            return;
        }
        let waiting = SyncStatus::Waiting { side: side.into() };
        match state.jobs.get_mut(&id) {
            Some(job)
                if matches!(
                    job.status,
                    SyncStatus::Working { .. }
                        | SyncStatus::Previewed { .. }
                        | SyncStatus::NeedsApproval { .. }
                        | SyncStatus::Undone { .. }
                        | SyncStatus::Failed { .. }
                ) => {}
            Some(job) => job.status = waiting,
            None => {
                state.jobs.insert(
                    id,
                    Job {
                        status: waiting,
                        cancel: Arc::new(AtomicBool::new(false)),
                        reviewed: None,
                        progress: Arc::default(),
                    },
                );
            }
        }
    }

    pub(crate) fn cancel(&self, id: PairId) {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(job) = state.jobs.get(&id) {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Start a job and run it on the blocking pool.
    pub(crate) fn start(
        self: &Arc<Self>,
        engine: &Arc<Mutex<Engine>>,
        id: PairId,
        work: Work,
    ) -> Result<SyncStatus, RpcError> {
        let cancel = Arc::new(AtomicBool::new(false));
        let Claim {
            held,
            skips,
            progress,
            acknowledged,
        } = self.claim(id, work, &cancel)?;
        let engine = Arc::clone(engine);
        let jobs = Arc::clone(self);
        self.spawn(id, move || {
            let started = SystemTime::now();
            let (status, reviewed) = perform(&engine, id, work, &cancel, held, skips, &progress);
            crate::history::record(&engine, id, work, started, &status);
            // A skip lasts one executed run.
            if matches!(status, SyncStatus::Finished { .. }) {
                jobs.spend_skips(id);
            }
            (status, reviewed)
        });
        Ok(acknowledged)
    }

    /// Start a test-provided job through the same claim and publish path.
    #[cfg(test)]
    fn start_with(
        self: &Arc<Self>,
        id: PairId,
        work: Work,
        run: impl FnOnce() -> (SyncStatus, Option<Reviewed>) + Send + 'static,
    ) -> Result<SyncStatus, RpcError> {
        let acknowledged = self
            .claim(id, work, &Arc::new(AtomicBool::new(false)))?
            .acknowledged;
        self.spawn(id, run);
        Ok(acknowledged)
    }

    /// Overwrite the pair's recorded status, for job state machine tests.
    #[cfg(test)]
    pub(crate) fn force_status(&self, id: PairId, status: SyncStatus) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .jobs
            .insert(
                id,
                Job {
                    status,
                    cancel: Arc::new(AtomicBool::new(false)),
                    reviewed: None,
                    progress: Arc::default(),
                },
            );
    }

    /// Park the next mutating pair command between its busy check and the
    /// engine write. Returns a reached receiver and a release sender.
    #[cfg(test)]
    pub(crate) fn gate_mutation(&self) -> (Receiver<()>, Sender<()>) {
        let (reached, reached_rx) = std::sync::mpsc::channel();
        let (release_tx, release) = std::sync::mpsc::channel();
        *self
            .mutation_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some((reached, release));
        (reached_rx, release_tx)
    }

    /// Wait on the rendezvous armed by `gate_mutation`, if any.
    #[cfg(test)]
    pub(crate) fn wait_mutation_gate(&self) {
        let gate = self
            .mutation_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some((reached, release)) = gate {
            let _ = reached.send(());
            let _ = release.recv();
        }
    }

    /// Reserve the pair for a configuration change (update or removal)
    /// while no job runs. New claims are refused until the returned guard
    /// drops; the reservation is made under the same lock as `claim`, so a
    /// job cannot start between the busy check and the write that follows.
    pub(crate) fn mutation(self: &Arc<Self>, id: PairId) -> Result<Mutation, RpcError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.mutating.contains(&id)
            || matches!(
                state.jobs.get(&id).map(|job| &job.status),
                Some(SyncStatus::Working { .. })
            )
        {
            return Err(RpcError::new(
                SYNC_BUSY,
                "Wait for the current preview or sync to finish",
            ));
        }
        state.mutating.insert(id);
        Ok(Mutation {
            jobs: Arc::clone(self),
            id,
        })
    }

    /// Reserve the pair for `work` while a job runs. Unattended work never
    /// replaces a result waiting for a person (ADR 0022): a stopped run
    /// stays stopped until a manual preview or sync releases the pair.
    fn claim(&self, id: PairId, work: Work, cancel: &Arc<AtomicBool>) -> Result<Claim, RpcError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let blocked = if state.mutating.contains(&id) {
            Some("The pair's configuration is changing")
        } else {
            match state.jobs.get(&id).map(|job| &job.status) {
                Some(SyncStatus::Working { .. }) => {
                    Some("This pair already has a preview or sync in progress")
                }
                // An undo waits too: an unattended run would redo it unseen.
                Some(
                    SyncStatus::Previewed { .. }
                    | SyncStatus::NeedsApproval { .. }
                    | SyncStatus::Undone { .. }
                    | SyncStatus::Failed { .. },
                ) if matches!(work, Work::Scheduled) => {
                    Some("The pair is waiting for review; sync it manually")
                }
                _ => None,
            }
        };
        if let Some(message) = blocked {
            return Err(RpcError::new(SYNC_BUSY, message));
        }
        let progress = Arc::new(Progress::default());
        let acknowledged = SyncStatus::Working {
            job: work.job(),
            cancelling: false,
            progress: SyncProgress::default(),
        };
        // The replaced job's held review is spent by this attempt: it moves
        // to the caller so a run can prove its plan is unchanged, and the
        // job's own result replaces it when published.
        let held = state
            .jobs
            .insert(
                id,
                Job {
                    status: acknowledged.clone(),
                    cancel: Arc::clone(cancel),
                    reviewed: None,
                    progress: Arc::clone(&progress),
                },
            )
            .and_then(|previous| previous.reviewed);
        let skips = state.skips.get(&id).cloned().unwrap_or_default();
        Ok(Claim {
            held,
            skips,
            progress,
            acknowledged,
        })
    }

    fn spend_skips(&self, id: PairId) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .skips
            .remove(&id);
    }

    /// Run `run` on the blocking pool and publish its result. A panic
    /// publishes an explicit failure, never success and never a job that
    /// stays Working, so the pair can take a later job.
    fn spawn(
        self: &Arc<Self>,
        id: PairId,
        run: impl FnOnce() -> (SyncStatus, Option<Reviewed>) + Send + 'static,
    ) {
        let jobs = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let crashed = || {
                (
                    SyncStatus::Failed {
                        message: "The job crashed; see the agent log".into(),
                    },
                    None,
                )
            };
            let (status, reviewed) =
                catch_unwind(AssertUnwindSafe(run)).unwrap_or_else(|_| crashed());
            let mut state = jobs.state.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(job) = state.jobs.get_mut(&id) {
                job.status = status;
                job.reviewed = reviewed;
            }
        });
    }
}

/// Holds a pair's mutation reservation; dropping allows job claims again.
pub(crate) struct Mutation {
    jobs: Arc<Jobs>,
    id: PairId,
}

impl Drop for Mutation {
    fn drop(&mut self) {
        self.jobs
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .mutating
            .remove(&self.id);
    }
}

fn perform(
    engine: &Mutex<Engine>,
    id: PairId,
    work: Work,
    cancel: &AtomicBool,
    held: Option<Reviewed>,
    skips: BTreeMap<SyncPath, Skip>,
    progress: &Progress,
) -> (SyncStatus, Option<Reviewed>) {
    let failed = |message: String| (SyncStatus::Failed { message }, None);
    let stored = match engine.lock() {
        Ok(engine) => engine
            .run_inputs(id)
            .and_then(|inputs| Ok((inputs, engine.pair(id)?, engine.mounts(id)?))),
        Err(_) => return failed("The sync engine failed".into()),
    };
    let (inputs, pair, mounts) = match stored {
        Ok(stored) => stored,
        Err(error) => return failed(error.to_string()),
    };
    let inputs = inputs.with_skips(skips);
    let journal = match work {
        Work::Undo => {
            let engine = engine.lock().unwrap_or_else(PoisonError::into_inner);
            let journal = match engine.journal(id) {
                Ok(Some(journal)) => journal,
                Ok(None) => return failed(RunError::NothingToUndo.to_string()),
                Err(error) => return failed(error.to_string()),
            };
            // The engine holds the pair with its first change.
            Some(journal)
        }
        Work::Preview | Work::Run(..) | Work::Scheduled => None,
    };
    // Someone asked for this run: mount a share that went away. The
    // scheduler makes its own, rate-limited attempts.
    if !matches!(work, Work::Scheduled)
        && let Some(side) = missing_side(&pair, inputs.reach())
        && let Some(source) = mounts.side(side)
    {
        reconnect(&pair, side, source, inputs.reach());
    }
    let mut control = || {
        if cancel.load(Ordering::Relaxed) {
            ScanControl::Cancel
        } else {
            ScanControl::Continue
        }
    };
    let run = match PairRun::prepare(inputs, &mut control, progress) {
        Ok(run) => run,
        // Nothing was written yet.
        Err(RunError::Cancelled) => return (SyncStatus::Idle, None),
        Err(error) => return failed(error.to_string()),
    };
    // Informational, like the snapshots below.
    let sources = run.mount_sources();
    if sources != mounts {
        let _ = engine
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record_mounts(id, run.revision(), &sources);
    }
    // A cache: a failed store only means the next run rereads more data.
    for side in [Side::Left, Side::Right] {
        let snapshot = run.snapshot(side);
        let _ = engine
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .store_snapshot(id, run.revision(), side, &snapshot);
    }
    // A panic elsewhere cannot leave a half-applied transaction, so a
    // poisoned lock still guards a consistent store.
    let mut commit = |revision: u64, commit: Commit<'_>| {
        engine
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .commit_run(id, revision, commit)
    };
    let (approval, context) = match work {
        Work::Undo => {
            let Some(journal) = journal else {
                return failed(RunError::NothingToUndo.to_string());
            };
            return match run.undo(journal, SystemTime::now(), &mut commit, &mut control) {
                Ok(report) => (
                    SyncStatus::Undone {
                        result: undone(&report),
                    },
                    None,
                ),
                Err(error) => failed(error.to_string()),
            };
        }
        Work::Preview => {
            let planned = run.preview();
            let status = SyncStatus::Previewed {
                preview: preview(&planned),
            };
            return (status, Some(Reviewed::from(&planned)));
        }
        Work::Run(approval, token) => {
            let planned = run.preview();
            // ADR 0013: approval binds to the exact reviewed plan. A run is
            // reviewed only when the presented token names the held preview
            // and that plan still equals the fresh one. Any presented token
            // or nonzero approval without both stops with the fresh plan
            // instead of touching either folder.
            let reviewed = held.as_ref().is_some_and(|held| {
                held.plan == planned && token.is_some_and(|token| token == held.token)
            });
            if !reviewed && (token.is_some() || approval.approves_anything()) {
                return (
                    SyncStatus::NeedsApproval {
                        preview: preview(&planned),
                    },
                    Some(Reviewed::from(&planned)),
                );
            }
            (
                approval,
                if reviewed {
                    RunContext::Reviewed
                } else {
                    RunContext::Interactive
                },
            )
        }
        Work::Scheduled => (Approval::default(), RunContext::Unattended),
    };
    match run.execute(
        approval,
        context,
        SystemTime::now(),
        &mut commit,
        &mut control,
        progress,
    ) {
        Ok(RunOutcome::Ran(report)) => {
            // A person's sync that ran ends a hold an undo left; one that
            // stopped before running leaves it.
            if !matches!(work, Work::Scheduled) {
                let _ = engine
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .release_automation(id);
            }
            (
                SyncStatus::Finished {
                    result: result(&report),
                },
                None,
            )
        }
        Ok(RunOutcome::NeedsApproval(planned)) => (
            SyncStatus::NeedsApproval {
                preview: preview(&planned),
            },
            Some(Reviewed::from(&planned)),
        ),
        Err(error) => failed(error.to_string()),
    }
}

fn preview(preview: &Preview) -> SyncPreview {
    let mut counts = SyncCounts::default();
    let mut items = Vec::new();
    // Reserve up to half the response for changes, even during an error storm.
    let reserved_changes = preview.items().len().min(SYNC_ITEM_LIMIT / 2);
    let failure_limit = preview
        .failures()
        .len()
        .min(SYNC_ITEM_LIMIT - reserved_changes);
    let item_limit = SYNC_ITEM_LIMIT - failure_limit;
    for item in preview.items() {
        let action = action(item.action());
        counts.copy_bytes = counts.copy_bytes.saturating_add(item.bytes());
        match action {
            SyncAction::Copy { .. } => counts.copies += 1,
            SyncAction::Update { .. } => counts.updates += 1,
            SyncAction::Archive { .. } => counts.archives += 1,
            SyncAction::Conflict { .. } => counts.conflicts += 1,
            SyncAction::Blocked => counts.blocked += 1,
            SyncAction::Keep => counts.kept += 1,
            SyncAction::Move { .. } => counts.moves += 1,
        }
        if items.len() < item_limit {
            items.push(wire_item(item));
        }
    }
    let deletions = |side: Side| {
        let counts = preview.deletions(side);
        SyncDeletions {
            deletions: counts.deletions(),
            paths: counts.paths(),
            needs_approval: counts.review(RunContext::Interactive)
                == DeletionReview::ConfirmationRequired,
        }
    };
    SyncPreview {
        items,
        counts,
        left: deletions(Side::Left),
        right: deletions(Side::Right),
        review_token: preview.token().to_string(),
        pair_revision: preview.revision(),
        failures: preview
            .failures()
            .iter()
            .take(failure_limit)
            .map(|failure| SyncFailure {
                path: failure.path().as_bytes().to_vec(),
                message: failure.message().to_owned(),
            })
            .collect(),
        failure_count: preview.failures().len() as u64,
    }
}

fn wire_item(item: &PreviewItem) -> SyncItem {
    SyncItem {
        path: item.path().as_bytes().to_vec(),
        action: action(item.action()),
        bytes: item.bytes(),
        moved_from: item.moved_from().map(|path| path.as_bytes().to_vec()),
    }
}

fn action(action: Action) -> SyncAction {
    match action {
        Action::Copy {
            to,
            change: ChangeKind::UpdateMetadata,
        } => SyncAction::Update { to: to.into() },
        Action::Copy { to, .. } => SyncAction::Copy { to: to.into() },
        Action::Archive { on } => SyncAction::Archive { on: on.into() },
        Action::Conflict(kind) => SyncAction::Conflict {
            conflict: match kind {
                ConflictKind::BothChanged => SyncConflict::BothChanged,
                ConflictKind::DeletedVersusEdited => SyncConflict::DeletedVersusEdited,
                ConflictKind::TypeMismatch => SyncConflict::TypeMismatch,
                ConflictKind::MetadataOnly => SyncConflict::MetadataOnly,
                ConflictKind::NameClash => SyncConflict::NameClash,
            },
        },
        Action::Blocked => SyncAction::Blocked,
        Action::KeepTarget => SyncAction::Keep,
        Action::Move { on } => SyncAction::Move { on: on.into() },
    }
}

fn result(report: &RunReport) -> SyncResult {
    SyncResult {
        cancelled: report.completion() == Completion::Cancelled,
        copied: report.copied(),
        moved: report.moved(),
        updated: report.updated(),
        archived: report.archived(),
        recovered: report.recovered(),
        conflicts: report.conflicts().len() as u64,
        failures: report
            .failures()
            .iter()
            .take(SYNC_ITEM_LIMIT)
            .map(|failure| SyncFailure {
                path: failure.path().as_bytes().to_vec(),
                message: failure.message().to_owned(),
            })
            .collect(),
        failure_count: report.failures().len() as u64,
        archive: report.archive().to_owned(),
    }
}

fn undone(report: &UndoReport) -> UndoResult {
    UndoResult {
        cancelled: report.completion() == Completion::Cancelled,
        removed: report.removed(),
        restored: report.restored(),
        beside: report.beside(),
        not_undone: report
            .not_undone()
            .iter()
            .take(SYNC_ITEM_LIMIT)
            .map(|failure| SyncFailure {
                path: failure.path().as_bytes().to_vec(),
                message: failure.message().to_owned(),
            })
            .collect(),
        not_undone_count: report.not_undone().len() as u64,
        archive: report.archive().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use foldover_engine::StateLocation;

    use super::*;

    #[tokio::test]
    async fn a_pair_runs_one_job_at_a_time_and_cancellation_is_reported() {
        let directory = tempfile::tempdir().unwrap();
        let engine =
            Engine::open(&StateLocation::Directory(directory.path().join("state"))).unwrap();
        let engine = Arc::new(Mutex::new(engine));
        let jobs = Arc::new(Jobs::default());
        let id: PairId = "01010101010101010101010101010101".parse().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        jobs.state.lock().unwrap().jobs.insert(
            id,
            Job {
                status: SyncStatus::Working {
                    job: SyncJob::Run,
                    cancelling: false,
                    progress: SyncProgress::default(),
                },
                cancel,
                reviewed: None,
                progress: Arc::default(),
            },
        );
        assert_eq!(
            jobs.start(&engine, id, Work::Preview).unwrap_err().code(),
            SYNC_BUSY
        );
        jobs.cancel(id);
        assert_eq!(
            jobs.status(id),
            SyncStatus::Working {
                job: SyncJob::Run,
                cancelling: true,
                progress: SyncProgress::default(),
            }
        );
    }

    async fn settled(jobs: &Jobs, id: PairId) -> SyncStatus {
        for _ in 0..300 {
            let status = jobs.status(id);
            if !matches!(status, SyncStatus::Working { .. }) {
                return status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("The sync job did not finish")
    }

    fn fixture() -> (tempfile::TempDir, Arc<Mutex<Engine>>, Arc<Jobs>) {
        let directory = tempfile::tempdir().unwrap();
        let engine =
            Engine::open(&StateLocation::Directory(directory.path().join("state"))).unwrap();
        (
            directory,
            Arc::new(Mutex::new(engine)),
            Arc::new(Jobs::default()),
        )
    }

    #[tokio::test]
    async fn a_panicking_job_fails_and_the_pair_accepts_later_jobs() {
        let (_directory, engine, jobs) = fixture();
        let id: PairId = "01010101010101010101010101010101".parse().unwrap();

        jobs.start_with(id, Work::Preview, || panic!("injected failure"))
            .unwrap();
        let status = settled(&jobs, id).await;
        assert!(
            matches!(status, SyncStatus::Failed { .. }),
            "a crash must end Failed, not Working: {status:?}"
        );

        // The panic never touched the engine lock, so the next job runs.
        jobs.start(&engine, id, Work::Preview).unwrap();
        let SyncStatus::Failed { message } = settled(&jobs, id).await else {
            panic!("The follow-up job must reach a terminal state")
        };
        assert_ne!(message, "The sync engine failed");
    }

    #[tokio::test]
    async fn a_panic_holding_the_engine_lock_fails_later_jobs_instead_of_hanging() {
        let (_directory, engine, jobs) = fixture();
        let id: PairId = "01010101010101010101010101010101".parse().unwrap();

        let poisoned = Arc::clone(&engine);
        jobs.start_with(id, Work::Preview, move || {
            let _guard = poisoned.lock().unwrap();
            panic!("injected failure")
        })
        .unwrap();
        let status = settled(&jobs, id).await;
        assert!(matches!(status, SyncStatus::Failed { .. }), "{status:?}");

        // The writable engine may be inconsistent, so its poisoned lock is
        // kept: later jobs fail explicitly rather than reuse it or hang.
        jobs.start(&engine, id, Work::Preview).unwrap();
        assert_eq!(
            settled(&jobs, id).await,
            SyncStatus::Failed {
                message: "The sync engine failed".into()
            }
        );
    }
}
