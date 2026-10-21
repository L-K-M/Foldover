use crate::{InvalidPair, MetadataPolicy, SyncPath};

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
    /// Unattended runs at a time of day on the set weekdays, in the
    /// machine's local time zone (PLAN.md §3.5).
    Daily(DailySchedule),
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

/// A time of day as minutes after local midnight.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TimeOfDay(u16);

impl TimeOfDay {
    /// `23:59`; a daily schedule cannot target the day's final instant.
    pub const MAX_MINUTES: u16 = 24 * 60 - 1;

    pub fn from_minutes(minutes: u32) -> Result<Self, InvalidPair> {
        if minutes > u32::from(Self::MAX_MINUTES) {
            return Err(InvalidPair::Schedule);
        }
        Ok(Self(minutes as u16))
    }

    pub const fn minutes(self) -> u32 {
        self.0 as u32
    }
}

/// Monday through Sunday; bit `0` of `Weekdays` is Monday.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Weekday {
    /// Monday first, the `Weekdays` bit order.
    pub const ALL: [Self; 7] = [
        Self::Monday,
        Self::Tuesday,
        Self::Wednesday,
        Self::Thursday,
        Self::Friday,
        Self::Saturday,
        Self::Sunday,
    ];

    const fn bit(self) -> u8 {
        1 << (self as u8)
    }

    fn previous(self) -> Self {
        Self::ALL[(self as usize + 6) % 7]
    }
}

/// A non-empty set of weekdays, one bit per `Weekday`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Weekdays(u8);

impl Weekdays {
    /// Every day of the week.
    pub const ALL: Self = Self(0x7f);
    /// Monday through Friday.
    pub const MONDAY_TO_FRIDAY: Self = Self(0x1f);

    /// The given days; at least one is required.
    pub fn of(days: &[Weekday]) -> Result<Self, InvalidPair> {
        Self::from_bits(days.iter().fold(0, |bits, day| bits | day.bit()))
    }

    /// Bits map Monday onto `0x01` through Sunday onto `0x40`.
    pub fn from_bits(bits: u8) -> Result<Self, InvalidPair> {
        if bits == 0 || bits & !Self::ALL.0 != 0 {
            return Err(InvalidPair::Schedule);
        }
        Ok(Self(bits))
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub fn contains(self, day: Weekday) -> bool {
        self.0 & day.bit() != 0
    }

    /// The set's days, Monday first.
    pub fn days(self) -> impl Iterator<Item = Weekday> {
        Weekday::ALL
            .into_iter()
            .filter(move |day| self.contains(*day))
    }
}

/// A local wall-clock date and minute; a calendar schedule compares
/// these, so weekday must match the date and minute stays under 1440.
/// Drivers produce valid values, so `new` does not check.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LocalDateTime {
    year: i32,
    month: u8,
    day: u8,
    weekday: Weekday,
    minute: u16,
}

impl LocalDateTime {
    pub const fn new(year: i32, month: u8, day: u8, weekday: Weekday, minute: u16) -> Self {
        Self {
            year,
            month,
            day,
            weekday,
            minute,
        }
    }

    pub const fn weekday(self) -> Weekday {
        self.weekday
    }

    /// Minutes after local midnight.
    pub const fn minute(self) -> u16 {
        self.minute
    }

    /// The same date at `minute` after midnight.
    fn with_minute(self, minute: u16) -> Self {
        Self { minute, ..self }
    }

    /// The calendar day before this one.
    fn previous_day(self) -> Self {
        let (year, month, day) = if self.day > 1 {
            (self.year, self.month, self.day - 1)
        } else if self.month > 1 {
            let month = self.month - 1;
            (self.year, month, Self::month_length(self.year, month))
        } else {
            (self.year - 1, 12, 31)
        };
        Self {
            year,
            month,
            day,
            weekday: self.weekday.previous(),
            minute: self.minute,
        }
    }

    fn month_length(year: i32, month: u8) -> u8 {
        match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            _ if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
            _ => 28,
        }
    }
}

/// Unattended runs at a time of day on the set weekdays.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DailySchedule {
    at: TimeOfDay,
    days: Weekdays,
}

impl DailySchedule {
    pub const fn new(at: TimeOfDay, days: Weekdays) -> Self {
        Self { at, days }
    }

    pub const fn at(self) -> TimeOfDay {
        self.at
    }

    pub const fn days(self) -> Weekdays {
        self.days
    }

