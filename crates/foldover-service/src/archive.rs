//! Archive browsing and put-back. Each request reads the pair's
//! configuration under the engine lock, then walks the folder on the
//! blocking pool without it.

use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use foldover_engine::{ArchiveError, ArchiveErrorKind, Engine, PairArchive};
use foldover_proto::local::{
    ARCHIVE_ITEM_LIMIT, ARCHIVE_ITEMS, ARCHIVE_NOT_FOUND, ARCHIVE_PUT_BACK_ALL, ARCHIVE_RUN_LIMIT,
    ARCHIVE_RUNS, ArchiveItem, ArchiveItemQuery, ArchiveItems, ArchiveKey, ArchivePlaced,
    ArchivePutBack, ArchiveRestored, ArchiveRun, ArchiveRunKey, ArchiveRuns, INTERNAL_ERROR,
    INVALID_PARAMS, ROOT_UNAVAILABLE, Request, RpcError, SYNC_BUSY, SyncFailure, SyncStatus,
};
use foldover_types::{PairId, Side, SyncPath};
use serde_json::Value;

use crate::sync::Jobs;
use crate::{encode, parameters};

pub(crate) async fn handle(
    engine: &Arc<Mutex<Engine>>,
    jobs: &Jobs,
    request: &Request,
) -> Result<Value, RpcError> {
    match request.method() {
        ARCHIVE_RUNS => {
            let params: ArchiveKey = parameters(request)?;
            let archive = open(engine, &params.id, params.side.into()).await?;
            let mut runs = blocking(move || archive.runs()).await?;
            let more = runs.len() > ARCHIVE_RUN_LIMIT;
            runs.truncate(ARCHIVE_RUN_LIMIT);
            let runs = runs
                .iter()
                .map(|run| ArchiveRun {
                    name: run.name().to_owned(),
                    started_ms: run.started().duration_since(UNIX_EPOCH).map_or(0, |since| {
                        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
                    }),
                })
                .collect();
            encode(ArchiveRuns { runs, more })
        }
        ARCHIVE_ITEMS => {
            let params: ArchiveItemQuery = parameters(request)?;
            let after = params.after.map(sync_path).transpose()?;
            let archive = open(engine, &params.id, params.side.into()).await?;
            let (items, more) =
                blocking(move || archive.items(&params.run, after.as_ref(), ARCHIVE_ITEM_LIMIT))
                    .await?;
            let items = items
                .iter()
                .map(|item| ArchiveItem {
                    path: item.path().as_bytes().to_vec(),
                    size: item.size(),
                })
                .collect();
            encode(ArchiveItems { items, more })
        }
        ARCHIVE_PUT_BACK_ALL => {
            let params: ArchiveRunKey = parameters(request)?;
            require_idle(jobs, pair_id(&params.id)?)?;
            let archive = open(engine, &params.id, params.side.into()).await?;
            let report = blocking(move || archive.put_back_all(&params.run)).await?;
            encode(ArchiveRestored {
                restored: report.restored(),
                beside: report.beside(),
                failures: report
                    .failures()
                    .iter()
                    .take(ARCHIVE_ITEM_LIMIT)
                    .map(|(path, message)| SyncFailure {
                        path: path.as_bytes().to_vec(),
                        message: message.clone(),
                    })
                    .collect(),
                failure_count: report.failures().len() as u64,
            })
        }
        _ => {
            let params: ArchivePutBack = parameters(request)?;
            let path = sync_path(params.path)?;
            require_idle(jobs, pair_id(&params.id)?)?;
            let archive = open(engine, &params.id, params.side.into()).await?;
            let placed = blocking(move || archive.put_back(&params.run, &path)).await?;
            encode(ArchivePlaced {
                path: placed.as_bytes().to_vec(),
            })
        }
    }
}

/// A put-back never replaces anything, so racing a run cannot lose data;
/// refusing it only keeps the run's preview accurate.
fn require_idle(jobs: &Jobs, id: PairId) -> Result<(), RpcError> {
    if matches!(jobs.status(id), SyncStatus::Working { .. }) {
        return Err(RpcError::new(
            SYNC_BUSY,
            "Wait for the current preview or sync to finish",
        ));
    }
    Ok(())
}

async fn open(engine: &Arc<Mutex<Engine>>, id: &str, side: Side) -> Result<PairArchive, RpcError> {
    let id = pair_id(id)?;
    let engine = Arc::clone(engine);
    let pair = tokio::task::spawn_blocking(move || {
        engine
            .lock()
            .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))?
            .pair(id)
            .map_err(crate::engine_error)
    })
    .await
    .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))??;
    blocking(move || PairArchive::open(&pair, side)).await
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ArchiveError> + Send + 'static,
) -> Result<T, RpcError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| RpcError::new(INTERNAL_ERROR, "Archive worker failed"))?
        .map_err(archive_error)
}

fn archive_error(error: ArchiveError) -> RpcError {
    let code = match error.kind() {
        ArchiveErrorKind::NotFound => ARCHIVE_NOT_FOUND,
        ArchiveErrorKind::Invalid => INVALID_PARAMS,
        ArchiveErrorKind::Unavailable => ROOT_UNAVAILABLE,
    };
    RpcError::new(code, error.to_string())
}

fn pair_id(id: &str) -> Result<PairId, RpcError> {
    id.parse().map_err(|error: foldover_types::InvalidPair| {
        RpcError::new(INVALID_PARAMS, error.to_string())
    })
}

fn sync_path(bytes: Vec<u8>) -> Result<SyncPath, RpcError> {
    SyncPath::try_from(bytes).map_err(|error| RpcError::new(INVALID_PARAMS, error.to_string()))
}
