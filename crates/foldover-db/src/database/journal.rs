//! The newest writing run's operations per pair, kept so that run can be
//! undone. A run's first recorded batch replaces the previous run's
//! entries, unless it continues an interrupted run (ADR 0055): then the
//! journal holds that chain of runs, and undoing spends them all. Entries
//! change only together with the base, in one transaction, and each run
//! keeps the identities of the folders it wrote beside them.

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, Transaction, params};

use super::{Database, Error};

/// One write a run made. Strings are the schema's enumerations; versions
/// use foldover-state's encoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalRow {
    pub seq: i64,
    pub path: Vec<u8>,
    pub side: String,
    pub action: String,
    /// What the run left at the path, or for an archive what it moved
    /// away.
    pub version: Vec<u8>,
    /// The base before the run; None when the path had none. For a move,
    /// the base of `source`, where the entry was.
    pub base: Option<(Vec<u8>, Vec<u8>)>,
    /// For a move, where the entry came from.
    pub source: Option<Vec<u8>>,
}

/// One journaled run: its entries in write order, and the identities of
/// the folders it wrote, left then right, which a run an older agent
/// journaled lacks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalRun {
    pub run: String,
    pub identities: Option<[Vec<u8>; 2]>,
    pub rows: Vec<JournalRow>,
}

/// How a base change touches the journal.
#[derive(Clone, Copy, Debug)]
pub enum JournalChange<'a> {
    None,
    /// Writes of `run`, recorded with the base updates they caused, in
    /// the folders with these identities, left then right.
    Record {
        run: &'a str,
        rows: &'a [JournalRow],
        identities: [&'a [u8]; 2],
    },
    /// Hold the pair's automatic syncs (`automation_hold`).
    Hold,
    /// An undo reverted these entries of `run`; a retry skips them.
    Undid {
        run: &'a str,
        seqs: &'a [i64],
    },
    /// The journal's runs were undone; their entries are spent, and an
    /// interrupted run among them has nothing left to continue.
    Spend,
}

/// The statements that drop every other run's entries when a run's
/// first recorded batch arrives. Each is a range over the PRIMARY KEY
/// (pair_id, run, seq), so both seek instead of scanning every journal
/// row of the pair: the old single `run != ?` delete could not seek on
/// `run` and made every batch of a run quadratic in the run's size.
const REPLACE_OTHER_RUNS: [&str; 2] = [
    "DELETE FROM run_journal WHERE pair_id=?1 AND run<?2",
    "DELETE FROM run_journal WHERE pair_id=?1 AND run>?2",
];
/// The identities of the runs those statements drop: a row per run.
const FORGET_OTHER_RUNS: &str = "DELETE FROM journal_identity WHERE pair_id=?1 AND run!=?2";
/// The identities of the folders a run wrote; each of its batches names
/// the same ones.
const RECORD_IDENTITIES: &str =
    "INSERT INTO journal_identity (pair_id,run,left_identity,right_identity)
     VALUES (?1,?2,?3,?4) ON CONFLICT (pair_id,run) DO NOTHING";

/// Whether the pair's running run continues the journal (`interrupted_run`).
const CONTINUES: &str =
    "SELECT keep_journal FROM interrupted_run WHERE pair_id=?1 AND state='running'";
/// The running run wrote, so a run continuing it keeps its entries.
const WROTE: &str =
    "UPDATE interrupted_run SET wrote=1 WHERE pair_id=?1 AND state='running' AND wrote=0";

