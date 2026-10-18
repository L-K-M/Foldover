//! Sync jobs for Swift: start a preview or run, then poll its state.

use foldover_client::{
    PairSide, SocketLocation, SyncAction, SyncConflict, SyncDeletions, SyncJob, SyncPreview,
    SyncResult, SyncStatus,
};

use crate::{AgentError, invalid_configuration};

#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum SyncSide {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum SyncConflictKind {
    BothChanged,
    DeletedVersusEdited,
    TypeMismatch,
    MetadataOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum SyncChangeKind {
    Copy { to: SyncSide },
    Update { to: SyncSide },
    Archive { on: SyncSide },
    Conflict { kind: SyncConflictKind },
    Blocked,
    Keep,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct SyncChange {
    pub path: Vec<u8>,
    pub kind: SyncChangeKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Record)]
pub struct SyncSideDeletions {
    pub deletions: u64,
    pub paths: u64,
    pub needs_approval: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct SyncPreviewSummary {
    /// The first changes in path order; the counts cover all of them.
    pub changes: Vec<SyncChange>,
    pub copies: u64,
    pub updates: u64,
    pub archives: u64,
    pub conflicts: u64,
    pub blocked: u64,
    pub kept: u64,
    pub left: SyncSideDeletions,
    pub right: SyncSideDeletions,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct SyncFailureItem {
    pub path: Vec<u8>,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct SyncRunSummary {
    pub cancelled: bool,
    pub copied: u64,
    pub updated: u64,
    pub archived: u64,
    pub recovered: u64,
    pub conflicts: u64,
    pub failures: Vec<SyncFailureItem>,
    pub failure_count: u64,
    pub archive: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum SyncJobKind {
    Preview,
    Run,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum SyncJobState {
    Idle,
    Working {
        job: SyncJobKind,
        cancelling: bool,
    },
    Previewed {
        preview: SyncPreviewSummary,
    },
    /// Too many deletions for this run; nothing was written.
    NeedsApproval {
        preview: SyncPreviewSummary,
    },
    Finished {
        result: SyncRunSummary,
    },
    Failed {
        message: String,
    },
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn sync_preview(id: String) -> Result<SyncJobState, AgentError> {
    let id = id.parse().map_err(invalid_configuration)?;
    state(foldover_client::start_preview(&SocketLocation::Default, id).await)
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn sync_run(
    id: String,
    approved_left_deletions: u64,
    approved_right_deletions: u64,
) -> Result<SyncJobState, AgentError> {
    let id = id.parse().map_err(invalid_configuration)?;
    state(
        foldover_client::start_run(
            &SocketLocation::Default,
            id,
            approved_left_deletions,
            approved_right_deletions,
        )
        .await,
    )
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn sync_status(id: String) -> Result<SyncJobState, AgentError> {
    let id = id.parse().map_err(invalid_configuration)?;
    state(foldover_client::sync_status(&SocketLocation::Default, id).await)
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn sync_cancel(id: String) -> Result<SyncJobState, AgentError> {
    let id = id.parse().map_err(invalid_configuration)?;
    state(foldover_client::cancel_sync(&SocketLocation::Default, id).await)
}

fn state(status: Result<SyncStatus, foldover_client::Error>) -> Result<SyncJobState, AgentError> {
    Ok(status.map_err(AgentError::from)?.into())
}

impl From<SyncStatus> for SyncJobState {
    fn from(status: SyncStatus) -> Self {
        match status {
            SyncStatus::Idle => Self::Idle,
            SyncStatus::Working { job, cancelling } => Self::Working {
                job: match job {
                    SyncJob::Preview => SyncJobKind::Preview,
                    SyncJob::Run => SyncJobKind::Run,
                },
                cancelling,
            },
            SyncStatus::Previewed { preview } => Self::Previewed {
                preview: preview.into(),
            },
            SyncStatus::NeedsApproval { preview } => Self::NeedsApproval {
                preview: preview.into(),
            },
            SyncStatus::Finished { result } => Self::Finished {
                result: result.into(),
            },
            SyncStatus::Failed { message } => Self::Failed { message },
        }
    }
}

impl From<SyncPreview> for SyncPreviewSummary {
    fn from(preview: SyncPreview) -> Self {
        Self {
            changes: preview
                .items
                .into_iter()
                .map(|item| SyncChange {
                    path: item.path,
                    kind: item.action.into(),
                })
                .collect(),
            copies: preview.counts.copies,
            updates: preview.counts.updates,
            archives: preview.counts.archives,
            conflicts: preview.counts.conflicts,
            blocked: preview.counts.blocked,
            kept: preview.counts.kept,
            left: preview.left.into(),
            right: preview.right.into(),
        }
    }
}

impl From<SyncDeletions> for SyncSideDeletions {
    fn from(deletions: SyncDeletions) -> Self {
        Self {
            deletions: deletions.deletions,
            paths: deletions.paths,
            needs_approval: deletions.needs_approval,
        }
    }
}

impl From<SyncResult> for SyncRunSummary {
    fn from(result: SyncResult) -> Self {
        Self {
            cancelled: result.cancelled,
            copied: result.copied,
            updated: result.updated,
            archived: result.archived,
            recovered: result.recovered,
            conflicts: result.conflicts,
            failures: result
                .failures
                .into_iter()
                .map(|failure| SyncFailureItem {
                    path: failure.path,
                    message: failure.message,
                })
                .collect(),
            failure_count: result.failure_count,
            archive: result.archive,
        }
    }
}

impl From<SyncAction> for SyncChangeKind {
    fn from(action: SyncAction) -> Self {
        match action {
            SyncAction::Copy { to } => Self::Copy { to: side(to) },
            SyncAction::Update { to } => Self::Update { to: side(to) },
            SyncAction::Archive { on } => Self::Archive { on: side(on) },
            SyncAction::Conflict { conflict } => Self::Conflict {
                kind: match conflict {
                    SyncConflict::BothChanged => SyncConflictKind::BothChanged,
                    SyncConflict::DeletedVersusEdited => SyncConflictKind::DeletedVersusEdited,
                    SyncConflict::TypeMismatch => SyncConflictKind::TypeMismatch,
                    SyncConflict::MetadataOnly => SyncConflictKind::MetadataOnly,
                },
            },
            SyncAction::Blocked => Self::Blocked,
            SyncAction::Keep => Self::Keep,
        }
    }
}

fn side(side: PairSide) -> SyncSide {
    match side {
        PairSide::Left => SyncSide::Left,
        PairSide::Right => SyncSide::Right,
    }
}

#[cfg(test)]
mod tests {
    use foldover_client::{SyncCounts, SyncFailure, SyncItem};

    use super::*;

    #[test]
    fn statuses_keep_every_count_path_byte_and_side() {
        let preview = SyncPreview {
            items: vec![SyncItem {
                path: b"a/\xff".to_vec(),
                action: SyncAction::Archive { on: PairSide::Left },
            }],
            counts: SyncCounts {
                copies: 1,
                updates: 2,
                archives: 3,
                conflicts: 4,
                blocked: 5,
                kept: 6,
            },
            left: SyncDeletions {
                deletions: 3,
                paths: 9,
                needs_approval: true,
            },
            right: SyncDeletions {
                deletions: 0,
                paths: 1,
                needs_approval: false,
            },
        };
        let SyncJobState::NeedsApproval { preview } = SyncStatus::NeedsApproval { preview }.into()
        else {
            panic!("unexpected state");
        };
        assert_eq!(
            preview.changes,
            [SyncChange {
                path: b"a/\xff".to_vec(),
                kind: SyncChangeKind::Archive { on: SyncSide::Left }
            }]
        );
        assert_eq!(
            (
                preview.copies,
                preview.updates,
                preview.archives,
                preview.conflicts
            ),
            (1, 2, 3, 4)
        );
        assert_eq!((preview.blocked, preview.kept), (5, 6));
        assert!(preview.left.needs_approval && !preview.right.needs_approval);

        let finished: SyncJobState = SyncStatus::Finished {
            result: SyncResult {
                cancelled: true,
                copied: 1,
                updated: 2,
                archived: 3,
                recovered: 4,
                conflicts: 5,
                failures: vec![SyncFailure {
                    path: b"x".to_vec(),
                    message: "failed".into(),
                }],
                failure_count: 6,
                archive: "run".into(),
            },
        }
        .into();
        let SyncJobState::Finished { result } = finished else {
            panic!("unexpected state");
        };
        assert!(result.cancelled);
        assert_eq!(result.failure_count, 6);
        assert_eq!(result.failures[0].message, "failed");
    }
}
