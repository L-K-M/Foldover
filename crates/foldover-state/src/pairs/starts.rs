//! How the agent's runs ended, for crash-loop safe mode (PLAN.md §4.2).

use std::time::SystemTime;

use super::{Error, PairStore};

impl PairStore {
    /// Record that the agent starts at `now`, and return the starts that
    /// found the run before them stopped uncleanly, oldest first.
    pub fn note_start(&self, now: SystemTime) -> Result<Vec<SystemTime>, Error> {
        Ok(self.database.note_start(now)?)
    }

    pub fn note_clean_stop(&self) -> Result<(), Error> {
        Ok(self.database.note_clean_stop()?)
    }

    /// Forget earlier unclean stops, as resuming after safe mode does.
    pub fn forget_unclean_stops(&self) -> Result<(), Error> {
        Ok(self.database.forget_unclean_stops()?)
    }
}
