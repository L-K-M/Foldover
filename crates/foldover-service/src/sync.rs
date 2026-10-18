//! Previews and runs as background jobs, at most one per pair. Clients start
//! a job, then poll its status; the result stays until the pair's next job.
//!
//! ```text
//! Idle ─start─▶ Working ─▶ Previewed | NeedsApproval | Finished | Failed
//!                  └─cancel─▶ Idle (before writing) or Finished (cancelled)
//! ```
//!
//! Jobs hold the engine lock only to read inputs and commit progress, so
//! configuration requests stay responsive during long runs.

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use foldover_engine::{
    Action, Approval, Commit, Completion, DeletionReview, Engine, PairRun, Preview, RunContext,
    RunError, RunOutcome, RunReport,
};
use foldover_proto::local::RpcError;
use foldover_proto::local::{
    INTERNAL_ERROR, PREVIEW_REQUIRED, SYNC_BUSY, SYNC_ITEM_LIMIT, SyncAction, SyncConflict,
    SyncCounts, SyncDeletions, SyncFailure, SyncItem, SyncJob, SyncPreview, SyncResult, SyncStatus,
};
use foldover_types::{
    ChangeKind, ConflictKind, EntryVersion, PairId, Resolution, ScanControl, Side, SyncPath,
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Work {
    Preview,
    Run(Approval),
    /// Unattended: stops instead of asking when deletions need approval.
    Scheduled,
}

impl Work {
    fn job(self) -> SyncJob {
        match self {
            Self::Preview => SyncJob::Preview,
            Self::Run(_) | Self::Scheduled => SyncJob::Run,
        }
    }
}

/// Conflict versions from a pair's latest preview; choices bind to them.
struct Conflicts {
    revision: u64,
    versions: HashMap<SyncPath, (Option<EntryVersion>, Option<EntryVersion>)>,
}

impl Conflicts {
    fn from(preview: &Preview) -> Self {
        Self {
            revision: preview.revision(),
            versions: preview
                .items()
                .iter()
                .filter(|item| matches!(item.action(), Action::Conflict(_)))
                .map(|item| (item.path().clone(), item.versions()))
                .collect(),
        }
    }
}

struct Job {
    status: SyncStatus,
    cancel: Arc<AtomicBool>,
    conflicts: Option<Conflicts>,
}

#[derive(Default)]
struct State {
    jobs: HashMap<PairId, Job>,
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
            },
            status => status.clone(),
        }
    }

    /// Record a choice for a conflict in the pair's latest preview.
    pub(crate) fn resolve(
        &self,
        engine: &Mutex<Engine>,
        id: PairId,
        path: &SyncPath,
        keep: Side,
    ) -> Result<(), RpcError> {
        let stale = || {
            RpcError::new(
                PREVIEW_REQUIRED,
                "This conflict is not in the pair's latest preview; preview again",
            )
        };
        let (revision, (left, right)) = {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let job = state.jobs.get(&id).ok_or_else(stale)?;
            if matches!(job.status, SyncStatus::Working { .. }) {
                return Err(RpcError::new(
                    SYNC_BUSY,
                    "Wait for the current preview or sync to finish",
                ));
            }
            let conflicts = job.conflicts.as_ref().ok_or_else(stale)?;
            let versions = *conflicts.versions.get(path).ok_or_else(stale)?;
            (conflicts.revision, versions)
        };
        engine
            .lock()
            .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))?
            .record_resolution(id, revision, path, Resolution::new(keep, left, right))
            .map_err(crate::engine_error)
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
        self.claim(id, work, &cancel)?;
        let engine = Arc::clone(engine);
        self.spawn(id, move || perform(&engine, id, work, &cancel));
        Ok(self.status(id))
    }

    /// Start a test-provided job through the same claim and publish path.
    #[cfg(test)]
    fn start_with(
        self: &Arc<Self>,
        id: PairId,
        work: Work,
        run: impl FnOnce() -> (SyncStatus, Option<Conflicts>) + Send + 'static,
    ) -> Result<SyncStatus, RpcError> {
        self.claim(id, work, &Arc::new(AtomicBool::new(false)))?;
        self.spawn(id, run);
        Ok(self.status(id))
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
                    conflicts: None,
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
    fn claim(&self, id: PairId, work: Work, cancel: &Arc<AtomicBool>) -> Result<(), RpcError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let blocked = if state.mutating.contains(&id) {
            Some("The pair's configuration is changing")
        } else {
            match state.jobs.get(&id).map(|job| &job.status) {
                Some(SyncStatus::Working { .. }) => {
                    Some("This pair already has a preview or sync in progress")
                }
                Some(
                    SyncStatus::Previewed { .. }
                    | SyncStatus::NeedsApproval { .. }
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
        state.jobs.insert(
            id,
            Job {
                status: SyncStatus::Working {
                    job: work.job(),
                    cancelling: false,
                },
                cancel: Arc::clone(cancel),
                conflicts: None,
            },
        );
        Ok(())
    }

    /// Run `run` on the blocking pool and publish its result. A panic
    /// publishes an explicit failure, never success and never a job that
    /// stays Working, so the pair can take a later job.
    fn spawn(
        self: &Arc<Self>,
        id: PairId,
        run: impl FnOnce() -> (SyncStatus, Option<Conflicts>) + Send + 'static,
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
            let (status, conflicts) =
                catch_unwind(AssertUnwindSafe(run)).unwrap_or_else(|_| crashed());
            let mut state = jobs.state.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(job) = state.jobs.get_mut(&id) {
                job.status = status;
                job.conflicts = conflicts;
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
) -> (SyncStatus, Option<Conflicts>) {
    let failed = |message: String| (SyncStatus::Failed { message }, None);
    let inputs = match engine.lock() {
        Ok(engine) => engine.run_inputs(id),
        Err(_) => return failed("The sync engine failed".into()),
    };
    let inputs = match inputs {
        Ok(inputs) => inputs,
        Err(error) => return failed(error.to_string()),
    };
    let mut control = || {
        if cancel.load(Ordering::Relaxed) {
            ScanControl::Cancel
        } else {
            ScanControl::Continue
        }
    };
    let run = match PairRun::prepare(inputs, &mut control) {
        Ok(run) => run,
        // Nothing was written yet.
        Err(RunError::Cancelled) => return (SyncStatus::Idle, None),
        Err(error) => return failed(error.to_string()),
    };
    // A cache: a failed store only means the next run rereads more data.
    for side in [Side::Left, Side::Right] {
        let snapshot = run.snapshot(side);
        let _ = engine
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .store_snapshot(id, run.revision(), side, &snapshot);
    }
    let (approval, context) = match work {
        Work::Preview => {
            let planned = run.preview();
            let status = SyncStatus::Previewed {
                preview: preview(&planned),
            };
            return (status, Some(Conflicts::from(&planned)));
        }
        Work::Run(approval) => (approval, RunContext::Interactive),
        Work::Scheduled => (Approval::default(), RunContext::Unattended),
    };
    // A panic elsewhere cannot leave a half-applied transaction, so a
    // poisoned lock still guards a consistent store.
    let mut commit = |revision: u64, commit: Commit<'_>| {
        engine
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .commit_run(id, revision, commit)
    };
    match run.execute(
        approval,
        context,
        SystemTime::now(),
        &mut commit,
        &mut control,
    ) {
        Ok(RunOutcome::Ran(report)) => (
            SyncStatus::Finished {
                result: result(&report),
            },
            None,
        ),
        Ok(RunOutcome::NeedsApproval(planned)) => (
            SyncStatus::NeedsApproval {
                preview: preview(&planned),
            },
            Some(Conflicts::from(&planned)),
        ),
        Err(error) => failed(error.to_string()),
    }
}

fn preview(preview: &Preview) -> SyncPreview {
    let mut counts = SyncCounts::default();
    let mut items = Vec::new();
    for item in preview.items() {
        let action = action(item.action());
        match action {
            SyncAction::Copy { .. } => counts.copies += 1,
            SyncAction::Update { .. } => counts.updates += 1,
            SyncAction::Archive { .. } => counts.archives += 1,
            SyncAction::Conflict { .. } => counts.conflicts += 1,
            SyncAction::Blocked => counts.blocked += 1,
            SyncAction::Keep => counts.kept += 1,
        }
        if items.len() < SYNC_ITEM_LIMIT {
            items.push(SyncItem {
                path: item.path().as_bytes().to_vec(),
                action,
            });
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
            },
        },
        Action::Blocked => SyncAction::Blocked,
        Action::KeepTarget => SyncAction::Keep,
    }
}

fn result(report: &RunReport) -> SyncResult {
    SyncResult {
        cancelled: report.completion() == Completion::Cancelled,
        copied: report.copied(),
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
                },
                cancel,
                conflicts: None,
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
                cancelling: true
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
