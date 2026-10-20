//! The person's automation choices (ADR 0036). Automatic syncs pause in
//! Low Power Mode unless they opted in; manual syncs still run.

use super::{Error, PairStore};

const PAUSE: &str = "pause";
const RUN: &str = "run";

/// The person's choice for automatic syncs in Low Power Mode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LowPowerSyncs {
    #[default]
    Pause,
    Run,
}

impl PairStore {
    pub fn low_power_syncs(&self) -> Result<LowPowerSyncs, Error> {
        let Some(row) = self.database.automation_setting()? else {
            return Ok(LowPowerSyncs::default());
        };
        match row.low_power.as_str() {
            PAUSE => Ok(LowPowerSyncs::Pause),
            RUN => Ok(LowPowerSyncs::Run),
            _ => Err(Error::InvalidStored("low power syncs")),
        }
    }

    pub fn set_low_power_syncs(&mut self, syncs: LowPowerSyncs) -> Result<(), Error> {
        Ok(self.database.set_low_power(match syncs {
            LowPowerSyncs::Pause => PAUSE,
            LowPowerSyncs::Run => RUN,
        })?)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::location;
    use super::*;

    #[test]
    fn the_low_power_choice_defaults_to_pause_and_survives_reopening() {
        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        assert_eq!(store.low_power_syncs().unwrap(), LowPowerSyncs::Pause);
        store.set_low_power_syncs(LowPowerSyncs::Run).unwrap();
        assert_eq!(store.low_power_syncs().unwrap(), LowPowerSyncs::Run);
        drop(store);

        let mut store = PairStore::open(&location).unwrap();
        assert_eq!(store.low_power_syncs().unwrap(), LowPowerSyncs::Run);
        store.set_low_power_syncs(LowPowerSyncs::Pause).unwrap();
        assert_eq!(store.low_power_syncs().unwrap(), LowPowerSyncs::Pause);
    }
}
