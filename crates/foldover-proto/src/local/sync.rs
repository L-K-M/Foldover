//! Previews and runs execute as background jobs in the agent; clients start
//! one, then poll `sync.status`. Results stay until the pair's next job.

use serde::{Deserialize, Serialize};

pub const SYNC_PREVIEW: &str = "sync.preview";
pub const SYNC_RUN: &str = "sync.run";
pub const SYNC_STATUS: &str = "sync.status";
pub const SYNC_CANCEL: &str = "sync.cancel";
pub const SYNC_RESOLVE: &str = "sync.resolve";
pub const SYNC_ITEMS: &str = "sync.items";
/// Undo the pair's newest sync that changed anything (ADR 0029).
pub const SYNC_UNDO: &str = "sync.undo";
/// Items per response. Even maximum-length paths, as byte arrays, keep a
/// status below the 1 MiB IPC frame limit.
pub const SYNC_ITEM_LIMIT: usize = 100;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncKey {
    pub id: String,
}

/// Deletions the user accepted after a preview, per side. Approving any
/// deletion — or reviewing a first run that rewrites a target — needs
/// `review_token` from the `SyncPreview` a person saw (ADR 0013).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncRun {
    pub id: String,
    pub approved_left_deletions: u64,
    pub approved_right_deletions: u64,
    /// The `review_token` of the preview being approved. An omitted token
    /// runs only plans that need no review; a stale or foreign one stops
    /// the run with a fresh `NeedsApproval`.
    #[serde(default)]
    pub review_token: Option<String>,
}

/// Keep one side's version of a conflict from the latest preview; the
/// other is archived by the next run. `Both` first saves the right side's
/// version beside the entry, then keeps the left side's.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncResolve {
    pub id: String,
    pub path: Vec<u8>,
    pub keep: ConflictKeep,
    /// The `review_token` of the preview the choice was made from.
    /// Missing or stale fails with `PREVIEW_REQUIRED`.
    #[serde(default)]
    pub review_token: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictKeep {
    Left,
    Right,
    Both,
    /// Leave the change, and everything inside it, alone in the next run.
    Skip,
}