    /// The most recent scheduled moment at or before `local`. The day
    /// set is never empty, so the answer is at most a week back.
    pub fn latest_at_or_before(self, local: LocalDateTime) -> LocalDateTime {
        let mut day = local;
        if !self.days.contains(day.weekday()) || u32::from(day.minute()) < self.at.minutes() {
            day = day.previous_day();
            while !self.days.contains(day.weekday()) {
                day = day.previous_day();
            }
        }
        day.with_minute(self.at.0)
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

/// Where Foldover keeps a folder's records: its identity, lease, archive
/// and resumable copies, all in the folder's `.foldover`.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum FolderRecords {
    #[default]
    Both,
    /// Only in a Backup's source, for a target that cannot hold
    /// `.foldover` (PLAN.md §3.6). What a run replaces or removes there is
    /// deleted, not archived.
    SourceOnly,
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
    canary: Option<SyncPath>,
    records: FolderRecords,
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
            canary: None,
            records: FolderRecords::Both,
        }
    }

    pub fn with_records(self, records: FolderRecords) -> Self {
        Self { records, ..self }
    }

    pub const fn records(&self) -> FolderRecords {
        self.records
    }

    pub fn with_schedule(self, schedule: Schedule) -> Self {
        Self { schedule, ..self }
    }

    pub fn with_filters(self, filters: Filters) -> Self {
        Self { filters, ..self }
    }

    /// A file kept unchanged in both folders that proves the right ones
    /// are mounted. Inside `.foldover` it would check Foldover's own
    /// state, so it is refused.
    pub fn with_canary(self, canary: Option<SyncPath>) -> Result<Self, InvalidPair> {
        if canary
            .as_ref()
            .is_some_and(|path| path.components().next() == Some(b".foldover"))
        {
            return Err(InvalidPair::Canary);
        }
        Ok(Self { canary, ..self })
    }

    pub fn canary(&self) -> Option<&SyncPath> {
        self.canary.as_ref()
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

    fn local(year: i32, month: u8, day: u8, weekday: Weekday, minute: u16) -> LocalDateTime {
        LocalDateTime::new(year, month, day, weekday, minute)
    }

    #[test]
    fn daily_time_and_days_are_validated() {
        assert_eq!(TimeOfDay::from_minutes(0).unwrap().minutes(), 0);
        assert_eq!(TimeOfDay::from_minutes(1439).unwrap().minutes(), 1439);
        assert!(TimeOfDay::from_minutes(1440).is_err());
        for bits in [0, 0x80, 0xff] {
            assert!(Weekdays::from_bits(bits).is_err());
        }
        assert!(Weekdays::of(&[]).is_err());
        let weekdays = Weekdays::MONDAY_TO_FRIDAY;
        assert!(weekdays.contains(Weekday::Friday));
        assert!(!weekdays.contains(Weekday::Saturday));
        assert_eq!(weekdays.days().count(), 5);
    }

    #[test]
    fn latest_at_or_before_wraps_weeks_and_months() {
        let at = TimeOfDay::from_minutes(18 * 60).unwrap();
        let weekdays = DailySchedule::new(at, Weekdays::MONDAY_TO_FRIDAY);
        // A Friday evening lands the same day; Saturday falls back.
        assert_eq!(
            weekdays.latest_at_or_before(local(2026, 10, 16, Weekday::Friday, 19 * 60)),
            local(2026, 10, 16, Weekday::Friday, 18 * 60)
        );
        assert_eq!(
            weekdays.latest_at_or_before(local(2026, 10, 17, Weekday::Saturday, 19 * 60)),
            local(2026, 10, 16, Weekday::Friday, 18 * 60)
        );
        // A Monday morning before the time falls back across the weekend
        // and, on March 1st, across a month boundary.
        assert_eq!(
            weekdays.latest_at_or_before(local(2026, 10, 19, Weekday::Monday, 9 * 60)),
            local(2026, 10, 16, Weekday::Friday, 18 * 60)
        );
        assert_eq!(
            weekdays.latest_at_or_before(local(2027, 3, 1, Weekday::Monday, 9 * 60)),
            local(2027, 2, 26, Weekday::Friday, 18 * 60)
        );
        // A Sunday-only schedule steps back a whole week.
        let sundays = DailySchedule::new(at, Weekdays::of(&[Weekday::Sunday]).unwrap());
        assert_eq!(
            sundays.latest_at_or_before(local(2026, 10, 18, Weekday::Sunday, 17 * 60)),
            local(2026, 10, 11, Weekday::Sunday, 18 * 60)
        );
        // Exactly at the scheduled minute still counts as reached.
        assert_eq!(
            weekdays.latest_at_or_before(local(2026, 10, 16, Weekday::Friday, 18 * 60)),
            local(2026, 10, 16, Weekday::Friday, 18 * 60)
        );
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

    #[test]
    fn a_canary_lives_outside_the_internal_directory() {
        let options = PairOptions::default();
        let canary = |bytes: &[u8]| SyncPath::try_from(bytes).unwrap();
        assert_eq!(options.canary(), None);
        for internal in [b".foldover".as_slice(), b".foldover/id"] {
            assert_eq!(
                options.clone().with_canary(Some(canary(internal))),
                Err(InvalidPair::Canary)
            );
        }
        let kept = options
            .with_canary(Some(canary(b"docs/canary.txt")))
            .unwrap();
        assert_eq!(kept.canary(), Some(&canary(b"docs/canary.txt")));
        assert_eq!(kept.with_canary(None).unwrap().canary(), None);
    }
}
