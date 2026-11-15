//! Writing runs the agent admitted, so a run the agent never saw end shows
//! as interrupted after a restart and its successor keeps its journal for
//! Undo (ADR 0055). One row per pair: admitting a run replaces the row of
//! the run before it, so no older run's row outlives a newer one.
//!
//! ```text
//! admit ─▶ running ─finished or cancelled─▶ (deleted)
//!             ├─stopped otherwise─▶ ended
//!             └─agent start finds it─▶ interrupted ─person declines─▶ (deleted)
//! ```
//!
//! A run that continues one which wrote, or which itself continued an
//! earlier one, keeps the journal: its first journal batch leaves the
//! earlier runs' entries in place (`journal::apply`).

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{Database, Error};

const RUNNING: &str = "running";

/// A writing run's row. `trigger` is `manual` or `scheduled`, `resume`
/// `auto` or `manual`; times are milliseconds since the Unix epoch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterruptedRow {
    pub pair: Vec<u8>,
    pub started_ms: i64,
    pub trigger: String,
    pub revision: i64,
    pub resume: String,
    /// Starts that found this run, or a run it continues, still running
    /// after an unclean stop.
    pub unclean: i64,
}

/// How an admitted run ended, as far as continuing it goes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunEnd {
    /// It finished, or a person cancelled it: nothing continues it.
    Spent,
    /// It stopped without finishing, e.g. failed or waiting for approval:
    /// the next run continues its journal, but nothing resumes it.
    Kept,
}

