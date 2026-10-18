use crate::{InvalidPair, MetadataPolicy};

/// When the agent syncs a pair without being asked.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Schedule {
    #[default]
    Manual,
    /// Unattended runs at this interval, after the pair's first manual sync.
    Every(ScheduleInterval),
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ScheduleInterval(u32);

impl ScheduleInterval {
    pub const MIN_MINUTES: u32 = 1;
    /// One week.
    pub const MAX_MINUTES: u32 = 7 * 24 * 60;

    pub fn from_minutes(minutes: u32) -> Result<Self, InvalidPair> {
        if !(Self::MIN_MINUTES..=Self::MAX_MINUTES).contains(&minutes) {
            return Err(InvalidPair::Schedule);
        }
        Ok(Self(minutes))
    }

    pub const fn minutes(self) -> u32 {
        self.0
    }
}

/// How far each write is flushed before a sync step counts as done.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Durability {
    /// `fsync`: survives app, agent and system crashes.
    #[default]
    Standard,
    /// Also flushes the drive's cache (`F_FULLFSYNC` on macOS), so writes
    /// survive power loss. Each file costs roughly 10–30 ms more.
    Full,
}

/// Per-pair settings beyond roots and mode.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct PairOptions {
    metadata: MetadataPolicy,
    durability: Durability,
    schedule: Schedule,
}

impl PairOptions {
    pub const fn new(metadata: MetadataPolicy, durability: Durability) -> Self {
        Self {
            metadata,
            durability,
            schedule: Schedule::Manual,
        }
    }

    pub const fn with_schedule(self, schedule: Schedule) -> Self {
        Self { schedule, ..self }
    }

    pub const fn metadata(self) -> MetadataPolicy {
        self.metadata
    }

    pub const fn durability(self) -> Durability {
        self.durability
    }

    pub const fn schedule(self) -> Schedule {
        self.schedule
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_span_a_minute_to_a_week() {
        assert!(ScheduleInterval::from_minutes(0).is_err());
        assert_eq!(ScheduleInterval::from_minutes(1).unwrap().minutes(), 1);
        assert!(ScheduleInterval::from_minutes(ScheduleInterval::MAX_MINUTES).is_ok());
        assert!(ScheduleInterval::from_minutes(ScheduleInterval::MAX_MINUTES + 1).is_err());
        assert_eq!(PairOptions::default().schedule(), Schedule::Manual);
    }
}