impl From<foldover_types::Side> for ConflictKeep {
    fn from(side: foldover_types::Side) -> Self {
        match side {
            foldover_types::Side::Left => Self::Left,
            foldover_types::Side::Right => Self::Right,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncConflict {
    BothChanged,
    DeletedVersusEdited,
    TypeMismatch,
    MetadataOnly,
    /// Names differing only in case or Unicode spelling that one folder
    /// would store as one file; renaming one resolves it.
    NameClash,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum SyncAction {
    /// New or replaced content goes to `to`.
    Copy {
        to: super::PairSide,
    },
    /// Only synced metadata changes on `to`.
    Update {
        to: super::PairSide,
    },
    /// Moved to `.foldover/archive` on `on`.
    Archive {
        on: super::PairSide,
    },
    Conflict {
        conflict: SyncConflict,
    },
    /// An ancestor's conflict must be resolved first.
    Blocked,
    /// Backup keeps a target-only entry or edit.
    Keep,
    /// Renamed on `on` from the item's `moved_from`: the other folder
    /// moved it, so nothing is copied.
    Move {
        on: super::PairSide,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncItem {
    pub path: Vec<u8>,
    pub action: SyncAction,
    /// File data the item copies; 0 for everything else.
    pub bytes: u64,
    /// For a move, the path it comes from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_from: Option<Vec<u8>>,
}

/// What an item does, for listing one kind at a time.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncItemKind {
    Copy,
    Update,
    Archive,
    Conflict,
    Blocked,
    Keep,
    Move,
}

impl SyncItemKind {
    pub fn of(action: &SyncAction) -> Self {
        match action {
            SyncAction::Copy { .. } => Self::Copy,
            SyncAction::Update { .. } => Self::Update,
            SyncAction::Archive { .. } => Self::Archive,
            SyncAction::Conflict { .. } => Self::Conflict,
            SyncAction::Blocked => Self::Blocked,
            SyncAction::Keep => Self::Keep,
            SyncAction::Move { .. } => Self::Move,
        }
    }
}

/// A page of the latest preview's items, in path order after `after`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncItemQuery {
    pub id: String,
    pub after: Option<Vec<u8>>,
    pub kind: Option<SyncItemKind>,
    /// The `review_token` of the preview being paged: items are shown only
    /// for the plan a client holds. Missing or stale fails with
    /// `PREVIEW_REQUIRED`.
    #[serde(default)]
    pub review_token: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncItemPage {
    pub items: Vec<SyncItem>,
    pub more: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncCounts {
    /// File data all copies move, in bytes.
    pub copy_bytes: u64,
    pub copies: u64,
    pub updates: u64,
    pub archives: u64,
    pub conflicts: u64,
    pub blocked: u64,
    pub kept: u64,
    #[serde(default)]
    pub moves: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncDeletions {
    pub deletions: u64,
    /// Entries on that side the deletions are measured against.
    pub paths: u64,
    /// More deletions than an interactive run makes without approval.
    pub needs_approval: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncPreview {
    /// The first items in path order; `counts` covers all of them.
    /// Items and failures together are bounded by `SYNC_ITEM_LIMIT`.
    pub items: Vec<SyncItem>,
    pub counts: SyncCounts,
    pub left: SyncDeletions,
    pub right: SyncDeletions,
    /// Opaque fingerprint of the whole plan — pair, revision and every
    /// item, not just the listed page. `sync.run`, `sync.resolve` and
    /// `sync.items` present it back to bind to this exact preview.
    pub review_token: String,
    /// The pair revision the plan was built for, so clients can tell a
    /// preview is obsolete after a configuration change without parsing
    /// the token.
    pub pair_revision: u64,
    /// The first entries left unchanged after scan errors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<SyncFailure>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub failure_count: u64,
}

fn is_zero(count: &u64) -> bool {
    *count == 0
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncFailure {
    pub path: Vec<u8>,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncResult {
    pub cancelled: bool,
    pub copied: u64,
    /// Files and links renamed for moves the other folder made.
    #[serde(default)]
    pub moved: u64,
    pub updated: u64,
    pub archived: u64,
    /// Leftovers of interrupted runs, moved to the archive.
    pub recovered: u64,
    pub conflicts: u64,
    /// The first failures; `failure_count` covers all of them.
    pub failures: Vec<SyncFailure>,
    pub failure_count: u64,
    /// The run's folder under `.foldover/archive` on each side.
    pub archive: String,
}

/// What undoing a sync did. Undone paths have their base from before
/// that sync again, so the next preview shows what syncing would redo.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UndoResult {
    pub cancelled: bool,
    /// Entries the sync created, now in this undo's archive folder.
    pub removed: u64,
    /// Entries it replaced or archived, back at their paths.
    pub restored: u64,
    /// Archived entries put back beside a newer entry at their path.
    pub beside: u64,
    /// The first entries left as they were, and why; `not_undone_count`
    /// covers all of them.
    pub not_undone: Vec<SyncFailure>,
    pub not_undone_count: u64,
    /// This undo's folder under `.foldover/archive` on each side.
    pub archive: String,
}

/// How far a working job has got (PLAN.md §3.2: live counters). Counts
/// grow while it works; `changes` and `bytes` stay 0 until its plan is
/// ready.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncProgress {
    /// Entries found in both folders.
    pub listed: u64,
    /// Entries whose version is known.
    pub checked: u64,
    /// Planned changes, and those handled so far.
    pub changes: u64,
    pub applied: u64,
    /// Bytes the changes copy, and those copied so far.
    pub bytes: u64,
    pub copied: u64,
    /// Copy throughput so far, in bytes per second; 0 before the first
    /// copy lands (PLAN.md §3.2: honest throughput).
    #[serde(default)]
    pub rate: u64,
    /// The Mac whose sync of a folder this job waits for, e.g. "Studio".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_for: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncJob {
    Preview,
    Run,
    Undo,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase", deny_unknown_fields)]
pub enum SyncStatus {
    Idle,
    Working {
        job: SyncJob,
        cancelling: bool,
        progress: SyncProgress,
    },
    Previewed {
        preview: SyncPreview,
    },
    /// Too many deletions for this run; nothing was written.
    NeedsApproval {
        preview: SyncPreview,
    },
    Finished {
        result: SyncResult,
    },
    /// The newest sync was undone. Unattended runs wait until a person
    /// syncs, so they do not redo it unseen.
    Undone {
        result: UndoResult,
    },
    Failed {
        message: String,
    },
    /// An unattended run is waiting for this side's folder, e.g. an
    /// unmounted disk; it starts once the folder is back.
    Waiting {
        side: super::PairSide,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::PairSide;

    #[test]
    fn worst_case_statuses_fit_the_frame_limit() {
        let path = vec![u8::MAX; 1024];
        let preview = SyncPreview {
            items: vec![
                // A move names two paths, the most an item carries.
                SyncItem {
                    path: path.clone(),
                    action: SyncAction::Move {
                        on: PairSide::Right,
                    },
                    bytes: u64::MAX,
                    moved_from: Some(path.clone()),
                };
                SYNC_ITEM_LIMIT
            ],
            counts: SyncCounts::default(),
            left: SyncDeletions {
                deletions: u64::MAX,
                paths: u64::MAX,
                needs_approval: true,
            },
            right: SyncDeletions {
                deletions: 0,
                paths: 0,
                needs_approval: false,
            },
            review_token: "ab".repeat(32),
            pair_revision: 1,
            failures: Vec::new(),
            failure_count: 0,
        };
        let status = SyncStatus::NeedsApproval {
            preview: preview.clone(),
        };
        assert!(serde_json::to_vec(&status).unwrap().len() < 1024 * 1024);
        let mut mixed = preview;
        mixed.items.truncate(SYNC_ITEM_LIMIT / 2);
        mixed.failures = vec![
            SyncFailure {
                path: path.clone(),
                message: "x".repeat(1024)
            };
            SYNC_ITEM_LIMIT / 2
        ];
        mixed.failure_count = u64::MAX;
        assert!(
            serde_json::to_vec(&SyncStatus::NeedsApproval { preview: mixed })
                .unwrap()
                .len()
                < 1024 * 1024
        );

        let result = SyncStatus::Finished {
            result: SyncResult {
                cancelled: false,
                copied: 1,
                moved: 0,
                updated: 0,
                archived: 0,
                recovered: 0,
                conflicts: 0,
                failures: vec![
                    SyncFailure {
                        path,
                        message: "x".repeat(1024),
                    };
                    SYNC_ITEM_LIMIT
                ],
                failure_count: u64::MAX,
                archive: "2026-10-18T09-30-00.123Z".into(),
            },
        };
        assert!(serde_json::to_vec(&result).unwrap().len() < 1024 * 1024);
        let copy = SyncAction::Copy {
            to: PairSide::Right,
        };
        assert_eq!(
            serde_json::to_value(copy).unwrap(),
            serde_json::json!({"kind": "copy", "to": "right"})
        );
    }
}
