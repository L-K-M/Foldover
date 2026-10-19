//! Pairs whose automatic syncs wait for a person, e.g. after an undo, so
//! an agent restart does not let a scheduled run redo it unseen. An undo
//! sets the hold with its base changes (`JournalChange::Hold`).

use rusqlite::{OptionalExtension, params};

use super::{Database, Error};

impl Database {
    pub fn release_automation(&mut self, pair: &[u8]) -> Result<(), Error> {
        self.connection.execute(
            "DELETE FROM automation_hold WHERE pair_id=?1",
            params![pair],
        )?;
        Ok(())
    }

    pub fn automation_held(&self, pair: &[u8]) -> Result<bool, Error> {
        Ok(self
            .connection
            .query_row(
                "SELECT 1 FROM automation_hold WHERE pair_id=?1",
                params![pair],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{location, values};
    use super::*;

    #[test]
    fn a_hold_lasts_until_released_and_goes_with_the_pair() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        assert!(!database.automation_held(&pair.id).unwrap());
        let hold = |database: &mut Database| {
            database
                .change_base(&pair.id, pair.revision, &[], crate::JournalChange::Hold)
                .unwrap()
        };
        hold(&mut database);
        hold(&mut database);
        assert!(database.automation_held(&pair.id).unwrap());
        database.release_automation(&pair.id).unwrap();
        assert!(!database.automation_held(&pair.id).unwrap());

        hold(&mut database);
        database.remove_pair(&pair.id, pair.revision).unwrap();
        assert!(!database.automation_held(&pair.id).unwrap());
    }
}
