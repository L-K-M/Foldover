//! Whether automatic syncs are paused, for the whole agent.

use rusqlite::{OptionalExtension, params};

use super::{Database, Error};

/// A pause; `until_ms` is milliseconds since the Unix epoch, or `None`
/// until resumed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PauseRow {
    pub until_ms: Option<i64>,
}

impl Database {
    pub fn pause(&self) -> Result<Option<PauseRow>, Error> {
        Ok(self
            .connection
            .query_row(
                "SELECT until_ms FROM automation_pause WHERE id=1",
                [],
                |row| {
                    Ok(PauseRow {
                        until_ms: row.get(0)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn set_pause(&mut self, pause: Option<PauseRow>) -> Result<(), Error> {
        match pause {
            Some(pause) => self.connection.execute(
                "INSERT INTO automation_pause (id,until_ms) VALUES (1,?1)
                 ON CONFLICT(id) DO UPDATE SET until_ms=excluded.until_ms",
                params![pause.until_ms],
            )?,
            None => self
                .connection
                .execute("DELETE FROM automation_pause", [])?,
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::location;
    use super::*;

    #[test]
    fn a_pause_is_one_row_until_cleared() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        assert_eq!(database.pause().unwrap(), None);
        database
            .set_pause(Some(PauseRow { until_ms: Some(5) }))
            .unwrap();
        database
            .set_pause(Some(PauseRow { until_ms: None }))
            .unwrap();
        assert_eq!(database.pause().unwrap(), Some(PauseRow { until_ms: None }));
        assert!(
            database
                .set_pause(Some(PauseRow { until_ms: Some(-1) }))
                .is_err()
        );
        database.set_pause(None).unwrap();
        assert_eq!(database.pause().unwrap(), None);
    }
}
