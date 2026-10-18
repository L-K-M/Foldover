use crate::{InvalidPair, MetadataPolicy};

/// When the agent syncs a pair without being asked.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Schedule {
    #[default]
    Manual,
    /// Unattended runs at this interval, after the pair's first manual sync.
    Every(ScheduleInterval),
    /// Unattended runs once changed files have been quiet briefly, and
    /// hourly, because change notifications can be missed.
    OnChange,
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

/// Gitignore-style exclusion rules, one per line, and an optional file
/// size limit. They apply to both folders, so an excluded entry never
/// looks deleted on one side.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Filters {
    rules: Vec<String>,
    size_limit: Option<u64>,
    age_limit: Option<u32>,
}

impl Filters {
    pub const MAX_RULES: usize = 256;
    pub const MAX_RULE_BYTES: usize = 1024;
    /// A century: older than any file a person still means to sync.
    pub const MAX_AGE_DAYS: u32 = 36_500;

    /// Syntax is checked where rules are compiled; here only size and
    /// characters that cannot be stored one per line.
    pub fn new(rules: Vec<String>) -> Result<Self, InvalidPair> {
        if rules.len() > Self::MAX_RULES
            || rules
                .iter()
                .any(|rule| rule.len() > Self::MAX_RULE_BYTES || rule.contains(['\n', '\r', '\0']))
        {
            return Err(InvalidPair::Filters);
        }
        Ok(Self {
            rules,
            size_limit: None,
            age_limit: None,
        })
    }

    /// Skip files unchanged for more than `days` in both folders. A file
    /// changed recently in either folder still syncs.
    pub fn with_age_limit(self, days: Option<u32>) -> Result<Self, InvalidPair> {
        if days.is_some_and(|days| days == 0 || days > Self::MAX_AGE_DAYS) {
            return Err(InvalidPair::Filters);
        }
        Ok(Self {
            age_limit: days,
            ..self
        })
    }

    pub fn age_limit(&self) -> Option<u32> {
        self.age_limit
    }

    /// Skip files larger than `bytes`, in both folders. Zero is refused:
    /// it would skip every file with content.
    pub fn with_size_limit(self, bytes: Option<u64>) -> Result<Self, InvalidPair> {
        if bytes == Some(0) {
            return Err(InvalidPair::Filters);
        }
        Ok(Self {
            size_limit: bytes,
            ..self
        })
    }

    pub fn rules(&self) -> &[String] {
        &self.rules
    }

    pub fn size_limit(&self) -> Option<u64> {
        self.size_limit
    }
}

/// Per-pair settings beyond roots and mode.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct PairOptions {
    metadata: MetadataPolicy,
    durability: Durability,
    schedule: Schedule,
    filters: Filters,
}

impl PairOptions {
    pub const fn new(metadata: MetadataPolicy, durability: Durability) -> Self {
        Self {
            metadata,
            durability,
            schedule: Schedule::Manual,
            filters: Filters {
                rules: Vec::new(),
                size_limit: None,
                age_limit: None,
            },
        }
    }

    pub fn with_schedule(self, schedule: Schedule) -> Self {
        Self { schedule, ..self }
    }

    pub fn with_filters(self, filters: Filters) -> Self {
        Self { filters, ..self }
    }

    pub const fn metadata(&self) -> MetadataPolicy {
        self.metadata
    }

    pub const fn durability(&self) -> Durability {
        self.durability
    }

    pub const fn schedule(&self) -> Schedule {
        self.schedule
    }

    pub fn filters(&self) -> &Filters {
        &self.filters
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

    #[test]
    fn age_limits_span_a_day_to_a_century() {
        let rules = Filters::new(Vec::new()).unwrap();
        assert!(rules.clone().with_age_limit(Some(0)).is_err());
        assert!(
            rules
                .clone()
                .with_age_limit(Some(Filters::MAX_AGE_DAYS + 1))
                .is_err()
        );
        assert_eq!(
            rules.with_age_limit(Some(30)).unwrap().age_limit(),
            Some(30)
        );
    }

    #[test]
    fn size_limits_must_allow_some_content() {
        let rules = Filters::new(Vec::new()).unwrap();
        assert!(rules.clone().with_size_limit(Some(0)).is_err());
        assert_eq!(
            rules.with_size_limit(Some(1)).unwrap().size_limit(),
            Some(1)
        );
    }
}
