//! `foldover-host` — domain services layer. Environment policy:
//! battery, Low Power Mode, metered networks and power assertions.
//! Depends on foldover-os. See PLAN.md §4.4.

pub use foldover_os::{NetworkCost, PowerMode, computer_name, local_time};

use std::io;

use foldover_os::IdleSleepAssertion;

/// Holds the Mac awake for one sync (PLAN.md §3.5) until dropped. A Mac
/// put to sleep on purpose still sleeps; the run then stops like any
/// interrupted run and the next one finishes the work.
pub struct RunAwake {
    /// None where the system has no power assertions, such as Linux:
    /// nothing there idles a running agent to sleep on Foldover's behalf.
    _assertion: Option<IdleSleepAssertion>,
}

/// Keep the Mac awake while `pair` syncs. Fails only where power
/// assertions exist and taking one failed.
pub fn keep_awake(pair: &str) -> io::Result<RunAwake> {
    Ok(RunAwake {
        _assertion: IdleSleepAssertion::take(&format!("Foldover is syncing {pair}"))?,
    })
}

/// Why automatic syncs wait right now (PLAN.md §3.5).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvironmentHold {
    LowPowerMode,
    ExpensiveNetwork,
    ConstrainedNetwork,
}

/// Whether a pair's syncs go over the network: to an SFTP server, another
/// Mac or a mounted share.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkUse {
    Local,
    Network,
}

/// The person's choice for automatic syncs in Low Power Mode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LowPowerSyncs {
    #[default]
    Pause,
    Run,
}

/// The Mac's state as automatic syncs see it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Environment {
    pub power: PowerMode,
    /// None until the system reports the network path.
    pub network: Option<NetworkCost>,
}

impl Environment {
    pub fn now() -> Self {
        Self {
            power: foldover_os::power_mode(),
            network: foldover_os::network_cost(),
        }
    }

    /// Why a pair's automatic syncs wait now, if they do. Low Power Mode
    /// holds every pair unless the person opted in; an expensive or
    /// constrained network holds the pairs that use it. Manual syncs
    /// never wait.
    pub fn hold(self, network: NetworkUse, low_power: LowPowerSyncs) -> Option<EnvironmentHold> {
        if self.power == PowerMode::LowPower && low_power == LowPowerSyncs::Pause {
            return Some(EnvironmentHold::LowPowerMode);
        }
        if network == NetworkUse::Local {
            return None;
        }
        match self.network {
            Some(NetworkCost::Expensive) => Some(EnvironmentHold::ExpensiveNetwork),
            Some(NetworkCost::Constrained) => Some(EnvironmentHold::ConstrainedNetwork),
            Some(NetworkCost::Unrestricted) | None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_power_mode_holds_every_pair_unless_the_person_opted_in() {
        let low = Environment {
            power: PowerMode::LowPower,
            network: Some(NetworkCost::Unrestricted),
        };
        for network in [NetworkUse::Local, NetworkUse::Network] {
            assert_eq!(
                low.hold(network, LowPowerSyncs::Pause),
                Some(EnvironmentHold::LowPowerMode)
            );
            assert_eq!(low.hold(network, LowPowerSyncs::Run), None);
        }
    }

    #[test]
    fn a_costly_network_holds_only_pairs_that_use_it() {
        for (cost, hold) in [
            (NetworkCost::Expensive, EnvironmentHold::ExpensiveNetwork),
            (
                NetworkCost::Constrained,
                EnvironmentHold::ConstrainedNetwork,
            ),
        ] {
            let costly = Environment {
                power: PowerMode::Normal,
                network: Some(cost),
            };
            assert_eq!(
                costly.hold(NetworkUse::Network, LowPowerSyncs::Pause),
                Some(hold)
            );
            assert_eq!(costly.hold(NetworkUse::Local, LowPowerSyncs::Pause), None);
        }
        let unknown = Environment {
            power: PowerMode::Normal,
            network: None,
        };
        assert_eq!(
            unknown.hold(NetworkUse::Network, LowPowerSyncs::Pause),
            None
        );
    }

    #[test]
    fn keeping_awake_succeeds_with_or_without_power_assertions() {
        let awake = keep_awake("Photos").unwrap();
        assert_eq!(awake._assertion.is_some(), cfg!(target_os = "macos"));
    }
}
