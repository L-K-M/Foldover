//! How the agent's runs ended, for crash-loop safe mode (PLAN.md §4.2).

use std::time::SystemTime;

use super::{Error, PairStore};

impl PairStore {
    /// Record that the agent starts at `now`, and return the starts that
    /// found the run before them stopped uncleanly, oldest first.
    pub fn note_start(&self, now: SystemTime) -> Result<Vec<SystemTime>, Error> {
        Ok(self.database.note_start(now)?)
    }

    /// The marker `note_start` leaves, to clear at a clean stop without
    /// holding the store.
    pub fn run_marker(&self) -> RunMarker {
        RunMarker(self.database.run_marker())
    }

    /// Forget earlier unclean stops, as resuming after safe mode does.
    pub fn forget_unclean_stops(&self) -> Result<(), Error> {
        Ok(self.database.forget_unclean_stops()?)
    }
}

/// The file that marks a running agent.
#[derive(Clone, Debug)]
pub struct RunMarker(foldover_db::RunMarker);

impl RunMarker {
    /// Record a clean stop: the next start does not count this run.
    pub fn note_clean_stop(&self) -> Result<(), Error> {
        Ok(self.0.note_clean_stop()?)
    }
}
