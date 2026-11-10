//! The person's automation choices (ADR 0036, ADR 0054). Automatic syncs
//! pause in Low Power Mode unless they opted in, and run gently unless
//! they chose full effort; manual syncs still run, at full effort.

use foldover_types::Effort;

use super::{Error, PairStore};

const PAUSE: &str = "pause";
const RUN: &str = "run";
const GENTLE: &str = "gentle";
const FULL: &str = "full";

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

    /// How hard automatic syncs may work the Mac: `Gentle` by default.
    pub fn automatic_effort(&self) -> Result<Effort, Error> {
        let Some(row) = self.database.automation_setting()? else {
            return Ok(Effort::Gentle);
        };
        match row.automatic_effort.as_str() {
            GENTLE => Ok(Effort::Gentle),
            FULL => Ok(Effort::Full),
            _ => Err(Error::InvalidStored("automatic effort")),
        }
    }

    pub fn set_automatic_effort(&mut self, effort: Effort) -> Result<(), Error> {
        Ok(self.database.set_automatic_effort(match effort {
            Effort::Gentle => GENTLE,
            Effort::Full => FULL,
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

    #[test]
    fn automatic_syncs_default_to_gentle_and_the_choice_survives_reopening() {
        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        assert_eq!(store.automatic_effort().unwrap(), Effort::Gentle);
        store.set_automatic_effort(Effort::Full).unwrap();
        assert_eq!(store.automatic_effort().unwrap(), Effort::Full);
        // Each choice keeps the other.
        assert_eq!(store.low_power_syncs().unwrap(), LowPowerSyncs::Pause);
        store.set_low_power_syncs(LowPowerSyncs::Run).unwrap();
        drop(store);

        let mut store = PairStore::open(&location).unwrap();
        assert_eq!(store.automatic_effort().unwrap(), Effort::Full);
        assert_eq!(store.low_power_syncs().unwrap(), LowPowerSyncs::Run);
        store.set_automatic_effort(Effort::Gentle).unwrap();
        assert_eq!(store.automatic_effort().unwrap(), Effort::Gentle);
    }
}
