//! Previews and runs execute as background jobs in the agent; clients start
//! one, then poll `sync.status`. Results stay until the pair's next job.

use serde::{Deserialize, Serialize};

pub const SYNC_PREVIEW: &str = "sync.preview";
pub const SYNC_RUN: &str = "sync.run";
pub const SYNC_STATUS: &str = "sync.status";
pub const SYNC_CANCEL: &str = "sync.cancel";
pub const SYNC_RESOLVE: &str = "sync.resolve";
/// Items per response. Even maximum-length paths, as byte arrays, keep a
/// status below the 1 MiB IPC frame limit.
pub const SYNC_ITEM_LIMIT: usize = 100;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncKey {
    pub id: String,
}

/// Deletions the user accepted after a preview, per side.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncRun {
    pub id: String,
    pub approved_left_deletions: u64,
    pub approved_right_deletions: u64,
}

/// Keep one side's version of a conflict from the latest preview; the
/// other is archived by the next run.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncResolve {
    pub id: String,
    pub path: Vec<u8>,
    pub keep: super::PairSide,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncConflict {
    BothChanged,
    DeletedVersusEdited,
    TypeMismatch,
    MetadataOnly,
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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncItem {
    pub path: Vec<u8>,
    pub action: SyncAction,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncCounts {
    pub copies: u64,
    pub updates: u64,
    pub archives: u64,
    pub conflicts: u64,
    pub blocked: u64,
    pub kept: u64,
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
    pub items: Vec<SyncItem>,
    pub counts: SyncCounts,
    pub left: SyncDeletions,
    pub right: SyncDeletions,
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncJob {
    Preview,
    Run,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase", deny_unknown_fields)]
pub enum SyncStatus {
    Idle,
    Working {
        job: SyncJob,
        cancelling: bool,
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
                SyncItem {
                    path: path.clone(),
                    action: SyncAction::Conflict {
                        conflict: SyncConflict::DeletedVersusEdited,
                    },
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
        };
        let status = SyncStatus::NeedsApproval { preview };
        assert!(serde_json::to_vec(&status).unwrap().len() < 1024 * 1024);

        let result = SyncStatus::Finished {
            result: SyncResult {
                cancelled: false,
                copied: 1,
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
