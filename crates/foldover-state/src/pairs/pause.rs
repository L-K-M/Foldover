//! Pausing automatic syncs for every pair. Manual syncs still run.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use foldover_db::PauseRow;

use super::{Error, PairStore};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pause {
    Off,
    UntilResumed,
    Until(SystemTime),
}

impl Pause {
    /// Whether automatic syncs wait at `now`.
    pub fn active(&self, now: SystemTime) -> bool {
        match self {
            Self::Off => false,
            Self::UntilResumed => true,
            Self::Until(until) => now < *until,
        }
    }
}

impl PairStore {
    pub fn pause(&self) -> Result<Pause, Error> {
        let Some(row) = self.database.pause()? else {
            return Ok(Pause::Off);
        };
        let Some(until) = row.until_ms else {
            return Ok(Pause::UntilResumed);
        };
        let until = u64::try_from(until).map_err(|_| Error::InvalidStored("pause"))?;
        Ok(Pause::Until(UNIX_EPOCH + Duration::from_millis(until)))
    }

    pub fn set_pause(&mut self, pause: Pause) -> Result<(), Error> {
        let row = match pause {
            Pause::Off => None,
            Pause::UntilResumed => Some(PauseRow { until_ms: None }),
            Pause::Until(until) => {
                let since = until.duration_since(UNIX_EPOCH).unwrap_or_default();
                Some(PauseRow {
                    until_ms: Some(i64::try_from(since.as_millis()).unwrap_or(i64::MAX)),
                })
            }
        };
        Ok(self.database.set_pause(row)?)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::location;
    use super::*;

    #[test]
    fn pauses_survive_reopening_and_expire() {
        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        assert_eq!(store.pause().unwrap(), Pause::Off);
        let until = UNIX_EPOCH + Duration::from_secs(100);
        store.set_pause(Pause::Until(until)).unwrap();
        drop(store);

        let store = PairStore::open(&location).unwrap();
        let pause = store.pause().unwrap();
        assert_eq!(pause, Pause::Until(until));
        assert!(pause.active(UNIX_EPOCH + Duration::from_secs(99)));
        assert!(!pause.active(until));
        assert!(Pause::UntilResumed.active(until));
    }
}