pub(super) fn apply(
    transaction: &Transaction<'_>,
    pair: &[u8],
    change: JournalChange<'_>,
) -> Result<(), Error> {
    match change {
        JournalChange::None => Ok(()),
        JournalChange::Hold => {
            transaction.execute(
                "INSERT OR IGNORE INTO automation_hold (pair_id) VALUES (?1)",
                params![pair],
            )?;
            Ok(())
        }
        JournalChange::Undid { run, seqs } => {
            let mut delete = transaction
                .prepare("DELETE FROM run_journal WHERE pair_id=?1 AND run=?2 AND seq=?3")?;
            for seq in seqs {
                delete.execute(params![pair, run, seq])?;
            }
            Ok(())
        }
        JournalChange::Spend => {
            transaction.execute("DELETE FROM run_journal WHERE pair_id=?1", params![pair])?;
            transaction.execute(
                "DELETE FROM journal_identity WHERE pair_id=?1",
                params![pair],
            )?;
            transaction.execute(
                "DELETE FROM interrupted_run WHERE pair_id=?1",
                params![pair],
            )?;
            Ok(())
        }
        JournalChange::Record {
            run,
            rows,
            identities: [left, right],
        } => {
            if rows.is_empty() {
                return Ok(());
            }
            // A run continuing an interrupted one keeps that run's entries,
            // so Undo covers both; the journal then holds only their chain.
            let continues = transaction
                .query_row(CONTINUES, params![pair], |row| row.get::<_, i64>(0))
                .optional()?
                .is_some_and(|keep| keep == 1);
            if !continues {
                for sql in REPLACE_OTHER_RUNS {
                    transaction.execute(sql, params![pair, run])?;
                }
                transaction.execute(FORGET_OTHER_RUNS, params![pair, run])?;
            }
            transaction.execute(RECORD_IDENTITIES, params![pair, run, left, right])?;
            transaction.execute(WROTE, params![pair])?;
            let mut insert = transaction.prepare(
                "INSERT INTO run_journal (pair_id,run,seq,path,side,action,version,base_left,base_right,source)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            )?;
            for row in rows {
                let (left, right) = row.base.as_ref().map(|(l, r)| (l, r)).unzip();
                insert.execute(params![
                    pair,
                    run,
                    row.seq,
                    row.path,
                    row.side,
                    row.action,
                    row.version,
                    left,
                    right,
                    row.source
                ])?;
            }
            Ok(())
        }
    }
}

impl Database {
    /// The pair's journaled runs, oldest first, each with its entries in
    /// write order. Run names begin with their start time, so name order
    /// is start order.
    pub fn journal(&self, pair: &[u8]) -> Result<Vec<JournalRun>, Error> {
        let mut identities: BTreeMap<String, [Vec<u8>; 2]> = self
            .connection
            .prepare(
                "SELECT run,left_identity,right_identity FROM journal_identity WHERE pair_id=?1",
            )?
            .query_map([pair], |row| Ok((row.get(0)?, [row.get(1)?, row.get(2)?])))?
            .collect::<Result<_, _>>()?;
        let mut statement = self.connection.prepare(
            "SELECT run,seq,path,side,action,version,base_left,base_right,source FROM run_journal
             WHERE pair_id=?1 ORDER BY run,seq",
        )?;
        let mut runs: Vec<JournalRun> = Vec::new();
        let mut rows = statement.query([pair])?;
        while let Some(row) = rows.next()? {
            let run: String = row.get(0)?;
            let base = match (row.get(6)?, row.get(7)?) {
                (Some(left), Some(right)) => Some((left, right)),
                _ => None,
            };
            let entry = JournalRow {
                seq: row.get(1)?,
                path: row.get(2)?,
                side: row.get(3)?,
                action: row.get(4)?,
                version: row.get(5)?,
                base,
                source: row.get(8)?,
            };
            match runs.last_mut() {
                Some(last) if last.run == run => last.rows.push(entry),
                _ => runs.push(JournalRun {
                    identities: identities.remove(&run),
                    run,
                    rows: vec![entry],
                }),
            }
        }
        Ok(runs)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{FOLDERS, location, remove_and_purge, values};
    use super::*;
    use crate::BaseChange;

    /// The journal's newest run, as tests of single runs read it.
    fn newest(
        database: &Database,
        pair: &[u8],
    ) -> Result<Option<(String, Vec<JournalRow>)>, Error> {
        Ok(database
            .journal(pair)?
            .pop()
            .map(|newest| (newest.run, newest.rows)))
    }

    fn row(seq: i64, path: &[u8], action: &str) -> JournalRow {
        JournalRow {
            seq,
            path: path.to_vec(),
            side: "right".into(),
            action: action.into(),
            version: vec![seq as u8],
            base: (action != "create").then(|| (vec![1], vec![2])),
            source: (action == "move").then(|| b"from".to_vec()),
        }
    }

    /// The plan SQLite chooses for `sql`, as the semicolon-joined
    /// details of `EXPLAIN QUERY PLAN`.
    fn plan<P: rusqlite::Params>(database: &Database, sql: &str, params: P) -> String {
        let mut statement = database
            .connection
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap();
        statement
            .query_map(params, |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("; ")
    }

    /// The regression guard for the per-batch journal cleanup: the old
    /// single `DELETE ... run!=?` could not seek on `run`, so every batch
    /// of a run scanned all of the pair's journal rows and the run's
    /// commit cost grew quadratically. The replacement deletes are ranges
    /// over the PRIMARY KEY (pair_id, run, seq), one seek per side.
    #[test]
    fn journal_replacement_deletes_seek_the_primary_key() {
        let (_directory, location) = location();
        let database = Database::open(&location).unwrap();
        assert_eq!(
            plan(
                &database,
                "DELETE FROM run_journal WHERE pair_id=?1 AND run!=?2",
                params![b"pair".as_slice(), "run"]
            ),
            "SEARCH run_journal USING PRIMARY KEY (pair_id=?)"
        );
        assert_eq!(
            plan(
                &database,
                REPLACE_OTHER_RUNS[0],
                params![b"pair".as_slice(), "run"]
            ),
            "SEARCH run_journal USING PRIMARY KEY (pair_id=? AND run<?)"
        );
        assert_eq!(
            plan(
                &database,
                REPLACE_OTHER_RUNS[1],
                params![b"pair".as_slice(), "run"]
            ),
            "SEARCH run_journal USING PRIMARY KEY (pair_id=? AND run>?)"
        );
    }

    /// A run's batches accumulate; an empty batch changes nothing; a new
    /// run replaces the previous run's entries whether its id sorts older
    /// or newer; another pair's journal is never touched.
    #[test]
    fn batches_of_a_run_accumulate_and_a_new_run_replaces_only_its_own() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let other = database.create_pair(&values()).unwrap();
        let record = |database: &mut Database, pair: &[u8], run: &str, rows: &[JournalRow]| {
            database.change_base(
                pair,
                1,
                &[],
                JournalChange::Record {
                    run,
                    rows,
                    identities: FOLDERS,
                },
            )
        };
        let first = [row(0, b"a", "create"), row(1, b"b", "create")];
        let second = [row(2, b"c", "replace")];
        record(&mut database, &pair.id, "2026-10-20T10-00-00.000Z", &first).unwrap();
        record(&mut database, &pair.id, "2026-10-20T10-00-00.000Z", &second).unwrap();
        assert_eq!(
            newest(&database, &pair.id).unwrap().unwrap(),
            (
                "2026-10-20T10-00-00.000Z".into(),
                [first[0].clone(), first[1].clone(), second[0].clone()].to_vec()
            )
        );

        // An empty batch, even under a new run id, keeps the journal.
        record(&mut database, &pair.id, "2026-10-20T11-00-00.000Z", &[]).unwrap();
        assert_eq!(newest(&database, &pair.id).unwrap().unwrap().1.len(), 3);

        // Another pair's journal is untouched by either run.
        let other_rows = [row(0, b"x", "create")];
        record(
            &mut database,
            &other.id,
            "2026-10-20T09-00-00.000Z",
            &other_rows,
        )
        .unwrap();
        assert_eq!(
            newest(&database, &other.id).unwrap().unwrap(),
            ("2026-10-20T09-00-00.000Z".into(), other_rows.to_vec())
        );
        assert_eq!(newest(&database, &pair.id).unwrap().unwrap().1.len(), 3);

        // A new run replaces the previous run's entries, and a run whose
        // id sorts before the current one removes the newer run's too.
        let newer = [row(0, b"d", "metadata")];
        record(&mut database, &pair.id, "2026-10-20T12-00-00.000Z", &newer).unwrap();
        assert_eq!(
            newest(&database, &pair.id).unwrap().unwrap(),
            ("2026-10-20T12-00-00.000Z".into(), newer.to_vec())
        );
        let older = [row(0, b"e", "archive")];
        record(&mut database, &pair.id, "2026-10-20T09-00-00.000Z", &older).unwrap();
        assert_eq!(
            newest(&database, &pair.id).unwrap().unwrap(),
            ("2026-10-20T09-00-00.000Z".into(), older.to_vec())
        );
        assert_eq!(
            newest(&database, &other.id).unwrap().unwrap(),
            ("2026-10-20T09-00-00.000Z".into(), other_rows.to_vec())
        );
    }

    /// A batch that fails rolls back with its base changes: the journal
    /// keeps the entries the run already committed.
    #[test]
    fn a_failed_batch_rolls_back_its_base_and_journal() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let run = "2026-10-20T10-00-00.000Z";
        let first = [row(0, b"a", "create")];
        database
            .change_base(
                &pair.id,
                1,
                &[],
                JournalChange::Record {
                    run,
                    rows: &first,
                    identities: FOLDERS,
                },
            )
            .unwrap();

        // A move without its source breaks the schema, so the base change
        // in the same transaction is rolled back with the journal insert.
        let mut invalid = row(1, b"m", "move");
        invalid.source = None;
        assert!(
            database
                .change_base(
                    &pair.id,
                    1,
                    &[BaseChange::Put {
                        path: b"m",
                        left_version: b"1",
                        right_version: b"1",
                    }],
                    JournalChange::Record {
                        run,
                        rows: std::slice::from_ref(&invalid),
                        identities: FOLDERS,
                    },
                )
                .is_err()
        );
        assert!(database.base_rows(&pair.id).unwrap().is_empty());
        assert_eq!(newest(&database, &pair.id).unwrap().unwrap().1, first);

        // An invalid base row runs first and leaves the journal alone.
        assert!(
            database
                .change_base(
                    &pair.id,
                    1,
                    &[BaseChange::Put {
                        path: b"/absolute",
                        left_version: b"1",
                        right_version: b"1",
                    }],
                    JournalChange::Record {
                        run,
                        rows: std::slice::from_ref(&row(2, b"n", "create")),
                        identities: FOLDERS,
                    },
                )
                .is_err()
        );
        assert!(database.base_rows(&pair.id).unwrap().is_empty());
        assert_eq!(newest(&database, &pair.id).unwrap().unwrap().1, first);
    }

    #[test]
    fn moves_keep_their_source_and_nothing_else_has_one() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let record = |database: &mut Database, rows: &[JournalRow]| {
            database.change_base(
                &pair.id,
                1,
                &[],
                JournalChange::Record {
                    run: "2026-10-20T10-00-00.000Z",
                    rows,
                    identities: FOLDERS,
                },
            )
        };
        let moved = row(0, b"new/a", "move");
        record(&mut database, std::slice::from_ref(&moved)).unwrap();
        assert_eq!(newest(&database, &pair.id).unwrap().unwrap().1, [moved]);

        let mut sourceless = row(1, b"b", "move");
        sourceless.source = None;
        assert!(record(&mut database, &[sourceless]).is_err());
        let mut sourced = row(1, b"c", "create");
        sourced.source = Some(b"d".to_vec());
        assert!(record(&mut database, &[sourced]).is_err());
    }

    #[test]
    fn a_new_run_replaces_the_journal_and_an_undo_spends_it() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let put = BaseChange::Put {
            path: b"a",
            left_version: b"1",
            right_version: b"1",
        };
        let first = [row(0, b"a", "create"), row(1, b"b", "archive")];
        database
            .change_base(
                &pair.id,
                1,
                &[put],
                JournalChange::Record {
                    run: "2026-10-19T10-00-00.000Z",
                    rows: &first,
                    identities: FOLDERS,
                },
            )
            .unwrap();
        let more = [row(2, b"c", "replace")];
        database
            .change_base(
                &pair.id,
                1,
                &[],
                JournalChange::Record {
                    run: "2026-10-19T10-00-00.000Z",
                    rows: &more,
                    identities: FOLDERS,
                },
            )
            .unwrap();
        let (run, rows) = newest(&database, &pair.id).unwrap().unwrap();
        assert_eq!(run, "2026-10-19T10-00-00.000Z");
        assert_eq!(rows, [first[0].clone(), first[1].clone(), more[0].clone()]);

        // An undo drops the entries it reverted, and only from that run.
        for run in ["2026-10-19T09-00-00.000Z", "2026-10-19T10-00-00.000Z"] {
            database
                .change_base(
                    &pair.id,
                    1,
                    &[],
                    JournalChange::Undid {
                        run,
                        seqs: &[first[1].seq],
                    },
                )
                .unwrap();
        }
        let (_, rows) = newest(&database, &pair.id).unwrap().unwrap();
        assert_eq!(rows, [first[0].clone(), more[0].clone()]);
        database
            .change_base(
                &pair.id,
                1,
                &[],
                JournalChange::Record {
                    run: "2026-10-19T10-00-00.000Z",
                    rows: &first[1..],
                    identities: FOLDERS,
                },
            )
            .unwrap();

        // A later run that writes nothing keeps the journal; one that
        // writes replaces it.
        database
            .change_base(
                &pair.id,
                1,
                &[],
                JournalChange::Record {
                    run: "2026-10-19T11-00-00.000Z",
                    rows: &[],
                    identities: FOLDERS,
                },
            )
            .unwrap();
        assert_eq!(newest(&database, &pair.id).unwrap().unwrap().1.len(), 3);
        let later = [row(0, b"d", "metadata")];
        database
            .change_base(
                &pair.id,
                1,
                &[],
                JournalChange::Record {
                    run: "2026-10-19T12-00-00.000Z",
                    rows: &later,
                    identities: FOLDERS,
                },
            )
            .unwrap();
        assert_eq!(
            newest(&database, &pair.id).unwrap().unwrap(),
            ("2026-10-19T12-00-00.000Z".into(), later.to_vec())
        );

        // A stale revision changes neither base nor journal.
        assert!(
            database
                .change_base(&pair.id, 2, &[], JournalChange::Spend)
                .is_err()
        );
        database
            .change_base(&pair.id, 1, &[], JournalChange::Spend)
            .unwrap();
        assert_eq!(newest(&database, &pair.id).unwrap(), None);

        // Rows that break the schema's rules are refused.
        let mut invalid = row(0, b"e", "archive");
        invalid.action = "delete".into();
        assert!(
            database
                .change_base(
                    &pair.id,
                    1,
                    &[],
                    JournalChange::Record {
                        run: "r",
                        rows: &[invalid],
                        identities: FOLDERS
                    }
                )
                .is_err()
        );
    }

    /// Each run keeps the identities of the folders it wrote, from its
    /// first batch, while its entries stay: a run replacing them, a spent
    /// journal and changed roots take them too. A run an older agent
    /// journaled has none.
    #[test]
    fn each_run_keeps_the_identities_of_the_folders_it_wrote() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let elsewhere: [&[u8]; 2] = [&[3; 16], &[4; 16]];
        let record = |database: &mut Database, run: &str, seq, identities| {
            database.change_base(
                &pair.id,
                1,
                &[],
                JournalChange::Record {
                    run,
                    rows: &[row(seq, b"a", "create")],
                    identities,
                },
            )
        };
        let journaled = |database: &Database| -> Vec<(String, Option<[Vec<u8>; 2]>)> {
            let runs = database.journal(&pair.id).unwrap();
            runs.into_iter()
                .map(|run| (run.run, run.identities))
                .collect()
        };
        let kept = |database: &Database| -> i64 {
            database
                .connection
                .query_row("SELECT count(*) FROM journal_identity", [], |row| {
                    row.get(0)
                })
                .unwrap()
        };
        let wrote = |identities: [&[u8]; 2]| Some(identities.map(<[u8]>::to_vec));

        record(&mut database, "r1", 0, FOLDERS).unwrap();
        record(&mut database, "r1", 1, elsewhere).unwrap();
        assert_eq!(journaled(&database), [("r1".into(), wrote(FOLDERS))]);
        // Identities that are not identities are refused with the batch.
        assert!(record(&mut database, "r1", 2, [&[1; 15], &[2; 16]]).is_err());
        assert_eq!(newest(&database, &pair.id).unwrap().unwrap().1.len(), 2);

        record(&mut database, "r2", 0, elsewhere).unwrap();
        assert_eq!(journaled(&database), [("r2".into(), wrote(elsewhere))]);
        assert_eq!(kept(&database), 1);
        database
            .change_base(&pair.id, 1, &[], JournalChange::Spend)
            .unwrap();
        assert_eq!(kept(&database), 0);

        database
            .connection
            .execute(
                "INSERT INTO run_journal (pair_id,run,seq,path,side,action,version) VALUES (?1,'r3',0,x'61','right','create',x'00')",
                [&pair.id],
            )
            .unwrap();
        assert_eq!(journaled(&database), [("r3".into(), None)]);

        record(&mut database, "r4", 0, FOLDERS).unwrap();
        let mut moved = values();
        moved.right_root = b"/elsewhere".to_vec();
        let moved = database.update_pair(&pair.id, 1, &moved).unwrap();
        assert!(journaled(&database).is_empty());
        assert_eq!(kept(&database), 0);

        // Removing the pair takes them with it.
        database
            .change_base(
                &pair.id,
                moved.revision,
                &[],
                JournalChange::Record {
                    run: "r5",
                    rows: &[row(0, b"a", "create")],
                    identities: FOLDERS,
                },
            )
            .unwrap();
        assert_eq!(kept(&database), 1);
        remove_and_purge(&mut database, &pair.id, moved.revision);
        assert_eq!(kept(&database), 0);
    }
}
