//! Where each side's network share is mounted from.

use foldover_db::MountRow;
use foldover_types::{PairId, Side};

use super::{Error, PairStore, database_revision};

/// Mount source URLs per side; `None` for local volumes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MountSources {
    pub left: Option<String>,
    pub right: Option<String>,
}

impl MountSources {
    pub fn side(&self, side: Side) -> Option<&str> {
        match side {
            Side::Left => self.left.as_deref(),
            Side::Right => self.right.as_deref(),
        }
    }
}

impl PairStore {
    pub fn mounts(&self, id: PairId) -> Result<MountSources, Error> {
        let row = self.database.mounts(id.as_bytes())?;
        Ok(MountSources {
            left: row.left,
            right: row.right,
        })
    }

    /// Record observed sources, only while the pair keeps `revision`.
    pub fn record_mounts(
        &mut self,
        id: PairId,
        revision: u64,
        sources: &MountSources,
    ) -> Result<(), Error> {
        let row = MountRow {
            left: sources.left.clone(),
            right: sources.right.clone(),
        };
        Ok(self
            .database
            .record_mounts(id.as_bytes(), database_revision(revision)?, &row)?)
    }
}
