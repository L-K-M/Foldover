//! Changes a person chose to leave alone in the pair's next run (PLAN.md
//! §3.8, skip once), kept across agent restarts (ADR 0055). The next run
//! that executes spends them.

use rusqlite::params;

use super::{Database, Error};

/// One skipped change: the versions each side held when it was chosen,
/// in foldover-state's encoding; `None` where the side had none.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkipRow {
    pub path: Vec<u8>,
    pub left_version: Option<Vec<u8>>,
    pub right_version: Option<Vec<u8>>,
}

impl Database {
    /// Store or replace the pair's skip of `row.path`.
    pub fn record_skip(&mut self, pair: &[u8], row: &SkipRow) -> Result<(), Error> {
        self.connection.execute(
            "INSERT INTO pending_skip (pair_id,path,left_version,right_version) VALUES (?1,?2,?3,?4)
             ON CONFLICT(pair_id,path) DO UPDATE SET left_version=excluded.left_version,right_version=excluded.right_version",
            params![pair, row.path, row.left_version, row.right_version],
        )?;
        Ok(())
    }

    /// The pair's skips, in path order.
    pub fn skips(&self, pair: &[u8]) -> Result<Vec<SkipRow>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT path,left_version,right_version FROM pending_skip WHERE pair_id=?1 ORDER BY path",
        )?;
        let rows = statement.query_map([pair], |row| {
            Ok(SkipRow {
                path: row.get(0)?,
                left_version: row.get(1)?,
                right_version: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// A run executed: the pair's skips are spent.
    pub fn spend_skips(&mut self, pair: &[u8]) -> Result<(), Error> {
        self.connection
            .execute("DELETE FROM pending_skip WHERE pair_id=?1", [pair])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{location, remove_and_purge, values};
    use super::*;

    #[test]
    fn skips_last_until_spent_and_go_with_the_pair() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let row = |path: &[u8], left: Option<&[u8]>| SkipRow {
            path: path.to_vec(),
            left_version: left.map(<[u8]>::to_vec),
            right_version: Some(b"r".to_vec()),
        };
        database.record_skip(&pair.id, &row(b"b", None)).unwrap();
        database
            .record_skip(&pair.id, &row(b"a", Some(b"l")))
            .unwrap();
        database
            .record_skip(&pair.id, &row(b"b", Some(b"l2")))
            .unwrap();
        assert_eq!(
            database.skips(&pair.id).unwrap(),
            [row(b"a", Some(b"l")), row(b"b", Some(b"l2"))]
        );
        database.spend_skips(&pair.id).unwrap();
        assert!(database.skips(&pair.id).unwrap().is_empty());

        database.record_skip(&pair.id, &row(b"a", None)).unwrap();
        remove_and_purge(&mut database, &pair.id, pair.revision);
        assert!(database.skips(&pair.id).unwrap().is_empty());
    }
}
