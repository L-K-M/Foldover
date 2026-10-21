//! `foldover-ffi` — client layer. The UniFFI export of foldover-client
//! for the Swift app (the `FoldoverCore` module). Exports local version
//! and agent configuration. Depends on foldover-client. See PLAN.md §4.4.
//!
//! This crate cannot opt into the workspace's `unsafe_code = "forbid"`
//! lint: UniFFI's scaffolding expands to `unsafe extern "C"` shims. It
//! contains no hand-written `unsafe`.

// Adapted from L-K-M/Sweetbriar.

mod archive;
pub use archive::*;
mod automation;
pub use automation::*;
mod history;
pub use history::*;
mod peers;
pub use peers::*;
mod servers;
pub use servers::*;
mod sync;
pub use sync::*;

/// `coreVersion()` in Swift: the workspace version, via the client.
#[uniffi::export]
pub fn core_version() -> String {
    foldover_client::version().to_owned()
}

#[derive(Clone, Debug, uniffi::Enum)]
pub enum AgentCapability {
    HealthOnly,
    PairConfiguration,
    Sync,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct AgentHealth {
    pub core_version: String,
    pub capability: AgentCapability,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum AgentError {
    #[error("{message}")]
    NotRunning { message: String },
    #[error("{message}")]
    Connection { message: String },
    #[error("{message}")]
    Request { message: String },
}

impl From<foldover_client::Error> for AgentError {
    fn from(error: foldover_client::Error) -> Self {
        let message = error.to_string();
        match error {
            foldover_client::Error::Io(error)
                if matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused) => Self::NotRunning { message },
            foldover_client::Error::Remote { .. } => Self::Request { message },
            foldover_client::Error::Timeout => Self::Request {
                message: "Agent request timed out. Refresh pairs before retrying a save; it may have committed.".into(),
            },
            _ => Self::Connection { message },
        }
    }
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn agent_status() -> Result<AgentHealth, AgentError> {
    let status = foldover_client::agent_status(&foldover_client::SocketLocation::Default)
        .await
        .map_err(AgentError::from)?;
    Ok(AgentHealth {
        core_version: status.core_version,
        capability: match status.capability {
            foldover_client::AgentCapability::HealthOnly => AgentCapability::HealthOnly,
            foldover_client::AgentCapability::PairConfiguration => {
                AgentCapability::PairConfiguration
            }
            foldover_client::AgentCapability::Sync => AgentCapability::Sync,
        },
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum PairMode {
    Sync,
    MirrorLeft,
    MirrorRight,
    BackupLeft,
    BackupRight,
}

impl From<PairMode> for foldover_client::SyncMode {
    fn from(mode: PairMode) -> Self {
        use foldover_client::Side;
        match mode {
            PairMode::Sync => Self::Sync,
            PairMode::MirrorLeft => Self::Mirror { source: Side::Left },
            PairMode::MirrorRight => Self::Mirror {
                source: Side::Right,
            },
            PairMode::BackupLeft => Self::Backup { source: Side::Left },
            PairMode::BackupRight => Self::Backup {
                source: Side::Right,
            },
        }
    }
}

impl From<foldover_client::SyncMode> for PairMode {
    fn from(mode: foldover_client::SyncMode) -> Self {
        use foldover_client::{Side, SyncMode};
        match mode {
            SyncMode::Sync => Self::Sync,
            SyncMode::Mirror { source: Side::Left } => Self::MirrorLeft,
            SyncMode::Mirror {
                source: Side::Right,
            } => Self::MirrorRight,
            SyncMode::Backup { source: Side::Left } => Self::BackupLeft,
            SyncMode::Backup {
                source: Side::Right,
            } => Self::BackupRight,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum MetadataSync {
    Sync,
    Ignore,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum WriteDurability {
    /// Survives app and system crashes.
    Standard,
    /// Also survives power loss; slower per file.
    Full,
}

/// Which folders hold Foldover's records (`.foldover`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum FolderRecords {
    Both,
    /// Only a Backup's source: its target cannot hold `.foldover`, and
    /// what a sync replaces there is deleted, not archived.
    SourceOnly,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct LocalPairOptions {
    pub modification_times: MetadataSync,
    pub extended_attributes: MetadataSync,
    pub permissions: MetadataSync,
    pub durability: WriteDurability,
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
    pub canary: Option<Vec<u8>>,
    pub records: FolderRecords,
    /// File data bytes per second the pair's copies share at most.
    pub bandwidth_limit: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum PairSchedule {
    Manual,
    /// Unattended syncs at this interval, after the first manual sync.
    Every {
        minutes: u32,
    },
    /// Unattended syncs after files change, and hourly.
    OnChange,
    /// Unattended syncs at `minutes` after local midnight, on `days`.
    Daily {
        minutes: u32,
        days: Vec<ScheduleDay>,
    },
}

/// A weekday of a `PairSchedule::Daily`, Monday first.
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum ScheduleDay {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl From<ScheduleDay> for foldover_client::Weekday {
    fn from(day: ScheduleDay) -> Self {
        match day {
            ScheduleDay::Monday => Self::Monday,
            ScheduleDay::Tuesday => Self::Tuesday,
            ScheduleDay::Wednesday => Self::Wednesday,
            ScheduleDay::Thursday => Self::Thursday,
            ScheduleDay::Friday => Self::Friday,
            ScheduleDay::Saturday => Self::Saturday,
            ScheduleDay::Sunday => Self::Sunday,
        }
    }
}

impl From<foldover_client::Weekday> for ScheduleDay {
    fn from(day: foldover_client::Weekday) -> Self {
        match day {
            foldover_client::Weekday::Monday => Self::Monday,
            foldover_client::Weekday::Tuesday => Self::Tuesday,
            foldover_client::Weekday::Wednesday => Self::Wednesday,
            foldover_client::Weekday::Thursday => Self::Thursday,
            foldover_client::Weekday::Friday => Self::Friday,
            foldover_client::Weekday::Saturday => Self::Saturday,
            foldover_client::Weekday::Sunday => Self::Sunday,
        }
    }
}

impl From<foldover_client::Propagation> for MetadataSync {
    fn from(value: foldover_client::Propagation) -> Self {
        match value {
            foldover_client::Propagation::Sync => Self::Sync,
            foldover_client::Propagation::Ignore => Self::Ignore,
        }
    }
}

impl From<MetadataSync> for foldover_client::Propagation {
    fn from(value: MetadataSync) -> Self {
        match value {
            MetadataSync::Sync => Self::Sync,
            MetadataSync::Ignore => Self::Ignore,
        }
    }
}

impl From<&foldover_client::PairOptions> for LocalPairOptions {
    fn from(options: &foldover_client::PairOptions) -> Self {
        let metadata = options.metadata();
        Self {
            modification_times: metadata.modification_times().into(),
            extended_attributes: metadata.extended_attributes().into(),
            permissions: metadata.permissions().into(),
            durability: match options.durability() {
                foldover_client::Durability::Standard => WriteDurability::Standard,
                foldover_client::Durability::Full => WriteDurability::Full,
            },
            schedule: match options.schedule() {
                foldover_client::Schedule::Manual => PairSchedule::Manual,
                foldover_client::Schedule::Every(interval) => PairSchedule::Every {
                    minutes: interval.minutes(),
                },
                foldover_client::Schedule::OnChange => PairSchedule::OnChange,
                foldover_client::Schedule::Daily(daily) => PairSchedule::Daily {
                    minutes: daily.at().minutes(),
                    days: daily.days().days().map(ScheduleDay::from).collect(),
                },
            },
            filters: options.filters().rules().to_vec(),
            size_limit: options.filters().size_limit(),
            age_limit: options.filters().age_limit(),
            canary: options.canary().map(|path| path.as_bytes().to_vec()),
            records: match options.records() {
                foldover_client::FolderRecords::Both => FolderRecords::Both,
                foldover_client::FolderRecords::SourceOnly => FolderRecords::SourceOnly,
            },
            bandwidth_limit: options.bandwidth_limit(),
        }
    }
}

impl TryFrom<LocalPairOptions> for foldover_client::PairOptions {
    type Error = foldover_client::InvalidPair;

    fn try_from(options: LocalPairOptions) -> Result<Self, Self::Error> {
        let schedule = match options.schedule {
            PairSchedule::Manual => foldover_client::Schedule::Manual,
            PairSchedule::Every { minutes } => foldover_client::Schedule::Every(
                foldover_client::ScheduleInterval::from_minutes(minutes)?,
            ),
            PairSchedule::OnChange => foldover_client::Schedule::OnChange,
            PairSchedule::Daily { minutes, days } => {
                foldover_client::Schedule::Daily(foldover_client::DailySchedule::new(
                    foldover_client::TimeOfDay::from_minutes(minutes)?,
                    foldover_client::Weekdays::of(
                        &days
                            .iter()
                            .map(|day| foldover_client::Weekday::from(*day))
                            .collect::<Vec<_>>(),
                    )?,
                ))
            }
        };
        Self::new(
            foldover_client::MetadataPolicy::new(
                options.modification_times.into(),
                options.extended_attributes.into(),
                options.permissions.into(),
            ),
            match options.durability {
                WriteDurability::Standard => foldover_client::Durability::Standard,
                WriteDurability::Full => foldover_client::Durability::Full,
            },
        )
        .with_schedule(schedule)
        .with_filters(
            foldover_client::Filters::new(options.filters)?
                .with_size_limit(options.size_limit)?
                .with_age_limit(options.age_limit)?,
        )
        .with_canary(
            options
                .canary
                .map(foldover_client::SyncPath::try_from)
                .transpose()
                .map_err(|_| foldover_client::InvalidPair::Canary)?,
        )
        .map(|converted| {
            converted.with_records(match options.records {
                FolderRecords::Both => foldover_client::FolderRecords::Both,
                FolderRecords::SourceOnly => foldover_client::FolderRecords::SourceOnly,
            })
        })?
        .with_bandwidth_limit(options.bandwidth_limit)
    }
}

/// The engine's defaults, so the app never restates them.
#[uniffi::export]
pub fn default_pair_options() -> LocalPairOptions {
    (&foldover_client::PairOptions::default()).into()
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct LocalPairDraft {
    pub name: String,
    pub left_root: Vec<u8>,
    pub right_root: Vec<u8>,
    pub mode: PairMode,
    pub options: LocalPairOptions,
}

impl From<&foldover_client::PairDraft> for LocalPairDraft {
    fn from(draft: &foldover_client::PairDraft) -> Self {
        Self {
            name: draft.name().to_owned(),
            left_root: draft.left().as_bytes().to_vec(),
            right_root: draft.right().as_bytes().to_vec(),
            mode: draft.mode().into(),
            options: draft.options().into(),
        }
    }
}

impl TryFrom<LocalPairDraft> for foldover_client::PairDraft {
    type Error = foldover_client::InvalidPair;

    fn try_from(draft: LocalPairDraft) -> Result<Self, Self::Error> {
        Ok(Self::new(
            draft.name,
            foldover_client::PairRoot::from_bytes(draft.left_root)?,
            foldover_client::PairRoot::from_bytes(draft.right_root)?,
            draft.mode.into(),
        )?
        .with_options(draft.options.try_into()?))
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct LocalPairConfiguration {
    pub id: String,
    pub revision: u64,
    pub configuration: LocalPairDraft,
}

impl From<foldover_client::PairConfig> for LocalPairConfiguration {
    fn from(pair: foldover_client::PairConfig) -> Self {
        Self {
            id: pair.id().to_string(),
            revision: pair.revision(),
            configuration: pair.draft().into(),
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct PairConfigurationPage {
    pub pairs: Vec<LocalPairConfiguration>,
    pub next: Option<String>,
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn pair_page(after: Option<String>) -> Result<PairConfigurationPage, AgentError> {
    let after = after
        .map(|id| id.parse())
        .transpose()
        .map_err(invalid_configuration)?;
    let page = foldover_client::pairs(&foldover_client::SocketLocation::Default, after)
        .await
        .map_err(AgentError::from)?;
    Ok(PairConfigurationPage {
        pairs: page.pairs.into_iter().map(Into::into).collect(),
        next: page.next.map(|id| id.to_string()),
    })
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn pair_create(
    configuration: LocalPairDraft,
) -> Result<LocalPairConfiguration, AgentError> {
    let draft = configuration.try_into().map_err(invalid_configuration)?;
    Ok(
        foldover_client::create_pair(&foldover_client::SocketLocation::Default, &draft)
            .await
            .map_err(AgentError::from)?
            .into(),
    )
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn pair_update(
    id: String,
    revision: u64,
    configuration: LocalPairDraft,
) -> Result<LocalPairConfiguration, AgentError> {
    let id = id.parse().map_err(invalid_configuration)?;
    let draft = configuration.try_into().map_err(invalid_configuration)?;
    Ok(foldover_client::update_pair(
        &foldover_client::SocketLocation::Default,
        id,
        revision,
        &draft,
    )
    .await
    .map_err(AgentError::from)?
    .into())
}

/// What a readiness check found on one side of a pair (PLAN.md §3.6).
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum ReadinessWarningKind {
    /// Another sync engine manages this folder; `provider` names it.
    CloudManaged,
    /// The pair syncs extended attributes this side cannot keep.
    MetadataUnsupported,
    /// One side ignores name case and the other does not.
    NamesDiffer,
    /// This folder cannot hold Foldover's `.foldover` records, so it can
    /// only be a Backup target that keeps none.
    RecordsUnsupported,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct ReadinessWarning {
    pub side: SyncSide,
    pub kind: ReadinessWarningKind,
    /// The other sync engine's name, for `CloudManaged` only.
    pub provider: Option<String>,
}

impl From<foldover_client::PairWarning> for ReadinessWarning {
    fn from(warning: foldover_client::PairWarning) -> Self {
        use foldover_client::{PairSide, PairWarning as Warning};
        let sync_side = |side: PairSide| match side {
            PairSide::Left => SyncSide::Left,
            PairSide::Right => SyncSide::Right,
        };
        match warning {
            Warning::CloudManaged { side, provider } => Self {
                side: sync_side(side),
                kind: ReadinessWarningKind::CloudManaged,
                provider: Some(provider),
            },
            Warning::MetadataUnsupported { side } => Self {
                side: sync_side(side),
                kind: ReadinessWarningKind::MetadataUnsupported,
                provider: None,
            },
            Warning::NamesDiffer { side } => Self {
                side: sync_side(side),
                kind: ReadinessWarningKind::NamesDiffer,
                provider: None,
            },
            Warning::RecordsUnsupported { side } => Self {
                side: sync_side(side),
                kind: ReadinessWarningKind::RecordsUnsupported,
                provider: None,
            },
        }
    }
}

/// What will not work as a person might expect with `configuration`'s
/// folders (PLAN.md §3.6). Warnings never block saving.
#[uniffi::export(async_runtime = "tokio")]
pub async fn check_pair(
    configuration: LocalPairDraft,
) -> Result<Vec<ReadinessWarning>, AgentError> {
    let draft = configuration.try_into().map_err(invalid_configuration)?;
    let warnings = foldover_client::check_pair(&foldover_client::SocketLocation::Default, &draft)
        .await
        .map_err(AgentError::from)?;
    Ok(warnings.into_iter().map(Into::into).collect())
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn pair_remove(id: String, revision: u64) -> Result<LocalPairConfiguration, AgentError> {
    let id = id.parse().map_err(invalid_configuration)?;
    Ok(
        foldover_client::remove_pair(&foldover_client::SocketLocation::Default, id, revision)
            .await
            .map_err(AgentError::from)?
            .into(),
    )
}

fn invalid_configuration(error: foldover_client::InvalidPair) -> AgentError {
    AgentError::Request {
        message: error.to_string(),
    }
}

uniffi::setup_scaffolding!();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swift_configuration_round_trips_modes_and_raw_root_bytes() {
        for mode in [
            PairMode::Sync,
            PairMode::MirrorLeft,
            PairMode::MirrorRight,
            PairMode::BackupLeft,
            PairMode::BackupRight,
        ] {
            let draft = LocalPairDraft {
                name: "Photos".into(),
                left_root: b"/a/\xff".to_vec(),
                right_root: b"/b".to_vec(),
                mode,
                options: LocalPairOptions {
                    modification_times: MetadataSync::Ignore,
                    extended_attributes: MetadataSync::Sync,
                    permissions: MetadataSync::Sync,
                    durability: WriteDurability::Full,
                    schedule: PairSchedule::OnChange,
                    filters: vec!["*.tmp".into(), "build/".into()],
                    size_limit: Some(1_000_000),
                    age_limit: Some(365),
                    canary: Some(b"docs/canary.txt".to_vec()),
                    records: FolderRecords::SourceOnly,
                    bandwidth_limit: Some(2_000_000),
                },
            };
            let domain = foldover_client::PairDraft::try_from(draft.clone()).unwrap();
            assert_eq!(LocalPairDraft::from(&domain), draft);
        }
        let invalid = LocalPairDraft {
            name: "Photos".into(),
            left_root: b"relative".to_vec(),
            right_root: b"/b".to_vec(),
            mode: PairMode::Sync,
            options: default_pair_options(),
        };
        assert!(foldover_client::PairDraft::try_from(invalid).is_err());
    }

    #[test]
    fn readiness_warnings_keep_side_kind_and_provider() {
        use foldover_client::{PairSide, PairWarning};
        for (warning, expected) in [
            (
                PairWarning::CloudManaged {
                    side: PairSide::Left,
                    provider: "iCloud Drive".into(),
                },
                ReadinessWarning {
                    side: SyncSide::Left,
                    kind: ReadinessWarningKind::CloudManaged,
                    provider: Some("iCloud Drive".into()),
                },
            ),
            (
                PairWarning::MetadataUnsupported {
                    side: PairSide::Right,
                },
                ReadinessWarning {
                    side: SyncSide::Right,
                    kind: ReadinessWarningKind::MetadataUnsupported,
                    provider: None,
                },
            ),
            (
                PairWarning::NamesDiffer {
                    side: PairSide::Left,
                },
                ReadinessWarning {
                    side: SyncSide::Left,
                    kind: ReadinessWarningKind::NamesDiffer,
                    provider: None,
                },
            ),
            (
                PairWarning::RecordsUnsupported {
                    side: PairSide::Right,
                },
                ReadinessWarning {
                    side: SyncSide::Right,
                    kind: ReadinessWarningKind::RecordsUnsupported,
                    provider: None,
                },
            ),
        ] {
            assert_eq!(ReadinessWarning::from(warning), expected);
        }
    }

    #[test]
    fn core_version_is_the_package_version() {
        assert_eq!(super::core_version(), env!("CARGO_PKG_VERSION"));
    }
}
