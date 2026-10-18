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
    Action, Approval, Commit, Completion, DeletionReview, Engine, KeepBothError, PairRun, Preview,
    PreviewItem, RunContext, RunError, RunOutcome, RunReport,
};
use foldover_proto::local::RpcError;
use foldover_proto::local::{
    INTERNAL_ERROR, INVALID_PARAMS, PREVIEW_REQUIRED, ROOT_UNAVAILABLE, SYNC_BUSY, SYNC_ITEM_LIMIT,
    SyncAction, SyncConflict, SyncCounts, SyncDeletions, SyncFailure, SyncItem, SyncItemKind,
    SyncItemPage, SyncJob, SyncPreview, SyncResult, SyncStatus,
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

/// Each side's version of a conflicted path; `None` when absent.
type ConflictVersions = (Option<EntryVersion>, Option<EntryVersion>);

/// A pair's latest preview: every item, for paging, and the conflict
/// versions that choices bind to.
struct Previewed {
    revision: u64,
    versions: HashMap<SyncPath, (ConflictKind, ConflictVersions)>,
    items: Vec<SyncItem>,
}

impl Previewed {
    fn from(preview: &Preview) -> Self {
        Self {
            revision: preview.revision(),
            versions: preview
                .items()
                .iter()
                .filter_map(|item| match item.action() {
                    Action::Conflict(kind) => Some((item.path().clone(), (kind, item.versions()))),
                    _ => None,
                })
                .collect(),
            items: preview.items().iter().map(wire_item).collect(),
        }
    }
}

struct Job {
    status: SyncStatus,
    cancel: Arc<AtomicBool>,
    previewed: Option<Previewed>,
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

    /// Record a choice for a conflict in the pair's latest preview.
    pub(crate) fn resolve(
        &self,
        engine: &Mutex<Engine>,
        id: PairId,
        path: &SyncPath,
        keep: Side,
    ) -> Result<(), RpcError> {
        let (revision, _, (left, right)) = self.conflict(id, path)?;
        engine
            .lock()
            .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))?
            .record_resolution(id, revision, path, Resolution::new(keep, left, right))
            .map_err(crate::engine_error)
    }

    /// Save the right folder's version of a both-changed conflict beside
    /// it, then keep the left folder's at the path; the next run syncs both.
    pub(crate) fn keep_both(
        &self,
        engine: &Mutex<Engine>,
        id: PairId,
        path: &SyncPath,
    ) -> Result<(), RpcError> {
        let (_, kind, (_, right)) = self.conflict(id, path)?;
        let (ConflictKind::BothChanged, Some(right)) = (kind, right) else {
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
        foldover_engine::keep_both(&pair, path, right, &mut || ScanControl::Continue).map_err(
            |error| match error {
                KeepBothError::Changed(_) => RpcError::new(PREVIEW_REQUIRED, error.to_string()),
                error => RpcError::new(ROOT_UNAVAILABLE, error.to_string()),
            },
        )?;
        self.resolve(engine, id, path, Side::Left)
    }

    /// A conflict from the pair's latest preview, while no job works.
    fn conflict(
        &self,
        id: PairId,
        path: &SyncPath,
    ) -> Result<(u64, ConflictKind, ConflictVersions), RpcError> {
        let stale = || {
            RpcError::new(
                PREVIEW_REQUIRED,
                "This conflict is not in the pair's latest preview; preview again",
            )
        };
        let jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        let job = jobs.get(&id).ok_or_else(stale)?;
        if matches!(job.status, SyncStatus::Working { .. }) {
            return Err(RpcError::new(
                SYNC_BUSY,
                "Wait for the current preview or sync to finish",
            ));
        }
        let previewed = job.previewed.as_ref().ok_or_else(stale)?;
        let (kind, versions) = *previewed.versions.get(path).ok_or_else(stale)?;
        Ok((previewed.revision, kind, versions))
    }

    /// A page of the latest preview's items after `after`, optionally of
    /// one kind.
    pub(crate) fn items(
        &self,
        id: PairId,
        after: Option<&[u8]>,
        kind: Option<SyncItemKind>,
    ) -> Result<SyncItemPage, RpcError> {
        let jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        let previewed = jobs
            .get(&id)
            .and_then(|job| job.previewed.as_ref())
            .ok_or_else(|| RpcError::new(PREVIEW_REQUIRED, "Preview the pair first"))?;
        // Items are in path order, so the cursor is a binary search.
        let start = after.map_or(0, |after| {
            previewed
                .items
                .partition_point(|item| item.path.as_slice() <= after)
        });
        let mut matching = previewed.items[start..]
            .iter()
            .filter(|item| kind.is_none_or(|kind| kind == SyncItemKind::of(&item.action)));
        let items: Vec<_> = matching.by_ref().take(SYNC_ITEM_LIMIT).cloned().collect();
        Ok(SyncItemPage {
            items,
            more: matching.next().is_some(),
        })
    }

    /// Show that an unattended run waits for a missing folder. A working
    /// job keeps its status; earlier conflicts stay resolvable.
    pub(crate) fn wait_for(&self, id: PairId, side: Side) {
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        let waiting = SyncStatus::Waiting { side: side.into() };
        match jobs.get_mut(&id) {
            Some(job) if matches!(job.status, SyncStatus::Working { .. }) => {}
            Some(job) => job.status = waiting,
            None => {
                jobs.insert(
                    id,
                    Job {
                        status: waiting,
                        cancel: Arc::new(AtomicBool::new(false)),
                        previewed: None,
                    },
                );
            }
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
                    previewed: None,
                },
            );
        }
        let jobs = Arc::clone(self);
        let engine = Arc::clone(engine);
        tokio::task::spawn_blocking(move || {
            let started = SystemTime::now();
            let (status, previewed) = perform(&engine, id, work, &cancel);
            crate::history::record(&engine, id, work, started, &status);
            let mut jobs = jobs.jobs.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(job) = jobs.get_mut(&id) {
                job.status = status;
                job.previewed = previewed;
            }
        });
        Ok(self.status(id))
    }
}

fn perform(
    engine: &Mutex<Engine>,
    id: PairId,
    work: Work,
    cancel: &AtomicBool,
) -> (SyncStatus, Option<Previewed>) {
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
            return (status, Some(Previewed::from(&planned)));
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
            Some(Previewed::from(&planned)),
        ),
        Err(error) => failed(error.to_string()),
    }
}

fn preview(preview: &Preview) -> SyncPreview {
    let mut counts = SyncCounts::default();
    let mut items = Vec::new();
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
        }
        if items.len() < SYNC_ITEM_LIMIT {
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
    }
}

fn wire_item(item: &PreviewItem) -> SyncItem {
    SyncItem {
        path: item.path().as_bytes().to_vec(),
        action: action(item.action()),
        bytes: item.bytes(),
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
                previewed: None,
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
