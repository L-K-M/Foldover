//! Finished runs per pair, newest kept. Rows outlive configuration edits
//! and go with the pair.

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{Database, Error};

/// One finished run. Strings are the schema's enumerations; counts are
/// non-negative.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryRow {
    pub started_ms: i64,
    pub finished_ms: i64,
    pub trigger: String,
    pub outcome: String,
    pub copied: i64,
    pub moved: i64,
    pub updated: i64,
    pub archived: i64,
    pub recovered: i64,
    pub conflicts: i64,
    pub failures: i64,
    pub archive: Option<String>,
    pub message: Option<String>,
}

impl Database {
    /// Store a finished run and drop the pair's rows beyond the newest `keep`.
    pub fn record_history(
        &mut self,
        pair: &[u8],
        row: &HistoryRow,
        keep: usize,
    ) -> Result<(), Error> {
        let keep = i64::try_from(keep).map_err(|_| Error::InvalidState("history limit"))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists = transaction
            .query_row("SELECT 1 FROM pair WHERE id=?1", [pair], |_| Ok(()))
            .optional()?;
        if exists.is_none() {
            return Err(Error::PairNotFound);
        }
        transaction.execute(
            "INSERT OR REPLACE INTO run_history (pair_id,started_ms,finished_ms,trigger,outcome,copied,updated,archived,recovered,conflicts,failures,archive,message,moved)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                pair,
                row.started_ms,
                row.finished_ms,
                row.trigger,
                row.outcome,
                row.copied,
                row.updated,
                row.archived,
                row.recovered,
                row.conflicts,
                row.failures,
                row.archive,
                row.message,
                row.moved
            ],
        )?;
        transaction.execute(
            "DELETE FROM run_history WHERE pair_id=?1 AND started_ms NOT IN
             (SELECT started_ms FROM run_history WHERE pair_id=?1 ORDER BY started_ms DESC LIMIT ?2)",
            params![pair, keep],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// The pair's newest runs first, at most `limit`.
    pub fn history(&self, pair: &[u8], limit: usize) -> Result<Vec<HistoryRow>, Error> {
        let limit = i64::try_from(limit).map_err(|_| Error::InvalidState("history limit"))?;
        let mut statement = self.connection.prepare(
            "SELECT started_ms,finished_ms,trigger,outcome,copied,updated,archived,recovered,conflicts,failures,archive,message,moved
             FROM run_history WHERE pair_id=?1 ORDER BY started_ms DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![pair, limit], |row| {
            Ok(HistoryRow {
                started_ms: row.get(0)?,
                finished_ms: row.get(1)?,
                trigger: row.get(2)?,
                outcome: row.get(3)?,
                copied: row.get(4)?,
                updated: row.get(5)?,
                archived: row.get(6)?,
                recovered: row.get(7)?,
                conflicts: row.get(8)?,
                failures: row.get(9)?,
                archive: row.get(10)?,
                message: row.get(11)?,
                moved: row.get(12)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{location, values};
    use super::*;

    fn row(started_ms: i64) -> HistoryRow {
        HistoryRow {
            started_ms,
            finished_ms: started_ms + 5,
            trigger: "manual".into(),
            outcome: "completed".into(),
            copied: 1,
            moved: 0,
            updated: 0,
            archived: 0,
            recovered: 0,
            conflicts: 0,
            failures: 0,
            archive: None,
            message: None,
        }
    }

    #[test]
    fn history_keeps_the_newest_runs_and_goes_with_the_pair() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        for started in [30, 10, 20] {
            database.record_history(&pair.id, &row(started), 2).unwrap();
        }
        let kept: Vec<_> = database
            .history(&pair.id, 10)
            .unwrap()
            .iter()
            .map(|row| row.started_ms)
            .collect();
        assert_eq!(kept, [30, 20]);
        assert_eq!(database.history(&pair.id, 1).unwrap().len(), 1);

        let mut invalid = row(40);
        invalid.outcome = "maybe".into();
        assert!(database.record_history(&pair.id, &invalid, 2).is_err());
        assert!(matches!(
            database.record_history(b"missing", &row(50), 2),
            Err(Error::PairNotFound)
        ));

        database.remove_pair(&pair.id, pair.revision).unwrap();
        assert!(database.history(&pair.id, 10).unwrap().is_empty());
    }
}
