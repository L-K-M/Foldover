//! Sync jobs for Swift: start a preview or run, then poll its state.

use foldover_client::{
    PairSide, ReviewToken, SocketLocation, SyncAction, SyncConflict, SyncDeletions, SyncItemKind,
    SyncItemPage, SyncJob, SyncPreview, SyncProgress, SyncResult, SyncStatus, UndoResult,
};

use crate::{AgentError, invalid_configuration};

/// A review token is the hex fingerprint from a `SyncPreviewSummary`;
/// presenting a malformed one fails before the agent is asked.
fn parse_review_token(token: &str) -> Result<ReviewToken, AgentError> {
    token.parse().map_err(|_| AgentError::Request {
        message: "The review token is not the one shown with a preview".into(),
    })
}

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
    NameClash,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum SyncChangeKind {
    Copy {
        to: SyncSide,
    },
    Update {
        to: SyncSide,
    },
    Archive {
        on: SyncSide,
    },
    Conflict {
        kind: SyncConflictKind,
    },
    Blocked,
    Keep,
    /// Renamed on `on` from `SyncChange::moved_from`.
    Move {
        on: SyncSide,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct SyncChange {
    pub path: Vec<u8>,
    pub kind: SyncChangeKind,
    /// File data the change copies; 0 for everything else.
    pub bytes: u64,
    /// For a move, the path it comes from.
    pub moved_from: Option<Vec<u8>>,
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
    /// File data all copies move, in bytes.
    pub copy_bytes: u64,
    pub copies: u64,
    pub updates: u64,
    pub archives: u64,
    pub conflicts: u64,
    pub blocked: u64,
    pub kept: u64,
    pub moves: u64,
    pub left: SyncSideDeletions,
    pub right: SyncSideDeletions,
    /// Fingerprint of the exact preview shown. Capture it with this
    /// summary and present it back to approve, resolve or page — a plan
    /// that changed since stops the call for a fresh review.
    pub review_token: String,
    /// The pair revision this preview was planned for; a higher stored
    /// revision means the preview is obsolete.
    pub pair_revision: u64,
    pub failures: Vec<SyncFailureItem>,
    pub failure_count: u64,
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
    pub moved: u64,
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
    Undo,
}

/// What undoing the newest sync did; see `foldover_client::UndoResult`.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct SyncUndoSummary {
    pub cancelled: bool,
    /// Entries the sync created, now in this undo's archive folder.
    pub removed: u64,
    /// Entries it replaced or archived, back at their paths.
    pub restored: u64,
    /// Put back beside a newer entry at their path.
    pub beside: u64,
    /// The first entries left as they were, with why.
    pub not_undone: Vec<SyncFailureItem>,
    pub not_undone_count: u64,
    pub archive: String,
}

/// How far a working job has got; see `foldover_client::SyncProgress`.
#[derive(Clone, Debug, Default, Eq, PartialEq, uniffi::Record)]
pub struct SyncWorkProgress {
    pub listed: u64,
    pub checked: u64,
    pub changes: u64,
    pub applied: u64,
    pub bytes: u64,
    pub copied: u64,
    /// Copy throughput so far, in bytes per second; 0 before the first
    /// copy lands.
    pub rate: u64,
    /// The Mac whose sync of a folder this job waits for.
    pub waiting_for: Option<String>,
}

impl From<SyncProgress> for SyncWorkProgress {
    fn from(progress: SyncProgress) -> Self {
        Self {
            listed: progress.listed,
            checked: progress.checked,
            changes: progress.changes,
            applied: progress.applied,
            bytes: progress.bytes,
            copied: progress.copied,
            rate: progress.rate,
            waiting_for: progress.waiting_for,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum SyncJobState {
    Idle,
    Working {
        job: SyncJobKind,
        cancelling: bool,
        progress: SyncWorkProgress,
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
    /// The newest sync was undone; automatic syncs wait for a person.
    Undone {
        result: SyncUndoSummary,
    },
    Failed {
        message: String,
    },
    /// An automatic run waits for this folder, e.g. an unmounted disk.
    Waiting {
        side: SyncSide,
    },
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn sync_preview(id: String) -> Result<SyncJobState, AgentError> {
    let id = id.parse().map_err(invalid_configuration)?;
    state(
        foldover_client::start_preview(&SocketLocation::Default, id).await,
        AgentError::from,
    )
}

/// `review_token` is the `SyncPreviewSummary.review_token` captured with
/// the approval being confirmed; `None` only for runs that need no review.
#[uniffi::export(async_runtime = "tokio")]
pub async fn sync_run(
    id: String,
    approved_left_deletions: u64,
    approved_right_deletions: u64,
    review_token: Option<String>,
) -> Result<SyncJobState, AgentError> {
    let id = id.parse().map_err(invalid_configuration)?;
    let token = review_token
        .as_deref()
        .map(parse_review_token)
        .transpose()?;
    state(
        foldover_client::start_run(
            &SocketLocation::Default,
            id,
            approved_left_deletions,
            approved_right_deletions,
            token,
        )
        .await,
        AgentError::from,
    )
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn sync_status(id: String) -> Result<SyncJobState, AgentError> {
    let id = id.parse().map_err(invalid_configuration)?;
    state(
        foldover_client::sync_status(&SocketLocation::Default, id).await,
        AgentError::read,
    )
}

/// Keep one side's version of a conflict from the pair's latest preview.
/// `review_token` must be the token of the preview the choice was made
/// from.
#[uniffi::export(async_runtime = "tokio")]
pub async fn sync_resolve(
    id: String,
    path: Vec<u8>,
    keep: SyncSide,
    review_token: String,
) -> Result<SyncJobState, AgentError> {
    let id = id.parse().map_err(invalid_configuration)?;
    let token = parse_review_token(&review_token)?;
    state(
        foldover_client::resolve_conflict(&SocketLocation::Default, id, &path, keep.into(), token)
            .await,
        AgentError::from,
    )
}

/// What a change does, for listing one kind at a time.
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum SyncChangeFilter {
    Copy,
    Update,
    Archive,
    Conflict,
    Blocked,
    Keep,
    Move,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct SyncChangePage {
    /// In path order.
    pub changes: Vec<SyncChange>,
    pub more: bool,
}

/// A page of the latest preview's changes after `after`, the last path of
/// the previous page; `filter` keeps one kind. `review_token` must be the
/// token of the preview being paged.
#[uniffi::export(async_runtime = "tokio")]
pub async fn sync_changes(
    id: String,
    after: Option<Vec<u8>>,
    filter: Option<SyncChangeFilter>,
    review_token: String,
) -> Result<SyncChangePage, AgentError> {
    let id = id.parse().map_err(invalid_configuration)?;
    let token = parse_review_token(&review_token)?;
    let kind = filter.map(|filter| match filter {
        SyncChangeFilter::Copy => SyncItemKind::Copy,
        SyncChangeFilter::Update => SyncItemKind::Update,
        SyncChangeFilter::Archive => SyncItemKind::Archive,
        SyncChangeFilter::Conflict => SyncItemKind::Conflict,
        SyncChangeFilter::Blocked => SyncItemKind::Blocked,
        SyncChangeFilter::Keep => SyncItemKind::Keep,
        SyncChangeFilter::Move => SyncItemKind::Move,
    });
    let page =
        foldover_client::sync_items(&SocketLocation::Default, id, token, after.as_deref(), kind)
            .await
            .map_err(AgentError::read)?;
    Ok(page.into())
}

impl From<SyncItemPage> for SyncChangePage {
    fn from(page: SyncItemPage) -> Self {
        Self {
            changes: page
                .items
                .into_iter()
                .map(|item| SyncChange {
                    path: item.path,
                    kind: item.action.into(),
                    bytes: item.bytes,
                    moved_from: item.moved_from,
                })
                .collect(),
            more: page.more,
        }
    }
}

/// Keep both versions of a both-changed conflict: Folder B's is saved
/// beside the entry and Folder A's wins at its path. `review_token` must
/// be the token of the preview the choice was made from.
#[uniffi::export(async_runtime = "tokio")]
pub async fn sync_keep_both(
    id: String,
    path: Vec<u8>,
    review_token: String,
) -> Result<SyncJobState, AgentError> {
    let id = id.parse().map_err(invalid_configuration)?;
    let token = parse_review_token(&review_token)?;
    state(
        foldover_client::keep_both(&SocketLocation::Default, id, &path, token).await,
        AgentError::from,
    )
}

/// Leave a previewed change, and everything inside it, alone in the next
/// run. `review_token` must be the token of the preview it was shown in.
#[uniffi::export(async_runtime = "tokio")]
pub async fn sync_skip_once(
    id: String,
    path: Vec<u8>,
    review_token: String,
) -> Result<SyncJobState, AgentError> {
    let id = id.parse().map_err(invalid_configuration)?;
    let token = parse_review_token(&review_token)?;
    state(
        foldover_client::skip_once(&SocketLocation::Default, id, &path, token).await,
        AgentError::from,
    )
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn sync_cancel(id: String) -> Result<SyncJobState, AgentError> {
    let id = id.parse().map_err(invalid_configuration)?;
    state(
        foldover_client::cancel_sync(&SocketLocation::Default, id).await,
        AgentError::from,
    )
}

/// Start undoing the pair's newest sync that changed anything; poll
/// `sync_status` for the result.
#[uniffi::export(async_runtime = "tokio")]
pub async fn sync_undo(id: String) -> Result<SyncJobState, AgentError> {
    let id = id.parse().map_err(invalid_configuration)?;
    state(
        foldover_client::undo_sync(&SocketLocation::Default, id).await,
        AgentError::from,
    )
}

fn state(
    status: Result<SyncStatus, foldover_client::Error>,
    error: impl FnOnce(foldover_client::Error) -> AgentError,
) -> Result<SyncJobState, AgentError> {
    Ok(status.map_err(error)?.into())
}

impl From<SyncStatus> for SyncJobState {
    fn from(status: SyncStatus) -> Self {
        match status {
            SyncStatus::Idle => Self::Idle,
            SyncStatus::Working {
                job,
                cancelling,
                progress,
            } => Self::Working {
                job: match job {
                    SyncJob::Preview => SyncJobKind::Preview,
                    SyncJob::Run => SyncJobKind::Run,
                    SyncJob::Undo => SyncJobKind::Undo,
                },
                cancelling,
                progress: progress.into(),
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
            SyncStatus::Undone { result } => Self::Undone {
                result: result.into(),
            },
            SyncStatus::Failed { message } => Self::Failed { message },
            SyncStatus::Waiting { side: waiting } => Self::Waiting {
                side: side(waiting),
            },
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
                    bytes: item.bytes,
                    moved_from: item.moved_from,
                })
                .collect(),
            copy_bytes: preview.counts.copy_bytes,
            copies: preview.counts.copies,
            updates: preview.counts.updates,
            archives: preview.counts.archives,
            conflicts: preview.counts.conflicts,
            blocked: preview.counts.blocked,
            kept: preview.counts.kept,
            moves: preview.counts.moves,
            left: preview.left.into(),
            right: preview.right.into(),
            review_token: preview.review_token,
            pair_revision: preview.pair_revision,
            failures: preview
                .failures
                .into_iter()
                .map(|failure| SyncFailureItem {
                    path: failure.path,
                    message: failure.message,
                })
                .collect(),
            failure_count: preview.failure_count,
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
            moved: result.moved,
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

impl From<UndoResult> for SyncUndoSummary {
    fn from(result: UndoResult) -> Self {
        Self {
            cancelled: result.cancelled,
            removed: result.removed,
            restored: result.restored,
            beside: result.beside,
            not_undone: result
                .not_undone
                .into_iter()
                .map(|failure| SyncFailureItem {
                    path: failure.path,
                    message: failure.message,
                })
                .collect(),
            not_undone_count: result.not_undone_count,
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
                    SyncConflict::NameClash => SyncConflictKind::NameClash,
                },
            },
            SyncAction::Blocked => Self::Blocked,
            SyncAction::Keep => Self::Keep,
            SyncAction::Move { on } => Self::Move { on: side(on) },
        }
    }
}

impl From<SyncSide> for foldover_client::Side {
    fn from(side: SyncSide) -> Self {
        match side {
            SyncSide::Left => Self::Left,
            SyncSide::Right => Self::Right,
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
        let SyncJobState::Working { progress, .. } = SyncStatus::Working {
            job: SyncJob::Run,
            cancelling: false,
            progress: SyncProgress {
                rate: 17,
                ..SyncProgress::default()
            },
        }
        .into() else {
            panic!("unexpected state");
        };
        assert_eq!(progress.rate, 17);

        let preview = SyncPreview {
            items: vec![
                SyncItem {
                    path: b"a/\xff".to_vec(),
                    action: SyncAction::Archive { on: PairSide::Left },
                    bytes: 0,
                    moved_from: None,
                },
                SyncItem {
                    path: b"b/moved".to_vec(),
                    action: SyncAction::Move {
                        on: PairSide::Right,
                    },
                    bytes: 0,
                    moved_from: Some(b"a/moved".to_vec()),
                },
            ],
            counts: SyncCounts {
                copy_bytes: 7,
                copies: 1,
                updates: 2,
                archives: 3,
                conflicts: 4,
                blocked: 5,
                kept: 6,
                moves: 8,
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
            review_token: "ab".repeat(32),
            pair_revision: 7,
            failures: vec![SyncFailure {
                path: b"unreadable".to_vec(),
                message: "Permission denied".into(),
            }],
            failure_count: 3,
        };
        let SyncJobState::NeedsApproval { preview } = SyncStatus::NeedsApproval { preview }.into()
        else {
            panic!("unexpected state");
        };
        assert_eq!(
            preview.changes,
            [
                SyncChange {
                    path: b"a/\xff".to_vec(),
                    kind: SyncChangeKind::Archive { on: SyncSide::Left },
                    bytes: 0,
                    moved_from: None,
                },
                SyncChange {
                    path: b"b/moved".to_vec(),
                    kind: SyncChangeKind::Move {
                        on: SyncSide::Right
                    },
                    bytes: 0,
                    moved_from: Some(b"a/moved".to_vec()),
                }
            ]
        );
        assert_eq!(preview.moves, 8);
        assert_eq!(preview.failure_count, 3);
        assert_eq!(preview.failures[0].path, b"unreadable");
        assert_eq!(preview.copy_bytes, 7);
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
                moved: 7,
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
        assert_eq!((result.moved, result.failure_count), (7, 6));
        assert_eq!(result.failures[0].message, "failed");

        let undone: SyncJobState = SyncStatus::Undone {
            result: UndoResult {
                cancelled: false,
                removed: 1,
                restored: 2,
                beside: 3,
                not_undone: vec![SyncFailure {
                    path: b"y/\xff".to_vec(),
                    message: "changed".into(),
                }],
                not_undone_count: 4,
                archive: "undo".into(),
            },
        }
        .into();
        let SyncJobState::Undone { result } = undone else {
            panic!("unexpected state");
        };
        assert_eq!(
            (
                result.removed,
                result.restored,
                result.beside,
                result.not_undone_count
            ),
            (1, 2, 3, 4)
        );
        assert_eq!(result.not_undone[0].path, b"y/\xff");
        assert_eq!(result.archive, "undo");
    }
}
