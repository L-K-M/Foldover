use foldover_types::{
    DailySchedule, Durability, Filters, FolderRecords, InvalidPair, MetadataPolicy, PairConfig,
    PairDraft, PairOptions, PairRoot, Propagation, RemovalProtection, ReviewPolicy, Schedule,
    ScheduleInterval, Side, SyncMode, SyncPath, TimeOfDay, Weekday, Weekdays,
};
use serde::{Deserialize, Serialize};

pub const PAIR_LIST: &str = "pair.list";
pub const PAIR_GET: &str = "pair.get";
pub const PAIR_CREATE: &str = "pair.create";
pub const PAIR_UPDATE: &str = "pair.update";
pub const PAIR_REMOVE: &str = "pair.remove";
pub const PAIR_CHECK: &str = "pair.check";
// Even maximum-length names and raw roots fit below the 1 MiB IPC frame limit.
pub const PAIR_PAGE_SIZE: usize = 16;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PairSide {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum PairMode {
    Sync,
    Mirror { source: PairSide },
    Backup { source: PairSide },
}

impl From<Side> for PairSide {
    fn from(side: Side) -> Self {
        match side {
            Side::Left => Self::Left,
            Side::Right => Self::Right,
        }
    }
}

impl From<PairSide> for Side {
    fn from(side: PairSide) -> Self {
        match side {
            PairSide::Left => Self::Left,
            PairSide::Right => Self::Right,
        }
    }
}

impl From<SyncMode> for PairMode {
    fn from(mode: SyncMode) -> Self {
        match mode {
            SyncMode::Sync => Self::Sync,
            SyncMode::Mirror { source } => Self::Mirror {
                source: source.into(),
            },
            SyncMode::Backup { source } => Self::Backup {
                source: source.into(),
            },
        }
    }
}

impl From<PairMode> for SyncMode {
    fn from(mode: PairMode) -> Self {
        match mode {
            PairMode::Sync => Self::Sync,
            PairMode::Mirror { source } => Self::Mirror {
                source: source.into(),
            },
            PairMode::Backup { source } => Self::Backup {
                source: source.into(),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PairPropagation {
    Sync,
    Ignore,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PairDurability {
    Standard,
    Full,
}

/// Which folders hold Foldover's records (`.foldover`).
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PairRecords {
    #[default]
    Both,
    /// Only a Backup's source: the target cannot hold `.foldover`, and
    /// what a run replaces there is deleted, not archived.
    SourceOnly,
}

impl PairRecords {
    fn is_both(&self) -> bool {
        *self == Self::Both
    }
}

/// Whether a sync run requires a person to review the preview first.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PairReviewPolicy {
    #[default]
    Automatic,
    Required,
}

/// Whether unusually large removals or sudden emptying stop for review.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PairRemovalProtection {
    #[default]
    Enabled,
    Disabled,
}

impl From<ReviewPolicy> for PairReviewPolicy {
    fn from(policy: ReviewPolicy) -> Self {
        match policy {
            ReviewPolicy::Automatic => Self::Automatic,
            ReviewPolicy::Required => Self::Required,
        }
    }
}

impl From<PairReviewPolicy> for ReviewPolicy {
    fn from(policy: PairReviewPolicy) -> Self {
        match policy {
            PairReviewPolicy::Automatic => Self::Automatic,
            PairReviewPolicy::Required => Self::Required,
        }
    }
}

impl From<RemovalProtection> for PairRemovalProtection {
    fn from(protection: RemovalProtection) -> Self {
        match protection {
            RemovalProtection::Enabled => Self::Enabled,
            RemovalProtection::Disabled => Self::Disabled,
        }
    }
}

impl From<PairRemovalProtection> for RemovalProtection {
    fn from(protection: PairRemovalProtection) -> Self {
        match protection {
            PairRemovalProtection::Enabled => Self::Enabled,
            PairRemovalProtection::Disabled => Self::Disabled,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairSettings {
    pub modification_times: PairPropagation,
    pub extended_attributes: PairPropagation,
    pub permissions: PairPropagation,
    pub durability: PairDurability,
    pub schedule: PairSchedule,
    /// Gitignore-style exclusion rules, one per entry.
    pub filters: Vec<String>,
    /// Files larger than this many bytes are skipped in both folders.
    pub size_limit: Option<u64>,
    /// Files unchanged for more than this many days in both folders are
    /// skipped.
    pub age_limit: Option<u32>,
    /// A file kept unchanged in both folders that proves the right ones
    /// are mounted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canary: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "PairRecords::is_both")]
    pub records: PairRecords,
    /// File data bytes per second the pair's copies share at most.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandwidth_limit: Option<u64>,
    /// Whether a sync run requires a person to review the preview first.
    #[serde(default)]
    pub review_policy: PairReviewPolicy,
    /// Whether unusually large removals or sudden emptying stop for review.
    #[serde(default)]
    pub removal_protection: PairRemovalProtection,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum PairSchedule {
    Manual,
    Every {
        minutes: u32,
    },
    OnChange,
    /// A time of day in minutes after local midnight, on these weekdays.
    Daily {
        at_minutes: u32,
        days: Vec<PairDay>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PairDay {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl From<PairDay> for Weekday {
    fn from(day: PairDay) -> Self {
        match day {
            PairDay::Monday => Self::Monday,
            PairDay::Tuesday => Self::Tuesday,
            PairDay::Wednesday => Self::Wednesday,
            PairDay::Thursday => Self::Thursday,
            PairDay::Friday => Self::Friday,
            PairDay::Saturday => Self::Saturday,
            PairDay::Sunday => Self::Sunday,
        }
    }
}

impl From<Weekday> for PairDay {
    fn from(day: Weekday) -> Self {
        match day {
            Weekday::Monday => Self::Monday,
            Weekday::Tuesday => Self::Tuesday,
            Weekday::Wednesday => Self::Wednesday,
            Weekday::Thursday => Self::Thursday,
            Weekday::Friday => Self::Friday,
            Weekday::Saturday => Self::Saturday,
            Weekday::Sunday => Self::Sunday,
        }
    }
}

impl From<Propagation> for PairPropagation {
    fn from(value: Propagation) -> Self {
        match value {
            Propagation::Sync => Self::Sync,
            Propagation::Ignore => Self::Ignore,
        }
    }
}

impl From<PairPropagation> for Propagation {
    fn from(value: PairPropagation) -> Self {
        match value {
            PairPropagation::Sync => Self::Sync,
            PairPropagation::Ignore => Self::Ignore,
        }
    }
}

impl From<&PairOptions> for PairSettings {
    fn from(options: &PairOptions) -> Self {
        let metadata = options.metadata();
        Self {
            modification_times: metadata.modification_times().into(),
            extended_attributes: metadata.extended_attributes().into(),
            permissions: metadata.permissions().into(),
            durability: match options.durability() {
                Durability::Standard => PairDurability::Standard,
                Durability::Full => PairDurability::Full,
            },
            schedule: match options.schedule() {
                Schedule::Manual => PairSchedule::Manual,
                Schedule::Every(interval) => PairSchedule::Every {
                    minutes: interval.minutes(),
                },
                Schedule::OnChange => PairSchedule::OnChange,
                Schedule::Daily(daily) => PairSchedule::Daily {
                    at_minutes: daily.at().minutes(),
                    days: daily.days().days().map(PairDay::from).collect(),
                },
            },
            filters: options.filters().rules().to_vec(),
            size_limit: options.filters().size_limit(),
            age_limit: options.filters().age_limit(),
            canary: options.canary().map(|path| path.as_bytes().to_vec()),
            records: match options.records() {
                FolderRecords::Both => PairRecords::Both,
                FolderRecords::SourceOnly => PairRecords::SourceOnly,
            },
            bandwidth_limit: options.bandwidth_limit(),
            review_policy: options.review_policy().into(),
            removal_protection: options.removal_protection().into(),
        }
    }
}

impl TryFrom<PairSettings> for PairOptions {
    type Error = InvalidPair;

    fn try_from(settings: PairSettings) -> Result<Self, Self::Error> {
        let schedule = match settings.schedule {
            PairSchedule::Manual => Schedule::Manual,
            PairSchedule::Every { minutes } => {
                Schedule::Every(ScheduleInterval::from_minutes(minutes)?)
            }
            PairSchedule::OnChange => Schedule::OnChange,
            PairSchedule::Daily { at_minutes, days } => Schedule::Daily(DailySchedule::new(
                TimeOfDay::from_minutes(at_minutes)?,
                Weekdays::of(
                    &days
                        .iter()
                        .map(|day| Weekday::from(*day))
                        .collect::<Vec<_>>(),
                )?,
            )),
        };
        Self::new(
            MetadataPolicy::new(
                settings.modification_times.into(),
                settings.extended_attributes.into(),
                settings.permissions.into(),
            ),
            match settings.durability {
                PairDurability::Standard => Durability::Standard,
                PairDurability::Full => Durability::Full,
            },
        )
        .with_schedule(schedule)
        .with_filters(
            Filters::new(settings.filters)?
                .with_size_limit(settings.size_limit)?
                .with_age_limit(settings.age_limit)?,
        )
        .with_canary(
            settings
                .canary
                .map(SyncPath::try_from)
                .transpose()
                .map_err(|_| InvalidPair::Canary)?,
        )
        .map(|options| {
            options
                .with_records(match settings.records {
                    PairRecords::Both => FolderRecords::Both,
                    PairRecords::SourceOnly => FolderRecords::SourceOnly,
                })
                .with_bandwidth_limit(settings.bandwidth_limit)
                .map(|options| {
                    options
                        .with_review_policy(settings.review_policy.into())
                        .with_removal_protection(settings.removal_protection.into())
                })
        })?
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairInput {
    pub name: String,
    pub left_root: Vec<u8>,
    pub right_root: Vec<u8>,
    pub mode: PairMode,
    pub options: PairSettings,
}

impl From<&PairDraft> for PairInput {
    fn from(draft: &PairDraft) -> Self {
        Self {
            name: draft.name().to_owned(),
            left_root: draft.left().as_bytes().to_vec(),
            right_root: draft.right().as_bytes().to_vec(),
            mode: draft.mode().into(),
            options: draft.options().into(),
        }
    }
}

impl TryFrom<PairInput> for PairDraft {
    type Error = InvalidPair;

    fn try_from(input: PairInput) -> Result<Self, Self::Error> {
        Ok(Self::new(
            input.name,
            PairRoot::from_bytes(input.left_root)?,
            PairRoot::from_bytes(input.right_root)?,
            input.mode.into(),
        )?
        .with_options(input.options.try_into()?))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairRecord {
    pub id: String,
    pub revision: u64,
    pub configuration: PairInput,
}

impl From<&PairConfig> for PairRecord {
    fn from(pair: &PairConfig) -> Self {
        Self {
            id: pair.id().to_string(),
            revision: pair.revision(),
            configuration: pair.draft().into(),
        }
    }
}

impl TryFrom<PairRecord> for PairConfig {
    type Error = InvalidPair;

    fn try_from(record: PairRecord) -> Result<Self, Self::Error> {
        Self::from_stored(
            record.id.parse()?,
            record.revision,
            record.configuration.try_into()?,
        )
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairList {
    pub after: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairKey {
    pub id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairRevision {
    pub id: String,
    pub revision: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairUpdate {
    pub id: String,
    pub revision: u64,
    pub configuration: PairInput,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairPage {
    pub pairs: Vec<PairRecord>,
    pub next: Option<String>,
}

/// `pair.check`'s answer: what will not work as a person might expect
/// with the folders they picked (PLAN.md §3.6). Never blocks saving.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairWarnings {
    pub warnings: Vec<PairWarning>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum PairWarning {
    /// Another sync engine manages this side's folder.
    CloudManaged { side: PairSide, provider: String },
    /// The pair syncs extended attributes this side cannot keep.
    MetadataUnsupported { side: PairSide },
    /// One side ignores name case and the other does not.
    NamesDiffer { side: PairSide },
    /// This side cannot hold Foldover's `.foldover` records, so it can
    /// only be a Backup target that keeps none.
    RecordsUnsupported { side: PairSide },
}

#[cfg(test)]
mod tests {
    use super::*;
    use foldover_types::{LocalRoot, MAX_LOCAL_ROOT_BYTES, MAX_PAIR_NAME_BYTES, PairId};
    use serde_json::json;

    #[test]
    fn raw_roots_and_all_modes_round_trip_without_losing_domain_validation() {
        for mode in [
            SyncMode::Sync,
            SyncMode::Mirror { source: Side::Left },
            SyncMode::Mirror {
                source: Side::Right,
            },
            SyncMode::Backup { source: Side::Left },
            SyncMode::Backup {
                source: Side::Right,
            },
        ] {
            let options = PairOptions::new(
                MetadataPolicy::new(Propagation::Ignore, Propagation::Sync, Propagation::Sync),
                Durability::Full,
            )
            .with_schedule(Schedule::Every(ScheduleInterval::from_minutes(30).unwrap()))
            .with_bandwidth_limit(Some(2_000_000))
            .unwrap();
            let draft = PairDraft::new(
                "Photos".into(),
                LocalRoot::from_bytes(b"/a/\xff".to_vec()).unwrap(),
                LocalRoot::from_bytes(b"/b".to_vec()).unwrap(),
                mode,
            )
            .unwrap()
            .with_options(options);
            let pair =
                PairConfig::from_stored(PairId::from_bytes([1; 16]).unwrap(), 1, draft).unwrap();
            let json = serde_json::to_value(PairRecord::from(&pair)).unwrap();
            let decoded: PairRecord = serde_json::from_value(json).unwrap();
            assert_eq!(PairConfig::try_from(decoded).unwrap(), pair);
        }
        assert!(
            serde_json::from_value::<PairMode>(
                json!({"kind":"mirror", "source":"left", "ignore":true})
            )
            .is_err()
        );
        assert!(serde_json::from_value::<PairKey>(json!({"id":"x", "root":"/private"})).is_err());
    }

    #[test]
    fn warnings_round_trip_and_reject_unknown_fields() {
        let warnings = PairWarnings {
            warnings: vec![
                PairWarning::CloudManaged {
                    side: PairSide::Left,
                    provider: "iCloud Drive".into(),
                },
                PairWarning::MetadataUnsupported {
                    side: PairSide::Right,
                },
                PairWarning::NamesDiffer {
                    side: PairSide::Left,
                },
            ],
        };
        let json = serde_json::to_value(&warnings).unwrap();
        assert_eq!(
            json,
            json!({"warnings":[
                {"kind":"cloudManaged","side":"left","provider":"iCloud Drive"},
                {"kind":"metadataUnsupported","side":"right"},
                {"kind":"namesDiffer","side":"left"},
            ]})
        );
        assert_eq!(
            serde_json::from_value::<PairWarnings>(json).unwrap(),
            warnings
        );
        assert!(
            serde_json::from_value::<PairWarnings>(
                json!({"warnings":[{"kind":"namesDiffer","side":"left","why":"case"}]})
            )
            .is_err()
        );
    }

    #[test]
    fn a_daily_schedule_serializes_as_time_and_weekdays() {
        let daily = Schedule::Daily(DailySchedule::new(
            TimeOfDay::from_minutes(18 * 60).unwrap(),
            Weekdays::MONDAY_TO_FRIDAY,
        ));
        let options = PairOptions::default().with_schedule(daily);
        assert_eq!(
            serde_json::to_value(PairSettings::from(&options).schedule).unwrap(),
            json!({"kind":"daily","at_minutes":1080,"days":["monday","tuesday","wednesday","thursday","friday"]})
        );
        for json in [
            json!({"kind":"daily","at_minutes":1440,"days":["monday"]}),
            json!({"kind":"daily","at_minutes":0,"days":[]}),
        ] {
            let settings: PairSettings = serde_json::from_value(json!({
                "modification_times":"sync","extended_attributes":"sync",
                "permissions":"sync","durability":"standard","schedule":json,
                "filters":[],"size_limit":null,"age_limit":null,"bandwidth_limit":null,
            }))
            .unwrap_or_else(|_| panic!("{json}"));
            assert!(PairOptions::try_from(settings).is_err(), "{json}");
        }
        assert!(
            serde_json::from_value::<PairSchedule>(
                json!({"kind":"daily","at_minutes":0,"days":["funday"]})
            )
            .is_err()
        );
    }

    /// Records stay off the wire while in both folders, and so does an
    /// unset bandwidth limit, so settings read as before on an agent
    /// that knows neither; kept only in the source, records round-trip.
    #[test]
    fn records_travel_only_when_kept_in_the_source() {
        let both = serde_json::to_value(PairSettings::from(&PairOptions::default())).unwrap();
        assert!(both.get("records").is_none());
        assert!(both.get("bandwidth_limit").is_none());
        let source_only = PairOptions::default().with_records(FolderRecords::SourceOnly);
        let settings = PairSettings::from(&source_only);
        let json = serde_json::to_value(&settings).unwrap();
        assert_eq!(json["records"], json!("sourceOnly"));
        let read: PairSettings = serde_json::from_value(json).unwrap();
        assert_eq!(PairOptions::try_from(read).unwrap(), source_only);
    }

    #[test]
    fn safety_settings_round_trip_all_combinations_and_defaults() {
        // All four combinations round-trip through the wire.
        for review in [ReviewPolicy::Automatic, ReviewPolicy::Required] {
            for removal in [RemovalProtection::Enabled, RemovalProtection::Disabled] {
                let options = PairOptions::default()
                    .with_review_policy(review)
                    .with_removal_protection(removal);
                let settings = PairSettings::from(&options);
                let json = serde_json::to_value(&settings).unwrap();
                let read: PairSettings = serde_json::from_value(json).unwrap();
                let restored = PairOptions::try_from(read).unwrap();
                assert_eq!(restored.review_policy(), review);
                assert_eq!(restored.removal_protection(), removal);
            }
        }
        // Omitted wire fields yield defaults.
        let minimal: PairSettings = serde_json::from_value(json!({
            "modification_times":"sync","extended_attributes":"sync",
            "permissions":"sync","durability":"standard",
            "schedule":{"kind":"manual"},
            "filters":[],"size_limit":null,"age_limit":null,
        }))
        .unwrap();
        let from_minimal = PairOptions::try_from(minimal).unwrap();
        assert_eq!(from_minimal.review_policy(), ReviewPolicy::Automatic);
        assert_eq!(
            from_minimal.removal_protection(),
            RemovalProtection::Enabled
        );
        // Invalid enum strings are rejected by serde.
        assert!(
            serde_json::from_value::<PairSettings>(json!({
                "modification_times":"sync","extended_attributes":"sync",
                "permissions":"sync","durability":"standard",
                "schedule":{"kind":"manual"},
                "filters":[],"size_limit":null,"age_limit":null,
                "review_policy":"unknown",
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<PairSettings>(json!({
                "modification_times":"sync","extended_attributes":"sync",
                "permissions":"sync","durability":"standard",
                "schedule":{"kind":"manual"},
                "filters":[],"size_limit":null,"age_limit":null,
                "removal_protection":"unknown",
            }))
            .is_err()
        );
    }

    #[test]
    fn worst_case_page_remains_below_the_frame_limit() {
        let mut left = vec![u8::MAX; MAX_LOCAL_ROOT_BYTES];
        left[0] = b'/';
        let mut right = left.clone();
        right[1] = b'x';
        let draft = PairDraft::new(
            "\u{1}".repeat(MAX_PAIR_NAME_BYTES),
            LocalRoot::from_bytes(left).unwrap(),
            LocalRoot::from_bytes(right).unwrap(),
            SyncMode::Sync,
        )
        .unwrap();
        let pair =
            PairConfig::from_stored(PairId::from_bytes([1; 16]).unwrap(), i64::MAX as u64, draft)
                .unwrap();
        let page = PairPage {
            pairs: vec![PairRecord::from(&pair); PAIR_PAGE_SIZE],
            next: Some(pair.id().to_string()),
        };
        assert!(serde_json::to_vec(&page).unwrap().len() < 1024 * 1024);
    }
}
