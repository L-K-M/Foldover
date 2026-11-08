//! Pairs a person paused: their automatic syncs wait until resumed.
//! Unlike the hold after an undo, a manual sync does not lift it.

use rusqlite::{TransactionBehavior, params};

use super::{Database, Error, pending};

impl Database {
    /// Pause the pair's automatic syncs; pausing twice is one pause.
    pub fn pause_pair(&mut self, pair: &[u8]) -> Result<(), Error> {
        self.set_paused(
            pair,
            "INSERT OR IGNORE INTO pair_pause (pair_id) VALUES (?1)",
        )
    }

    /// Resume the pair's automatic syncs; resuming a running pair is a no-op.
    pub fn resume_pair(&mut self, pair: &[u8]) -> Result<(), Error> {
        self.set_paused(pair, "DELETE FROM pair_pause WHERE pair_id=?1")
    }

    fn set_paused(&mut self, pair: &[u8], statement: &str) -> Result<(), Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // A removed pair has no state left to change.
        pending::active_revision(&transaction, pair)?;
        transaction.execute(statement, params![pair])?;
        transaction.commit()?;
        Ok(())
    }

    /// The IDs of every paused pair, removed ones excluded.
    pub fn paused_pairs(&self) -> Result<Vec<Vec<u8>>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT pair_id FROM pair_pause \
             WHERE pair_id NOT IN (SELECT pair_id FROM pair_removal) ORDER BY pair_id",
        )?;
        let rows = statement.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{location, values};
    use super::*;

    #[test]
    fn a_pause_lasts_until_resumed_and_goes_with_the_pair() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        assert!(database.paused_pairs().unwrap().is_empty());

        database.pause_pair(&pair.id).unwrap();
        database.pause_pair(&pair.id).unwrap();
        assert_eq!(
            database.paused_pairs().unwrap(),
            std::slice::from_ref(&pair.id)
        );

        // Editing the pair keeps it paused.
        let mut renamed = values();
        renamed.name = "renamed".into();
        database.update_pair(&pair.id, 1, &renamed).unwrap();
        assert_eq!(
            database.paused_pairs().unwrap(),
            std::slice::from_ref(&pair.id)
        );

        database.resume_pair(&pair.id).unwrap();
        database.resume_pair(&pair.id).unwrap();
        assert!(database.paused_pairs().unwrap().is_empty());

        database.pause_pair(&pair.id).unwrap();
        database.remove_pair(&pair.id, 2).unwrap();
        assert!(database.paused_pairs().unwrap().is_empty());
        assert!(matches!(
            database.resume_pair(&pair.id),
            Err(Error::PairNotFound)
        ));
        assert!(matches!(
            database.pause_pair(&[7; 16]),
            Err(Error::PairNotFound)
        ));
    }
}