impl Database {
    /// Record that the pair's writing run `row` starts, replacing the row
    /// of the run before it. The new run continues the journal when the
    /// run before it wrote or continued one, unless that journal's oldest
    /// run started before `chain_cutoff_ms`: its archive folder may be
    /// pruned, so Undo could no longer cover it. `row.unclean` is ignored:
    /// a run continuing an interrupted one inherits its count.
    pub fn admit_run(&mut self, row: &InterruptedRow, chain_cutoff_ms: i64) -> Result<(), Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<(i64, String, i64, bool, bool, i64)> = transaction
            .query_row(
                "SELECT started_ms,state,unclean,wrote,keep_journal,chain_ms FROM interrupted_run WHERE pair_id=?1",
                [&row.pair],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get::<_, i64>(3)? == 1,
                        row.get::<_, i64>(4)? == 1,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        let mut keep = false;
        let mut chain_ms = row.started_ms;
        let mut unclean = 0;
        if let Some((started, state, previous_unclean, wrote, kept, previous_chain)) = previous {
            let oldest = match (kept, wrote) {
                (true, _) => Some(previous_chain),
                (false, true) => Some(started),
                (false, false) => None,
            };
            if let Some(oldest) = oldest.filter(|oldest| *oldest >= chain_cutoff_ms) {
                keep = true;
                chain_ms = oldest;
            }
            if state == "interrupted" {
                unclean = previous_unclean;
            }
        }
        transaction.execute(
            "INSERT INTO interrupted_run (pair_id,started_ms,trigger,revision,state,resume,unclean,wrote,keep_journal,chain_ms)
             VALUES (?1,?2,?3,?4,'running',?5,?6,0,?7,?8)
             ON CONFLICT(pair_id) DO UPDATE SET started_ms=excluded.started_ms,trigger=excluded.trigger,
                revision=excluded.revision,state='running',resume=excluded.resume,unclean=excluded.unclean,
                wrote=0,keep_journal=excluded.keep_journal,chain_ms=excluded.chain_ms",
            params![
                row.pair,
                row.started_ms,
                row.trigger,
                row.revision,
                row.resume,
                unclean,
                i64::from(keep),
                chain_ms
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// The pair's running run ended as `end`.
    pub fn end_run(&mut self, pair: &[u8], end: RunEnd) -> Result<(), Error> {
        let sql = match end {
            RunEnd::Spent => "DELETE FROM interrupted_run WHERE pair_id=?1 AND state='running'",
            RunEnd::Kept => {
                "UPDATE interrupted_run SET state='ended' WHERE pair_id=?1 AND state='running'"
            }
        };
        self.connection.execute(sql, [pair])?;
        Ok(())
    }

    /// At an agent start: every run still running was interrupted, counted
    /// as unclean when the agent before this one stopped uncleanly. Returns
    /// the interrupted runs of pairs not being removed.
    pub fn interrupted_at_start(&mut self, unclean: bool) -> Result<Vec<InterruptedRow>, Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE interrupted_run SET state='interrupted',unclean=unclean+?1 WHERE state=?2",
            params![i64::from(unclean), RUNNING],
        )?;
        let rows = {
            let mut statement = transaction.prepare(
                "SELECT pair_id,started_ms,trigger,revision,resume,unclean FROM interrupted_run AS run
                 WHERE state='interrupted'
                   AND NOT EXISTS (SELECT 1 FROM pair_removal WHERE pair_id=run.pair_id)
                 ORDER BY pair_id",
            )?;
            statement
                .query_map([], |row| {
                    Ok(InterruptedRow {
                        pair: row.get(0)?,
                        started_ms: row.get(1)?,
                        trigger: row.get(2)?,
                        revision: row.get(3)?,
                        resume: row.get(4)?,
                        unclean: row.get(5)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        transaction.commit()?;
        Ok(rows)
    }

    /// A person chose not to continue the pair's interrupted run.
    pub fn forget_interrupted(&mut self, pair: &[u8]) -> Result<(), Error> {
        self.connection.execute(
            "DELETE FROM interrupted_run WHERE pair_id=?1 AND state='interrupted'",
            [pair],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{location, remove_and_purge, values};
    use super::*;
    use crate::{JournalChange, JournalRow};

    fn admitted(pair: &[u8], started_ms: i64) -> InterruptedRow {
        InterruptedRow {
            pair: pair.to_vec(),
            started_ms,
            trigger: "scheduled".into(),
            revision: 1,
            resume: "auto".into(),
            unclean: 0,
        }
    }

    fn journal_row(seq: i64) -> JournalRow {
        JournalRow {
            seq,
            path: format!("file-{seq}").into_bytes(),
            side: "right".into(),
            action: "create".into(),
            version: vec![1],
            base: None,
            source: None,
        }
    }

    fn record(database: &mut Database, pair: &[u8], run: &str) {
        let rows = [journal_row(0)];
        database
            .change_base(pair, 1, &[], JournalChange::Record { run, rows: &rows })
            .unwrap();
    }

    fn runs(database: &Database, pair: &[u8]) -> Vec<String> {
        database
            .journal(pair)
            .unwrap()
            .into_iter()
            .map(|(run, _)| run)
            .collect()
    }

    #[test]
    fn a_run_found_running_at_a_start_is_interrupted_once_and_counted_when_unclean() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        database.admit_run(&admitted(&pair.id, 10), 0).unwrap();

        let found = database.interrupted_at_start(true).unwrap();
        assert_eq!(
            found,
            [InterruptedRow {
                unclean: 1,
                ..admitted(&pair.id, 10)
            }]
        );
        // A later clean start still shows it, without counting again.
        assert_eq!(database.interrupted_at_start(false).unwrap()[0].unclean, 1);

        // Continuing it inherits the count; an unclean stop adds to it.
        database.admit_run(&admitted(&pair.id, 20), 0).unwrap();
        assert_eq!(database.interrupted_at_start(false).unwrap()[0].unclean, 1);
        database.admit_run(&admitted(&pair.id, 30), 0).unwrap();
        assert_eq!(database.interrupted_at_start(true).unwrap()[0].unclean, 2);

        database.forget_interrupted(&pair.id).unwrap();
        assert!(database.interrupted_at_start(false).unwrap().is_empty());
    }

    #[test]
    fn ended_runs_are_not_interrupted_and_spent_runs_leave_nothing() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        database.admit_run(&admitted(&pair.id, 10), 0).unwrap();
        database.end_run(&pair.id, RunEnd::Kept).unwrap();
        assert!(database.interrupted_at_start(true).unwrap().is_empty());

        database.admit_run(&admitted(&pair.id, 20), 0).unwrap();
        database.end_run(&pair.id, RunEnd::Spent).unwrap();
        assert!(database.interrupted_at_start(true).unwrap().is_empty());
        let rows: i64 = database
            .connection
            .query_row("SELECT count(*) FROM interrupted_run", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn a_continuing_run_keeps_the_journal_of_the_run_it_continues() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        // A finished run's journal.
        database.admit_run(&admitted(&pair.id, 1), 0).unwrap();
        record(&mut database, &pair.id, "r1");
        database.end_run(&pair.id, RunEnd::Spent).unwrap();

        // The next run replaces it, writes, and is killed.
        database.admit_run(&admitted(&pair.id, 2), 0).unwrap();
        record(&mut database, &pair.id, "r2");
        assert_eq!(runs(&database, &pair.id), ["r2"]);
        database.interrupted_at_start(false).unwrap();

        // Its continuation keeps r2, as does one after a failure that
        // wrote nothing.
        database.admit_run(&admitted(&pair.id, 3), 0).unwrap();
        record(&mut database, &pair.id, "r3");
        database.end_run(&pair.id, RunEnd::Kept).unwrap();
        database.admit_run(&admitted(&pair.id, 4), 0).unwrap();
        database.end_run(&pair.id, RunEnd::Kept).unwrap();
        database.admit_run(&admitted(&pair.id, 5), 0).unwrap();
        record(&mut database, &pair.id, "r5");
        assert_eq!(runs(&database, &pair.id), ["r2", "r3", "r5"]);
        database.end_run(&pair.id, RunEnd::Spent).unwrap();

        // After the chain finishes, the next run starts a fresh journal.
        database.admit_run(&admitted(&pair.id, 6), 0).unwrap();
        record(&mut database, &pair.id, "r6");
        assert_eq!(runs(&database, &pair.id), ["r6"]);
    }

    #[test]
    fn a_run_killed_before_writing_continues_nothing_and_old_chains_lapse() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        database.admit_run(&admitted(&pair.id, 1), 0).unwrap();
        record(&mut database, &pair.id, "r1");
        database.end_run(&pair.id, RunEnd::Spent).unwrap();

        // Killed during its observe: it wrote nothing, so r1 is replaced.
        database.admit_run(&admitted(&pair.id, 2), 0).unwrap();
        database.interrupted_at_start(false).unwrap();
        database.admit_run(&admitted(&pair.id, 3), 0).unwrap();
        record(&mut database, &pair.id, "r3");
        assert_eq!(runs(&database, &pair.id), ["r3"]);
        database.interrupted_at_start(false).unwrap();

        // r3 started before the cutoff: its archive may be pruned.
        database.admit_run(&admitted(&pair.id, 9), 4).unwrap();
        record(&mut database, &pair.id, "r9");
        assert_eq!(runs(&database, &pair.id), ["r9"]);
    }

    #[test]
    fn spending_the_journal_forgets_the_interrupted_run_and_removal_takes_it() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        database.admit_run(&admitted(&pair.id, 1), 0).unwrap();
        record(&mut database, &pair.id, "r1");
        database.interrupted_at_start(false).unwrap();
        database
            .change_base(&pair.id, 1, &[], JournalChange::Spend)
            .unwrap();
        assert!(database.interrupted_at_start(false).unwrap().is_empty());
        assert!(runs(&database, &pair.id).is_empty());

        database.admit_run(&admitted(&pair.id, 2), 0).unwrap();
        database.interrupted_at_start(false).unwrap();
        remove_and_purge(&mut database, &pair.id, pair.revision);
        assert!(database.interrupted_at_start(false).unwrap().is_empty());
    }
}
