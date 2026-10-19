//! Browsing one side's archive and putting items back. Each side keeps
//! what runs displaced or removed under `.foldover/archive/<run>`.

use serde::{Deserialize, Serialize};

pub const ARCHIVE_RUNS: &str = "archive.runs";
pub const ARCHIVE_ITEMS: &str = "archive.items";
pub const ARCHIVE_PUT_BACK: &str = "archive.putBack";
pub const ARCHIVE_PUT_BACK_ALL: &str = "archive.putBackAll";
/// Runs per response, newest first. Names are short timestamps.
pub const ARCHIVE_RUN_LIMIT: usize = 1000;
/// Items per response; sized like `SYNC_ITEM_LIMIT` for the frame limit.
pub const ARCHIVE_ITEM_LIMIT: usize = super::SYNC_ITEM_LIMIT;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveKey {
    pub id: String,
    pub side: super::PairSide,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveRun {
    /// The run's folder name, e.g. `2026-10-18T09-30-00.000Z`.
    pub name: String,
    /// When the run started, in milliseconds since the Unix epoch.
    pub started_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveRuns {
    pub runs: Vec<ArchiveRun>,
    /// Older runs exist beyond the limit.
    pub more: bool,
}

/// A page of one run's items, in path order after `after`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveItemQuery {
    pub id: String,
    pub side: super::PairSide,
    pub run: String,
    pub after: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveItem {
    /// The original path in the synced folder.
    pub path: Vec<u8>,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveItems {
    pub items: Vec<ArchiveItem>,
    pub more: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchivePutBack {
    pub id: String,
    pub side: super::PairSide,
    pub run: String,
    pub path: Vec<u8>,
}

/// Every item one run archived on one side.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveRunKey {
    pub id: String,
    pub side: super::PairSide,
    pub run: String,
}

/// What putting back a whole run did; `failures` holds the first 100.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveRestored {
    pub restored: u64,
    /// Put beside a newer entry as `(restored)`.
    pub beside: u64,
    pub failures: Vec<super::SyncFailure>,
    pub failure_count: u64,
}

/// Where the item went: its original path, or a `(restored)` name beside
/// it when that was taken.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchivePlaced {
    pub path: Vec<u8>,
}
