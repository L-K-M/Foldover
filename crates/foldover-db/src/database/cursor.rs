//! Each pair side's watch cursor: the file event its folder had no
//! unsynced change before, with the volume's event history ID and the
//! root it was taken for. A cursor for another root or history is stale.

use rusqlite::{OptionalExtension, params};

use super::{Database, Error};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchCursorRow {
    pub root: Vec<u8>,
    pub history: String,
    pub event: i64,
}

impl Database {
    pub fn save_watch_cursor(
        &mut self,
        pair: &[u8],
        side: &str,
        cursor: &WatchCursorRow,
    ) -> Result<(), Error> {
        self.connection.execute(
            "INSERT INTO watch_cursor (pair_id, side, root, history, event) VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(pair_id, side) DO UPDATE SET root=excluded.root, history=excluded.history, event=excluded.event",
            params![pair, side, cursor.root, cursor.history, cursor.event],
        )?;
        Ok(())
    }

    pub fn watch_cursor(&self, pair: &[u8], side: &str) -> Result<Option<WatchCursorRow>, Error> {
        Ok(self
            .connection
            .query_row(
                "SELECT root, history, event FROM watch_cursor WHERE pair_id=?1 AND side=?2",
                params![pair, side],
                |row| {
                    Ok(WatchCursorRow {
                        root: row.get(0)?,
                        history: row.get(1)?,
                        event: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{location, values};
    use super::*;

    #[test]
    fn a_cursor_is_replaced_per_side_and_goes_with_the_pair() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        assert_eq!(database.watch_cursor(&pair.id, "left").unwrap(), None);

        let cursor = |event| WatchCursorRow {
            root: b"/Volumes/Photos".to_vec(),
            history: "5C3E…".into(),
            event,
        };
        database
            .save_watch_cursor(&pair.id, "left", &cursor(7))
            .unwrap();
        database
            .save_watch_cursor(&pair.id, "left", &cursor(9))
            .unwrap();
        assert_eq!(
            database.watch_cursor(&pair.id, "left").unwrap(),
            Some(cursor(9))
        );
        assert_eq!(database.watch_cursor(&pair.id, "right").unwrap(), None);
        assert!(
            database
                .save_watch_cursor(&pair.id, "middle", &cursor(1))
                .is_err()
        );

        database.remove_pair(&pair.id, pair.revision).unwrap();
        assert_eq!(database.watch_cursor(&pair.id, "left").unwrap(), None);
    }
}
