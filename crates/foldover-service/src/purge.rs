//! Deleting removed pairs' rows (ADR 0053). Removal only marks a pair, so
//! it answers at once; this task then deletes the pair's rows a batch at
//! a time, taking the engine lock for each batch only, so requests that
//! queue behind it still answer within milliseconds.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use foldover_engine::{Engine, Purge};
use tokio::sync::Notify;

use crate::Service;

/// Rows one step deletes: about 20 ms of the engine lock on the million-
/// file qualification catalog, where one statement for all of them took
/// four seconds.
const BATCH_ROWS: usize = 10_000;

/// The engine stays free this long between steps, so requests waiting
/// for it go first.
const BREATHER: Duration = Duration::from_millis(5);

/// After a failed pass, the task tries again this much later, or at the
/// next removal, whichever comes first.
const RETRY: Duration = Duration::from_secs(5 * 60);

/// Wakes the purge task. A wake while it runs is kept, so a removal that
/// lands during a pass gets another pass.
#[derive(Default)]
pub(crate) struct Purger {
    wake: Notify,
}

impl Purger {
    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }
}

impl Service {
    /// Delete removed pairs' rows: now, for removals an earlier run left
    /// unfinished, and after every removal. Call once, at start, like
    /// `start_scheduler`.
    pub fn start_purge(&self) {
        let (engine, purger) = (Arc::clone(&self.engine), Arc::clone(&self.purge));
        tokio::spawn(async move {
            loop {
                match purge(&engine).await {
                    Ok(()) => purger.wake.notified().await,
                    Err(error) => {
                        // The rows stay hidden meanwhile.
                        eprintln!("foldoverd: removed pairs' records stay for now: {error}");
                        // Woken by a removal, or timed out: either way, try again.
                        let _ = tokio::time::timeout(RETRY, purger.wake.notified()).await;
                    }
                }
            }
        });
    }
}

/// Run purge steps until no removed pair has rows left.
pub(crate) async fn purge(engine: &Arc<Mutex<Engine>>) -> Result<(), String> {
    loop {
        let engine = Arc::clone(engine);
        let step = tokio::task::spawn_blocking(move || {
            let mut engine = engine
                .lock()
                .map_err(|_| "the sync engine failed".to_owned())?;
            engine
                .purge_removed(BATCH_ROWS)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|_| "the purge step failed".to_owned())??;
        match step {
            Purge::Pending => tokio::time::sleep(BREATHER).await,
            Purge::Done => return Ok(()),
        }
    }
}
