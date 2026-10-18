//! Archive browsing for Swift: one side's runs and items, and put-back.

use foldover_client::{ArchiveItems, ArchiveRuns, SocketLocation};

use crate::{AgentError, SyncSide, invalid_configuration};

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct ArchivedRunRecord {
    /// The run's folder name, e.g. `2026-10-18T09-30-00.000Z`.
    pub name: String,
    /// Milliseconds since the Unix epoch.
    pub started_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct ArchivedRunList {
    /// Newest first.
    pub runs: Vec<ArchivedRunRecord>,
    pub more: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct ArchivedItemRecord {
    /// The original path in the synced folder.
    pub path: Vec<u8>,
    pub size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct ArchivedItemPage {
    /// In path order.
    pub items: Vec<ArchivedItemRecord>,
    pub more: bool,
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn archive_runs(id: String, side: SyncSide) -> Result<ArchivedRunList, AgentError> {
    let id = id.parse().map_err(invalid_configuration)?;
    let runs = foldover_client::archive_runs(&SocketLocation::Default, id, side.into())
        .await
        .map_err(AgentError::from)?;
    Ok(runs.into())
}

/// A page of a run's items after `after`, the last path of the previous page.
#[uniffi::export(async_runtime = "tokio")]
pub async fn archive_items(
    id: String,
    side: SyncSide,
    run: String,
    after: Option<Vec<u8>>,
) -> Result<ArchivedItemPage, AgentError> {
    let id = id.parse().map_err(invalid_configuration)?;
    let items = foldover_client::archive_items(
        &SocketLocation::Default,
        id,
        side.into(),
        &run,
        after.as_deref(),
    )
    .await
    .map_err(AgentError::from)?;
    Ok(items.into())
}

/// Put an item back; returns its path, which gains ` (restored)` when the
/// original path is taken.
#[uniffi::export(async_runtime = "tokio")]
pub async fn archive_put_back(
    id: String,
    side: SyncSide,
    run: String,
    path: Vec<u8>,
) -> Result<Vec<u8>, AgentError> {
    let id = id.parse().map_err(invalid_configuration)?;
    foldover_client::put_back(&SocketLocation::Default, id, side.into(), &run, &path)
        .await
        .map_err(AgentError::from)
}

impl From<ArchiveRuns> for ArchivedRunList {
    fn from(runs: ArchiveRuns) -> Self {
        Self {
            runs: runs
                .runs
                .into_iter()
                .map(|run| ArchivedRunRecord {
                    name: run.name,
                    started_ms: run.started_ms,
                })
                .collect(),
            more: runs.more,
        }
    }
}

impl From<ArchiveItems> for ArchivedItemPage {
    fn from(items: ArchiveItems) -> Self {
        Self {
            items: items
                .items
                .into_iter()
                .map(|item| ArchivedItemRecord {
                    path: item.path,
                    size: item.size,
                })
                .collect(),
            more: items.more,
        }
    }
}
