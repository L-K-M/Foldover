//! Finished runs per pair, newest first, for the app's history.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use foldover_db::HistoryRow;
use foldover_types::PairId;

use super::{Error, PairStore};

/// Runs kept per pair; older ones are dropped as new ones finish.
pub const HISTORY_LIMIT: usize = 100;

const MANUAL: &str = "manual";
const SCHEDULED: &str = "scheduled";
const UNDO: &str = "undo";
const COMPLETED: &str = "completed";
const CANCELLED: &str = "cancelled";
const NEEDS_APPROVAL: &str = "needs_approval";
const FAILED: &str = "failed";

/// What started a run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunTrigger {
    Manual,
    Scheduled,
    /// An undo of the sync before it: `archived` counts what that sync
    /// created, `recovered` what came back, `failures` what was left.
    Undo,
}

/// How a run ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryOutcome {
    Completed,
    Cancelled,
    /// Stopped before writing: too many deletions to make without asking.
    NeedsApproval,
    Failed,
}

/// One finished run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryEntry {
    pub started: SystemTime,
    pub finished: SystemTime,
    pub trigger: RunTrigger,
    pub outcome: HistoryOutcome,
    pub copied: u64,
    pub updated: u64,
    pub archived: u64,
    pub recovered: u64,
    pub conflicts: u64,
    pub failures: u64,
    /// The run's folder under `.foldover/archive`, when it archived anything.
    pub archive: Option<String>,
    /// Why the run failed.
    pub message: Option<String>,
}

impl PairStore {
    /// Store a finished run, keeping the pair's newest `HISTORY_LIMIT`.
    pub fn record_history(&mut self, id: PairId, entry: &HistoryEntry) -> Result<(), Error> {
        let count = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
        let row = HistoryRow {
            started_ms: milliseconds(entry.started),
            finished_ms: milliseconds(entry.finished).max(milliseconds(entry.started)),
            trigger: match entry.trigger {
                RunTrigger::Manual => MANUAL,
                RunTrigger::Scheduled => SCHEDULED,
                RunTrigger::Undo => UNDO,
            }
            .into(),
            outcome: match entry.outcome {
                HistoryOutcome::Completed => COMPLETED,
                HistoryOutcome::Cancelled => CANCELLED,
                HistoryOutcome::NeedsApproval => NEEDS_APPROVAL,
                HistoryOutcome::Failed => FAILED,
            }
            .into(),
            copied: count(entry.copied),
            updated: count(entry.updated),
            archived: count(entry.archived),
            recovered: count(entry.recovered),
            conflicts: count(entry.conflicts),
            failures: count(entry.failures),
            archive: entry.archive.clone(),
            message: entry.message.clone(),
        };
        Ok(self
            .database
            .record_history(id.as_bytes(), &row, HISTORY_LIMIT)?)
    }

    /// The pair's newest runs first, at most `limit`.
    pub fn history(&self, id: PairId, limit: usize) -> Result<Vec<HistoryEntry>, Error> {
        self.database
            .history(id.as_bytes(), limit)?
            .into_iter()
            .map(entry)
            .collect()
    }
}

fn entry(row: HistoryRow) -> Result<HistoryEntry, Error> {
    let invalid = || Error::InvalidStored("run history");
    let time = |ms: i64| -> Result<SystemTime, Error> {
        let ms = u64::try_from(ms).map_err(|_| invalid())?;
        UNIX_EPOCH
            .checked_add(Duration::from_millis(ms))
            .ok_or_else(invalid)
    };
    let count = |value: i64| u64::try_from(value).map_err(|_| invalid());
    Ok(HistoryEntry {
        started: time(row.started_ms)?,
        finished: time(row.finished_ms)?,
        trigger: match row.trigger.as_str() {
            MANUAL => RunTrigger::Manual,
            SCHEDULED => RunTrigger::Scheduled,
            UNDO => RunTrigger::Undo,
            _ => return Err(invalid()),
        },
        outcome: match row.outcome.as_str() {
            COMPLETED => HistoryOutcome::Completed,
            CANCELLED => HistoryOutcome::Cancelled,
            NEEDS_APPROVAL => HistoryOutcome::NeedsApproval,
            FAILED => HistoryOutcome::Failed,
            _ => return Err(invalid()),
        },
        copied: count(row.copied)?,
        updated: count(row.updated)?,
        archived: count(row.archived)?,
        recovered: count(row.recovered)?,
        conflicts: count(row.conflicts)?,
        failures: count(row.failures)?,
        archive: row.archive,
        message: row.message,
    })
}

/// Milliseconds since the Unix epoch; earlier times count as the epoch.
fn milliseconds(time: SystemTime) -> i64 {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use foldover_types::{LocalRoot, PairDraft, SyncMode};

    use super::super::tests::location;
    use super::*;

    #[test]
    fn entries_round_trip_newest_first() {
        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let draft = PairDraft::new(
            "Docs".into(),
            LocalRoot::from_bytes(b"/left".to_vec()).unwrap(),
            LocalRoot::from_bytes(b"/right".to_vec()).unwrap(),
            SyncMode::Sync,
        )
        .unwrap();
        let pair = store.create(&draft).unwrap();
        let entry = |seconds: u64, outcome| HistoryEntry {
            started: UNIX_EPOCH + Duration::from_secs(seconds),
            finished: UNIX_EPOCH + Duration::from_secs(seconds + 1),
            trigger: RunTrigger::Scheduled,
            outcome,
            copied: 3,
            updated: 1,
            archived: 2,
            recovered: 0,
            conflicts: 1,
            failures: 0,
            archive: Some("2026-10-18T09-30-00.000Z".into()),
            message: None,
        };
        let older = entry(10, HistoryOutcome::Completed);
        let newer = HistoryEntry {
            message: Some("Disk full".into()),
            ..entry(20, HistoryOutcome::Failed)
        };
        store.record_history(pair.id(), &older).unwrap();
        store.record_history(pair.id(), &newer).unwrap();

        assert_eq!(store.history(pair.id(), 10).unwrap(), [newer, older]);
    }
}
