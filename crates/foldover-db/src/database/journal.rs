//! The newest writing run's operations per pair, kept so that run can be
//! undone. A run's first recorded batch replaces the previous run's
//! entries; undoing a run spends them. Entries change only together with
//! the base, in one transaction.

use rusqlite::{Transaction, params};

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

/// How a base change touches the journal.
#[derive(Clone, Copy, Debug)]
pub enum JournalChange<'a> {
    None,
    /// Writes of `run`, recorded with the base updates they caused.
    Record {
        run: &'a str,
        rows: &'a [JournalRow],
    },
    /// Hold the pair's automatic syncs (`automation_hold`).
    Hold,
    /// An undo reverted these entries of `run`; a retry skips them.
    Undid {
        run: &'a str,
        seqs: &'a [i64],
    },
    /// `run` was undone; its entries are spent.
    Spend {
        run: &'a str,
    },
}

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
        JournalChange::Spend { run } => {
            transaction.execute(
                "DELETE FROM run_journal WHERE pair_id=?1 AND run=?2",
                params![pair, run],
            )?;
            Ok(())
        }
        JournalChange::Record { run, rows } => {
            if rows.is_empty() {
                return Ok(());
            }
            transaction.execute(
                "DELETE FROM run_journal WHERE pair_id=?1 AND run!=?2",
                params![pair, run],
            )?;
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
    /// The pair's journaled run and its entries in write order, if any.
    pub fn journal(&self, pair: &[u8]) -> Result<Option<(String, Vec<JournalRow>)>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT run,seq,path,side,action,version,base_left,base_right,source FROM run_journal
             WHERE pair_id=?1 AND run=(SELECT max(run) FROM run_journal WHERE pair_id=?1)
             ORDER BY seq",
        )?;
        let mut run = None;
        let rows = statement
            .query_map([pair], |row| {
                run = Some(row.get::<_, String>(0)?);
                let base = match (row.get(6)?, row.get(7)?) {
                    (Some(left), Some(right)) => Some((left, right)),
                    _ => None,
                };
                Ok(JournalRow {
                    seq: row.get(1)?,
                    path: row.get(2)?,
                    side: row.get(3)?,
                    action: row.get(4)?,
                    version: row.get(5)?,
                    base,
                    source: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(run.map(|run| (run, rows)))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{location, values};
    use super::*;
    use crate::BaseChange;

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
                },
            )
        };
        let moved = row(0, b"new/a", "move");
        record(&mut database, std::slice::from_ref(&moved)).unwrap();
        assert_eq!(database.journal(&pair.id).unwrap().unwrap().1, [moved]);

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
                },
            )
            .unwrap();
        let (run, rows) = database.journal(&pair.id).unwrap().unwrap();
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
        let (_, rows) = database.journal(&pair.id).unwrap().unwrap();
        assert_eq!(rows, [first[0].clone(), more[0].clone()]);
        database
            .change_base(
                &pair.id,
                1,
                &[],
                JournalChange::Record {
                    run: "2026-10-19T10-00-00.000Z",
                    rows: &first[1..],
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
                },
            )
            .unwrap();
        assert_eq!(database.journal(&pair.id).unwrap().unwrap().1.len(), 3);
        let later = [row(0, b"d", "metadata")];
        database
            .change_base(
                &pair.id,
                1,
                &[],
                JournalChange::Record {
                    run: "2026-10-19T12-00-00.000Z",
                    rows: &later,
                },
            )
            .unwrap();
        assert_eq!(
            database.journal(&pair.id).unwrap().unwrap(),
            ("2026-10-19T12-00-00.000Z".into(), later.to_vec())
        );

        // A stale revision changes neither base nor journal.
        assert!(
            database
                .change_base(
                    &pair.id,
                    2,
                    &[],
                    JournalChange::Spend {
                        run: "2026-10-19T12-00-00.000Z"
                    }
                )
                .is_err()
        );
        database
            .change_base(
                &pair.id,
                1,
                &[],
                JournalChange::Spend {
                    run: "2026-10-19T12-00-00.000Z",
                },
            )
            .unwrap();
        assert_eq!(database.journal(&pair.id).unwrap(), None);

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
                        rows: &[invalid]
                    }
                )
                .is_err()
        );
    }
}
