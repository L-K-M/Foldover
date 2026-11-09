//! Removing a pair (ADR 0053). Removal only marks the pair, so it answers
//! at once however many files the pair tracks; every read then treats
//! the pair as gone. `purge_removed_pairs` deletes the marked pairs' rows
//! a batch at a time, so the writer is free between batches, and the
//! pair row last, which takes the marker and the small tables with it.

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{Database, Error, pending};

/// The tables whose rows grow with a pair's files or runs, each with the
/// rest of its primary key after `pair_id`. The others hold a row or two
/// per pair and go with the pair row.
const BULK_TABLES: [(&str, &str); 5] = [
    ("snapshot_entry", "side,path"),
    ("base_entry", "path"),
    ("run_journal", "run,seq"),
    ("resolution", "path"),
    ("run_history", "started_ms"),
];

/// What a purge step left.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Purge {
    /// Rows of removed pairs remain; call again.
    Pending,
    /// No removed pair has rows left.
    Done,
}

impl Database {
    /// Remove a pair: hide it now, delete its rows later. The caller
    /// reserves the pair against jobs first.
    pub fn remove_pair(&mut self, id: &[u8], revision: i64) -> Result<(), Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        pending::check_revision(&transaction, id, revision)?;
        transaction.execute("DELETE FROM pending_pair WHERE id=?1", [id])?;
        transaction.execute("INSERT INTO pair_removal (pair_id) VALUES (?1)", [id])?;
        transaction.commit()?;
        Ok(())
    }

    /// Delete up to `limit` rows of one removed pair in one transaction,
    /// and the pair itself once none are left.
    pub fn purge_removed_pairs(&mut self, limit: usize) -> Result<Purge, Error> {
        let limit = i64::try_from(limit)
            .ok()
            .filter(|limit| *limit > 0)
            .ok_or(Error::InvalidState("Purge limit must be positive"))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(id) = transaction
            .query_row(
                "SELECT pair_id FROM pair_removal ORDER BY pair_id LIMIT 1",
                [],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?
        else {
            return Ok(Purge::Done);
        };
        let mut left = limit;
        for (table, key) in BULK_TABLES {
            if left == 0 {
                break;
            }
            let deleted = transaction.execute(
                &format!(
                    "DELETE FROM {table} WHERE pair_id=?1 AND ({key}) IN \
                     (SELECT {key} FROM {table} WHERE pair_id=?1 LIMIT ?2)"
                ),
                params![id, left],
            )?;
            left -= i64::try_from(deleted).unwrap_or(left);
        }
        // Each key is the rest of its table's primary key, so a delete
        // takes at most `left` rows, and only a step that ran out before
        // its limit emptied every table. The pair row's cascade takes the
        // small tables, and would take any row this missed.
        if left > 0 {
            transaction.execute("DELETE FROM pair WHERE id=?1", [&id])?;
        }
        transaction.commit()?;
        Ok(Purge::Pending)
    }

    /// Whether removed pairs still have rows to purge.
    pub fn has_removed_pairs(&self) -> Result<bool, Error> {
        Ok(self
            .connection
            .query_row("SELECT EXISTS(SELECT 1 FROM pair_removal)", [], |row| {
                row.get(0)
            })?)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{location, put, snapshot_row, values};
    use super::super::{JournalChange, SnapshotRow};
    use super::*;

    fn count(database: &Database, table: &str, pair: &[u8]) -> i64 {
        database
            .connection
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE pair_id=?1"),
                [pair],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn purge_all(database: &mut Database, limit: usize) -> usize {
        let mut steps = 0;
        while database.purge_removed_pairs(limit).unwrap() == Purge::Pending {
            steps += 1;
            assert!(steps < 10_000, "the purge must finish");
        }
        steps
    }

    /// The regression behind ADR 0053: removal used to delete every row
    /// of the pair in its own transaction. It now leaves them for the
    /// purge, and the pair is gone for every reader at once.
    #[test]
    fn removal_hides_the_pair_and_leaves_its_rows_for_the_purge() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let mut other = values();
        other.left_root = b"/other-left".to_vec();
        other.right_root = b"/other-right".to_vec();
        let kept = database.create_pair(&other).unwrap();
        let paths: Vec<Vec<u8>> = (0..25u32)
            .map(|index| format!("file-{index:03}").into_bytes())
            .collect();
        let rows: Vec<SnapshotRow> = paths.iter().map(|path| snapshot_row(path)).collect();
        let changes: Vec<_> = paths.iter().map(|path| put(path, b"1")).collect();
        for id in [&pair.id, &kept.id] {
            database.replace_snapshot(id, 1, "left", &rows).unwrap();
            database
                .change_base(id, 1, &changes, JournalChange::None)
                .unwrap();
        }

        database.remove_pair(&pair.id, pair.revision).unwrap();

        assert!(database.pair(&pair.id).unwrap().is_none());
        assert!(database.active_pair(&pair.id).unwrap().is_none());
        assert!(
            database
                .pairs()
                .unwrap()
                .iter()
                .all(|row| row.id != pair.id)
        );
        assert!(matches!(
            database.remove_pair(&pair.id, pair.revision),
            Err(Error::PairNotFound)
        ));
        assert!(matches!(
            database.update_pair(&pair.id, pair.revision, &values()),
            Err(Error::PairNotFound)
        ));
        assert_eq!(count(&database, "snapshot_entry", &pair.id), 25);
        assert_eq!(count(&database, "base_entry", &pair.id), 25);
        assert!(database.has_removed_pairs().unwrap());

        // Ten rows a step: the snapshot, then the base, then the pair.
        let steps = purge_all(&mut database, 10);
        assert_eq!(steps, 6);
        for table in ["snapshot_entry", "base_entry"] {
            assert_eq!(count(&database, table, &pair.id), 0, "{table}");
            assert_eq!(count(&database, table, &kept.id), 25, "{table}");
        }
        let left: i64 = database
            .connection
            .query_row("SELECT count(*) FROM pair WHERE id=?1", [&pair.id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(left, 0);
        assert!(!database.has_removed_pairs().unwrap());
        assert_eq!(database.purge_removed_pairs(10).unwrap(), Purge::Done);
        assert_eq!(database.pairs().unwrap().len(), 1);
    }

    #[test]
    fn a_removed_pair_with_a_saved_edit_is_gone_too() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let mut renamed = values();
        renamed.name = "Renamed".into();
        let saved = database
            .defer_pair_update(&pair.id, pair.revision, &renamed)
            .unwrap();

        database.remove_pair(&pair.id, saved.revision).unwrap();

        assert!(database.pair(&pair.id).unwrap().is_none());
        assert_eq!(database.purge_removed_pairs(100).unwrap(), Purge::Pending);
        assert_eq!(database.purge_removed_pairs(100).unwrap(), Purge::Done);
    }

    #[test]
    fn a_purge_limit_must_be_positive() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        assert!(matches!(
            database.purge_removed_pairs(0),
            Err(Error::InvalidState(_))
        ));
    }
}
