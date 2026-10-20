//! Watch cursors: the file event before which a pair side's folder had
//! no unsynced change, replayed from when the agent starts (ADR 0002).

use foldover_db::WatchCursorRow;
use foldover_types::{LocalRoot, PairId, Side};

use super::{Error, PairStore, side_name};

/// Where to replay one side's folder from. It holds only while the
/// folder is `root` and its volume keeps the event history `history`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchCursor {
    pub root: LocalRoot,
    pub history: String,
    pub event: u64,
}

const INVALID_EVENT: &str = "watch cursor event";

impl PairStore {
    pub fn watch_cursor(&self, id: PairId, side: Side) -> Result<Option<WatchCursor>, Error> {
        let Some(row) = self.database.watch_cursor(id.as_bytes(), side_name(side))? else {
            return Ok(None);
        };
        Ok(Some(WatchCursor {
            root: LocalRoot::from_bytes(row.root)?,
            history: row.history,
            event: u64::try_from(row.event).map_err(|_| Error::InvalidStored(INVALID_EVENT))?,
        }))
    }

    pub fn save_watch_cursor(
        &mut self,
        id: PairId,
        side: Side,
        cursor: &WatchCursor,
    ) -> Result<(), Error> {
        let row = WatchCursorRow {
            root: cursor.root.as_bytes().to_vec(),
            history: cursor.history.clone(),
            event: i64::try_from(cursor.event).map_err(|_| Error::InvalidStored(INVALID_EVENT))?,
        };
        Ok(self
            .database
            .save_watch_cursor(id.as_bytes(), side_name(side), &row)?)
    }
}
