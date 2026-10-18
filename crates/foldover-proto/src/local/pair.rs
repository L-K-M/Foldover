use foldover_types::{
    Durability, Filters, InvalidPair, MetadataPolicy, PairConfig, PairDraft, PairOptions, PairRoot,
    Propagation, Schedule, ScheduleInterval, Side, SyncMode,
};
use serde::{Deserialize, Serialize};

pub const PAIR_LIST: &str = "pair.list";
pub const PAIR_GET: &str = "pair.get";
pub const PAIR_CREATE: &str = "pair.create";
pub const PAIR_UPDATE: &str = "pair.update";
pub const PAIR_REMOVE: &str = "pair.remove";
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
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum PairSchedule {
    Manual,
    Every { minutes: u32 },
    OnChange,
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
            },
            filters: options.filters().rules().to_vec(),
            size_limit: options.filters().size_limit(),
            age_limit: options.filters().age_limit(),
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
        };
        Ok(Self::new(
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
        ))
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
            .with_schedule(Schedule::Every(ScheduleInterval::from_minutes(30).unwrap()));
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
