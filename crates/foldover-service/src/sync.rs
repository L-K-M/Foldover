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

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use foldover_engine::{
    Action, Approval, Commit, Completion, DeletionReview, Engine, PairRun, Preview, RunContext,
    RunError, RunOutcome, RunReport,
};
use foldover_proto::local::RpcError;
use foldover_proto::local::{
    SYNC_BUSY, SYNC_ITEM_LIMIT, SyncAction, SyncConflict, SyncCounts, SyncDeletions, SyncFailure,
    SyncItem, SyncJob, SyncPreview, SyncResult, SyncStatus,
};
use foldover_types::{ChangeKind, ConflictKind, PairId, ScanControl, Side};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Work {
    Preview,
    Run(Approval),
}

impl Work {
    fn job(self) -> SyncJob {
        match self {
            Self::Preview => SyncJob::Preview,
            Self::Run(_) => SyncJob::Run,
        }
    }
}

struct Job {
    status: SyncStatus,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub(crate) struct Jobs {
    jobs: Mutex<HashMap<PairId, Job>>,
}

impl Jobs {
    pub(crate) fn status(&self, id: PairId) -> SyncStatus {
        let jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(job) = jobs.get(&id) else {
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

    pub(crate) fn cancel(&self, id: PairId) {
        let jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(job) = jobs.get(&id) {
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
        {
            let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
            if matches!(
                jobs.get(&id).map(|job| &job.status),
                Some(SyncStatus::Working { .. })
            ) {
                return Err(RpcError::new(
                    SYNC_BUSY,
                    "This pair already has a preview or sync in progress",
                ));
            }
            jobs.insert(
                id,
                Job {
                    status: SyncStatus::Working {
                        job: work.job(),
                        cancelling: false,
                    },
                    cancel: Arc::clone(&cancel),
                },
            );
        }
        let jobs = Arc::clone(self);
        let engine = Arc::clone(engine);
        tokio::task::spawn_blocking(move || {
            let status = perform(&engine, id, work, &cancel);
            let mut jobs = jobs.jobs.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(job) = jobs.get_mut(&id) {
                job.status = status;
            }
        });
        Ok(self.status(id))
    }
}

fn perform(engine: &Mutex<Engine>, id: PairId, work: Work, cancel: &AtomicBool) -> SyncStatus {
    let failed = |message: String| SyncStatus::Failed { message };
    let inputs = match engine.lock() {
        Ok(engine) => engine.run_inputs(id),
        Err(_) => return failed("The sync engine failed".into()),
    };
    let (pair, base, identities) = match inputs {
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
    let run = match PairRun::prepare(pair, &base, identities, &mut control) {
        Ok(run) => run,
        // Nothing was written yet.
        Err(RunError::Cancelled) => return SyncStatus::Idle,
        Err(error) => return failed(error.to_string()),
    };
    let approval = match work {
        Work::Preview => {
            return SyncStatus::Previewed {
                preview: preview(&run.preview()),
            };
        }
        Work::Run(approval) => approval,
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
        RunContext::Interactive,
        SystemTime::now(),
        &mut commit,
        &mut control,
    ) {
        Ok(RunOutcome::Ran(report)) => SyncStatus::Finished {
            result: result(&report),
        },
        Ok(RunOutcome::NeedsApproval(preview_data)) => SyncStatus::NeedsApproval {
            preview: preview(&preview_data),
        },
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
        jobs.jobs.lock().unwrap().insert(
            id,
            Job {
                status: SyncStatus::Working {
                    job: SyncJob::Run,
                    cancelling: false,
                },
                cancel,
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
}
