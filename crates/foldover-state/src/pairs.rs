use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::ffi::OsStrExt;

pub use foldover_db::Purge;
use foldover_db::{
    BaseChange, BaseRow, Database, PairRow, PairValues, ReadView, ReadViewError, ResolutionRow,
    SnapshotData, SnapshotRow, StateLocation,
};
use foldover_types::{
    BaselineEntry, DailySchedule, Durability, EndpointId, Filters, FolderRecords, InvalidPair,
    LocalRoot, MetadataPolicy, PairConfig, PairDraft, PairId, PairOptions, PairRoot, Propagation,
    RemovalProtection, Resolution, ReviewPolicy, Schedule, ScheduleInterval, Side, SyncMode,
    SyncPath, TimeOfDay, Weekdays,
};

use crate::BaseUpdate;
use crate::base::{VERSION_BYTES, decode, decode_entry, encode};

mod cursor;
mod device;
mod history;
mod journal;
mod mounts;
mod pause;
mod settings;
mod share;
mod starts;
pub use cursor::WatchCursor;
pub use device::Device;
pub use history::{HISTORY_LIMIT, HistoryEntry, HistoryOutcome, RunTrigger};
pub use journal::{Journal, JournalAction, JournalEntry, JournalUpdate};
pub use mounts::MountSources;
pub use pause::Pause;
pub use settings::LowPowerSyncs;
pub use share::Share;
pub use starts::RunMarker;

const SYNC: &str = "sync";
const MIRROR_LEFT: &str = "mirror-left";
const MIRROR_RIGHT: &str = "mirror-right";
const BACKUP_LEFT: &str = "backup-left";
const BACKUP_RIGHT: &str = "backup-right";
const SYNC_CATEGORY: &str = "sync";
const IGNORE_CATEGORY: &str = "ignore";
const STANDARD_DURABILITY: &str = "standard";
const MANUAL_SCHEDULE: &str = "manual";
const INTERVAL_SCHEDULE: &str = "every";
const CHANGE_SCHEDULE: &str = "changes";
const DAILY_SCHEDULE: &str = "daily";
const FULL_DURABILITY: &str = "full";
const RECORDS_IN_BOTH: &str = "both";
const RECORDS_IN_SOURCE: &str = "source";
const REVIEW_AUTOMATIC: &str = "automatic";
const REVIEW_REQUIRED: &str = "required";
const REMOVAL_ENABLED: &str = "enabled";
const REMOVAL_DISABLED: &str = "disabled";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Storage(foldover_db::Error),
    #[error(transparent)]
    InvalidPair(#[from] InvalidPair),
    #[error("Stored pair configuration is invalid: {0}")]
    InvalidStored(&'static str),
    #[error("Pair does not exist")]
    PairNotFound,
    #[error("Pair configuration changed; refresh before updating it")]
    StaleRevision,
}

impl From<foldover_db::Error> for Error {
    fn from(error: foldover_db::Error) -> Self {
        match error {
            foldover_db::Error::PairNotFound => Self::PairNotFound,
            foldover_db::Error::StaleRevision => Self::StaleRevision,
            error => Self::Storage(error),
        }
    }
}

pub struct PairStore {
    database: Database,
}

impl PairStore {
    pub fn open(location: &StateLocation) -> Result<Self, Error> {
        Ok(Self {
            database: Database::open(location)?,
        })
    }

    pub fn create(&mut self, draft: &PairDraft) -> Result<PairConfig, Error> {
        from_row(self.database.create_pair(&values(draft))?)
    }

    pub fn root(&self) -> Result<LocalRoot, Error> {
        Ok(LocalRoot::from_bytes(
            self.database.directory().as_os_str().as_bytes().to_vec(),
        )?)
    }

    pub fn pairs(&self) -> Result<Vec<PairConfig>, Error> {
        self.database.pairs()?.into_iter().map(from_row).collect()
    }

    pub fn pair(&self, id: PairId) -> Result<Option<PairConfig>, Error> {
        self.database.pair(id.as_bytes())?.map(from_row).transpose()
    }

    /// The configuration an already-running job is using.
    pub fn active_pair(&self, id: PairId) -> Result<Option<PairConfig>, Error> {
        self.database
            .active_pair(id.as_bytes())?
            .map(from_row)
            .transpose()
    }

    /// Save edits while an old revision's run is still committing progress.
    pub fn defer_update(
        &mut self,
        id: PairId,
        revision: u64,
        draft: &PairDraft,
    ) -> Result<PairConfig, Error> {
        let revision = database_revision(revision)?;
        from_row(
            self.database
                .defer_pair_update(id.as_bytes(), revision, &values(draft))?,
        )
    }

    /// Activate saved edits before a new run reads its inputs.
    pub fn activate_update(&mut self, id: PairId) -> Result<(), Error> {
        self.pair(id)?.ok_or(Error::PairNotFound)?;
        Ok(self.database.activate_pair_update(id.as_bytes())?)
    }

    /// After restart there are no old runs to retain an active configuration.
    pub fn activate_updates(&mut self) -> Result<(), Error> {
        self.pairs()?;
        Ok(self.database.activate_pair_updates()?)
    }

    pub fn pairs_after(
        &self,
        after: Option<PairId>,
        limit: usize,
    ) -> Result<Vec<PairConfig>, Error> {
        self.database
            .pairs_after(after.as_ref().map(|id| id.as_bytes().as_slice()), limit)?
            .into_iter()
            .map(from_row)
            .collect()
    }

    pub fn update(
        &mut self,
        id: PairId,
        revision: u64,
        draft: &PairDraft,
    ) -> Result<PairConfig, Error> {
        let revision = database_revision(revision)?;
        from_row(
            self.database
                .update_pair(id.as_bytes(), revision, &values(draft))?,
        )
    }

    /// Remove a pair at once; `purge_removed` deletes its rows later.
    pub fn remove(&mut self, id: PairId, revision: u64) -> Result<PairConfig, Error> {
        let revision = database_revision(revision)?;
        // Decode before deleting: corrupt domain records must not disappear silently.
        let current = self.pair(id)?.ok_or(Error::PairNotFound)?;
        self.database.remove_pair(id.as_bytes(), revision)?;
        Ok(current)
    }

    /// Delete up to `limit` rows of removed pairs, in one transaction.
    pub fn purge_removed(&mut self, limit: usize) -> Result<Purge, Error> {
        Ok(self.database.purge_removed_pairs(limit)?)
    }

    /// Whether removed pairs still have rows to purge.
    pub fn removals_pending(&self) -> Result<bool, Error> {
        Ok(self.database.has_removed_pairs()?)
    }

    /// Pause the pair's automatic syncs until it is resumed.
    pub fn pause_pair(&mut self, id: PairId) -> Result<(), Error> {
        Ok(self.database.pause_pair(id.as_bytes())?)
    }

    pub fn resume_pair(&mut self, id: PairId) -> Result<(), Error> {
        Ok(self.database.resume_pair(id.as_bytes())?)
    }

    /// Every paused pair.
    pub fn paused_pairs(&self) -> Result<BTreeSet<PairId>, Error> {
        self.database
            .paused_pairs()?
            .into_iter()
            .map(|id| {
                let bytes = id
                    .try_into()
                    .map_err(|_| Error::InvalidStored("Paused pair ID"))?;
                Ok(PairId::from_bytes(bytes)?)
            })
            .collect()
    }
}

/// Endpoint identities recorded on a pair's first sync.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RecordedIdentities {
    left: Option<EndpointId>,
    right: Option<EndpointId>,
}

impl RecordedIdentities {
    pub const fn new(left: Option<EndpointId>, right: Option<EndpointId>) -> Self {
        Self { left, right }
    }

    pub fn side(self, side: Side) -> Option<EndpointId> {
        match side {
            Side::Left => self.left,
            Side::Right => self.right,
        }
    }
}

impl PairStore {
    pub fn identities(&self, id: PairId) -> Result<RecordedIdentities, Error> {
        let decode = |bytes: Option<Vec<u8>>| {
            bytes
                .map(|bytes| {
                    bytes
                        .try_into()
                        .map(EndpointId::from_bytes)
                        .map_err(|_| Error::InvalidStored("endpoint identity"))
                })
                .transpose()
        };
        let row = self.database.identities(id.as_bytes())?;
        Ok(RecordedIdentities::new(
            decode(row.left)?,
            decode(row.right)?,
        ))
    }

    /// Record both identities, only while the pair keeps `revision`.
    pub fn record_identities(
        &mut self,
        id: PairId,
        revision: u64,
        left: EndpointId,
        right: EndpointId,
    ) -> Result<(), Error> {
        let revision = database_revision(revision)?;
        Ok(self.database.record_identities(
            id.as_bytes(),
            revision,
            left.as_bytes(),
            right.as_bytes(),
        )?)
    }
}

const KEEP_LEFT: &str = "left";
const KEEP_RIGHT: &str = "right";

/// One side's stored observation of a path. The fingerprint is opaque here;
/// the endpoint that wrote it interprets it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotEntry {
    pub path: SyncPath,
    pub fingerprint: Vec<u8>,
    pub version: foldover_types::EntryVersion,
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Left => KEEP_LEFT,
        Side::Right => KEEP_RIGHT,
    }
}

impl PairStore {
    pub fn snapshot(&self, id: PairId, side: Side) -> Result<Vec<SnapshotEntry>, Error> {
        decode_snapshot_rows(self.database.snapshot(id.as_bytes(), side_name(side))?)
    }

    /// Replace one side's observation, only while the pair keeps `revision`.
    pub fn replace_snapshot(
        &mut self,
        id: PairId,
        revision: u64,
        side: Side,
        entries: &[SnapshotEntry],
    ) -> Result<(), Error> {
        let revision = database_revision(revision)?;
        let rows: Vec<_> = entries
            .iter()
            .map(|entry| EncodedSnapshot {
                entry,
                version: encode(entry.version),
            })
            .collect();
        Ok(self
            .database
            .replace_snapshot_data(id.as_bytes(), revision, side_name(side), &rows)?)
    }

    /// Replace one side's observation from rows already sorted by path with
    /// no duplicates, as produced by a run's effective snapshot iterator.
    /// Each row is encoded once into an owned entry, so no whole-tree vector
    /// is built between the run and the database.
    pub fn replace_snapshot_iter<I>(
        &mut self,
        id: PairId,
        revision: u64,
        side: Side,
        entries: I,
    ) -> Result<(), Error>
    where
        I: IntoIterator<Item = SnapshotEntry>,
    {
        let revision = database_revision(revision)?;
        let rows = entries.into_iter().map(|entry| EncodedSnapshotOwned {
            path: entry.path,
            fingerprint: entry.fingerprint,
            version: encode(entry.version),
        });
        Ok(self
            .database
            .replace_snapshot_sorted(id.as_bytes(), revision, side_name(side), rows)?)
    }
}

struct EncodedSnapshot<'a> {
    entry: &'a SnapshotEntry,
    version: [u8; VERSION_BYTES],
}

impl SnapshotData for EncodedSnapshot<'_> {
    fn path(&self) -> &[u8] {
        self.entry.path.as_bytes()
    }
    fn fingerprint(&self) -> &[u8] {
        &self.entry.fingerprint
    }
    fn version(&self) -> &[u8] {
        &self.version
    }
}

/// An owned snapshot row: the validated path and opaque fingerprint are kept
/// as-is, and only the version is encoded into its stored fixed-width form.
struct EncodedSnapshotOwned {
    path: SyncPath,
    fingerprint: Vec<u8>,
    version: [u8; VERSION_BYTES],
}

impl SnapshotData for EncodedSnapshotOwned {
    fn path(&self) -> &[u8] {
        self.path.as_bytes()
    }
    fn fingerprint(&self) -> &[u8] {
        &self.fingerprint
    }
    fn version(&self) -> &[u8] {
        &self.version
    }
}

fn decode_base_row(row: BaseRow) -> Result<(SyncPath, BaselineEntry), Error> {
    let path = SyncPath::try_from(row.path).map_err(|_| Error::InvalidStored("base path"))?;
    Ok((path, decode_entry(&row.left_version, &row.right_version)?))
}

fn decode_snapshot_row(row: SnapshotRow) -> Option<Result<SnapshotEntry, Error>> {
    let path = match SyncPath::try_from(row.path) {
        Ok(path) => path,
        Err(_) => return Some(Err(Error::InvalidStored("snapshot path"))),
    };
    // Versions here are cache hints, not the authoritative base.
    // A corrupt hint must allow the full observation that replaces it.
    let version = decode(&row.version).ok()?;
    Some(Ok(SnapshotEntry {
        path,
        fingerprint: row.fingerprint,
        version,
    }))
}

fn decode_snapshot_rows(rows: Vec<SnapshotRow>) -> Result<Vec<SnapshotEntry>, Error> {
    rows.into_iter().filter_map(decode_snapshot_row).collect()
}

impl PairStore {
    /// Conflict choices not yet applied, keyed by path.
    pub fn resolutions(&self, id: PairId) -> Result<BTreeMap<SyncPath, Resolution>, Error> {
        self.database
            .resolutions(id.as_bytes())?
            .into_iter()
            .map(|row| {
                let path = SyncPath::try_from(row.path)
                    .map_err(|_| Error::InvalidStored("resolution path"))?;
                let keep = match row.keep.as_str() {
                    KEEP_LEFT => Side::Left,
                    KEEP_RIGHT => Side::Right,
                    _ => return Err(Error::InvalidStored("resolution side")),
                };
                let version = |bytes: Option<Vec<u8>>| bytes.as_deref().map(decode).transpose();
                let resolution = Resolution::new(
                    keep,
                    version(row.left_version)?,
                    version(row.right_version)?,
                );
                Ok((path, resolution))
            })
            .collect()
    }

    /// Store a conflict choice, only while the pair keeps `revision`.
    pub fn record_resolution(
        &mut self,
        id: PairId,
        revision: u64,
        path: &SyncPath,
        resolution: Resolution,
    ) -> Result<(), Error> {
        let revision = database_revision(revision)?;
        let version = |version: Option<foldover_types::EntryVersion>| {
            version.map(|version| encode(version).to_vec())
        };
        let row = ResolutionRow {
            path: path.as_bytes().to_vec(),
            keep: match resolution.keep() {
                Side::Left => KEEP_LEFT,
                Side::Right => KEEP_RIGHT,
            }
            .to_owned(),
            left_version: version(resolution.left()),
            right_version: version(resolution.right()),
        };
        Ok(self
            .database
            .record_resolution(id.as_bytes(), revision, &row)?)
    }
}

impl PairStore {
    /// The pair's last-synced state. An unknown pair has an empty base.
    pub fn base(&self, id: PairId) -> Result<BTreeMap<SyncPath, BaselineEntry>, Error> {
        self.database
            .base_rows(id.as_bytes())?
            .into_iter()
            .map(decode_base_row)
            .collect()
    }

    fn change_base(
        &mut self,
        id: PairId,
        revision: i64,
        updates: &[BaseUpdate],
        journal: foldover_db::JournalChange<'_>,
    ) -> Result<(), Error> {
        let encoded: Vec<_> = updates
            .iter()
            .map(|update| match update {
                BaseUpdate::Record(path, entry) => {
                    (path, Some((encode(entry.left()), encode(entry.right()))))
                }
                BaseUpdate::Forget(path) => (path, None),
            })
            .collect();
        let changes: Vec<_> = encoded
            .iter()
            .map(|(path, versions)| match versions {
                Some((left, right)) => BaseChange::Put {
                    path: path.as_bytes(),
                    left_version: left,
                    right_version: right,
                },
                None => BaseChange::Remove {
                    path: path.as_bytes(),
                },
            })
            .collect();
        self.database
            .change_base(id.as_bytes(), revision, &changes, journal)?;
        Ok(())
    }

    /// Capture one run's stored base and snapshot rows together, so the
    /// engine can decode them outside the lock that captured them. The
    /// view pins the pair's current revision and keeps the state's single
    /// writer lease until it drops.
    pub fn run_records(&self, id: PairId) -> Result<RunRecords, Error> {
        // A run reads the active revision's base and caches; a saved
        // replacement waits for the next run after activation.
        let revision = self
            .database
            .active_pair(id.as_bytes())?
            .ok_or(Error::PairNotFound)?
            .revision;
        Ok(RunRecords {
            view: self.database.read_view(id.as_bytes(), revision)?,
        })
    }
}

/// A run's stored base and snapshot caches, pinned at one revision under
/// the state's single writer. The view holds a clone of the writer lease,
/// so the rows stay readable after the store closes and no later write can
/// change them.
pub struct RunRecords {
    view: ReadView,
}

impl RunRecords {
    /// Capacity for the stored base, counted from the same pinned view.
    pub fn base_len(&self) -> Result<usize, Error> {
        Ok(self.view.base_len()?)
    }

    /// Capacity for one stored snapshot. Malformed cache rows may be
    /// omitted when visited, so this is an upper bound on decoded rows.
    pub fn snapshot_len(&self, side: Side) -> Result<usize, Error> {
        Ok(self.view.snapshot_len(side_name(side))?)
    }

    /// Visit the run's base rows in stored path order. Malformed paths and
    /// base versions stay errors, as in [`PairStore::base`].
    pub fn visit_base<F>(&self, mut visit: F) -> Result<(), Error>
    where
        F: FnMut(SyncPath, BaselineEntry),
    {
        self.view
            .base_rows(|row| {
                let path =
                    SyncPath::try_from(row.path).map_err(|_| Error::InvalidStored("base path"))?;
                let entry = decode_entry(row.left_version, row.right_version)?;
                visit(path, entry);
                Ok::<_, Error>(())
            })
            .map_err(read_view_error)?;
        Ok(())
    }

    /// Visit one side's snapshot rows in stored path order. A malformed
    /// snapshot version is a cache miss, as in [`PairStore::snapshot`];
    /// malformed paths stay errors.
    pub fn visit_snapshot<F>(&self, side: Side, mut visit: F) -> Result<(), Error>
    where
        F: FnMut(SnapshotEntry),
    {
        self.view
            .snapshot(side_name(side), |row| {
                let path = SyncPath::try_from(row.path)
                    .map_err(|_| Error::InvalidStored("snapshot path"))?;
                // Versions here are cache hints, not the authoritative base.
                // A corrupt hint must allow the full observation that replaces it.
                let Some(version) = decode(row.version).ok() else {
                    return Ok(());
                };
                visit(SnapshotEntry {
                    path,
                    fingerprint: row.fingerprint.to_vec(),
                    version,
                });
                Ok::<_, Error>(())
            })
            .map_err(read_view_error)?;
        Ok(())
    }

    /// Decode the captured rows. A malformed snapshot version is a cache
    /// miss, as in [`PairStore::snapshot`]; malformed paths and base
    /// versions stay errors.
    pub fn decode(self) -> Result<RunRecordInputs, Error> {
        let mut base = BTreeMap::new();
        self.visit_base(|path, entry| {
            base.insert(path, entry);
        })?;
        let mut left_snapshot = Vec::new();
        self.visit_snapshot(Side::Left, |mut entry| {
            if let Some((path, _)) = base.get_key_value(&entry.path) {
                entry.path = path.clone();
            }
            left_snapshot.push(entry);
        })?;
        let mut right_snapshot = Vec::new();
        self.visit_snapshot(Side::Right, |mut entry| {
            if let Some((path, _)) = base.get_key_value(&entry.path) {
                entry.path = path.clone();
            }
            right_snapshot.push(entry);
        })?;
        Ok(RunRecordInputs {
            base,
            left_snapshot,
            right_snapshot,
        })
    }
}

fn read_view_error(error: ReadViewError<Error>) -> Error {
    match error {
        ReadViewError::Driver(error) => Error::from(error),
        ReadViewError::Visitor(error) => error,
    }
}

/// Decoded base and snapshot rows of one captured run.
pub struct RunRecordInputs {
    pub base: BTreeMap<SyncPath, BaselineEntry>,
    pub left_snapshot: Vec<SnapshotEntry>,
    pub right_snapshot: Vec<SnapshotEntry>,
}

fn database_revision(revision: u64) -> Result<i64, InvalidPair> {
    i64::try_from(revision)
        .ok()
        .filter(|revision| *revision > 0)
        .ok_or(InvalidPair::Revision)
}

fn values(draft: &PairDraft) -> PairValues {
    let mode = match draft.mode() {
        SyncMode::Sync => SYNC,
        SyncMode::Mirror { source: Side::Left } => MIRROR_LEFT,
        SyncMode::Mirror {
            source: Side::Right,
        } => MIRROR_RIGHT,
        SyncMode::Backup { source: Side::Left } => BACKUP_LEFT,
        SyncMode::Backup {
            source: Side::Right,
        } => BACKUP_RIGHT,
    };
    let options = draft.options();
    let metadata = options.metadata();
    PairValues {
        name: draft.name().to_owned(),
        left_root: draft.left().as_bytes().to_vec(),
        right_root: draft.right().as_bytes().to_vec(),
        mode: mode.to_owned(),
        modification_times: propagation(metadata.modification_times()).to_owned(),
        extended_attributes: propagation(metadata.extended_attributes()).to_owned(),
        permissions: propagation(metadata.permissions()).to_owned(),
        durability: match options.durability() {
            Durability::Standard => STANDARD_DURABILITY,
            Durability::Full => FULL_DURABILITY,
        }
        .to_owned(),
        schedule: match options.schedule() {
            Schedule::Manual => MANUAL_SCHEDULE,
            Schedule::Every(_) => INTERVAL_SCHEDULE,
            Schedule::OnChange => CHANGE_SCHEDULE,
            Schedule::Daily(_) => DAILY_SCHEDULE,
        }
        .to_owned(),
        schedule_minutes: match options.schedule() {
            Schedule::Every(interval) => Some(i64::from(interval.minutes())),
            _ => None,
        },
        schedule_time: match options.schedule() {
            Schedule::Daily(daily) => Some(i64::from(daily.at().minutes())),
            _ => None,
        },
        schedule_days: match options.schedule() {
            Schedule::Daily(daily) => Some(i64::from(daily.days().bits())),
            _ => None,
        },
        filters: options.filters().rules().join("\n"),
        size_limit: options
            .filters()
            .size_limit()
            .map(|bytes| i64::try_from(bytes).unwrap_or(i64::MAX)),
        age_limit: options.filters().age_limit().map(i64::from),
        canary: options.canary().map(|path| path.as_bytes().to_vec()),
        records: match options.records() {
            FolderRecords::Both => RECORDS_IN_BOTH,
            FolderRecords::SourceOnly => RECORDS_IN_SOURCE,
        }
        .to_owned(),
        bandwidth_limit: options
            .bandwidth_limit()
            .map(|rate| i64::try_from(rate).unwrap_or(i64::MAX)),
        review_policy: match options.review_policy() {
            ReviewPolicy::Automatic => REVIEW_AUTOMATIC,
            ReviewPolicy::Required => REVIEW_REQUIRED,
        }
        .to_owned(),
        removal_protection: match options.removal_protection() {
            RemovalProtection::Enabled => REMOVAL_ENABLED,
            RemovalProtection::Disabled => REMOVAL_DISABLED,
        }
        .to_owned(),
    }
}

fn propagation(value: Propagation) -> &'static str {
    match value {
        Propagation::Sync => SYNC_CATEGORY,
        Propagation::Ignore => IGNORE_CATEGORY,
    }
}

fn stored_propagation(value: &str) -> Result<Propagation, Error> {
    match value {
        SYNC_CATEGORY => Ok(Propagation::Sync),
        IGNORE_CATEGORY => Ok(Propagation::Ignore),
        _ => Err(Error::InvalidStored("metadata category")),
    }
}

fn from_row(row: PairRow) -> Result<PairConfig, Error> {
    let id = PairId::from_bytes(
        row.id
            .try_into()
            .map_err(|_| Error::InvalidStored("identity length"))?,
    )?;
    let mode = match row.values.mode.as_str() {
        SYNC => SyncMode::Sync,
        MIRROR_LEFT => SyncMode::Mirror { source: Side::Left },
        MIRROR_RIGHT => SyncMode::Mirror {
            source: Side::Right,
        },
        BACKUP_LEFT => SyncMode::Backup { source: Side::Left },
        BACKUP_RIGHT => SyncMode::Backup {
            source: Side::Right,
        },
        _ => return Err(Error::InvalidStored("mode")),
    };
    let metadata = MetadataPolicy::new(
        stored_propagation(&row.values.modification_times)?,
        stored_propagation(&row.values.extended_attributes)?,
        stored_propagation(&row.values.permissions)?,
    );
    let schedule = match (
        row.values.schedule.as_str(),
        row.values.schedule_minutes,
        row.values.schedule_time,
        row.values.schedule_days,
    ) {
        (MANUAL_SCHEDULE, None, None, None) => Schedule::Manual,
        (CHANGE_SCHEDULE, None, None, None) => Schedule::OnChange,
        (INTERVAL_SCHEDULE, Some(minutes), None, None) => {
            Schedule::Every(ScheduleInterval::from_minutes(
                u32::try_from(minutes).map_err(|_| Error::InvalidStored("schedule"))?,
            )?)
        }
        (DAILY_SCHEDULE, None, Some(time), Some(days)) => Schedule::Daily(DailySchedule::new(
            TimeOfDay::from_minutes(
                u32::try_from(time).map_err(|_| Error::InvalidStored("schedule"))?,
            )?,
            Weekdays::from_bits(u8::try_from(days).map_err(|_| Error::InvalidStored("schedule"))?)?,
        )),
        _ => return Err(Error::InvalidStored("schedule")),
    };
    // Rules are single lines, so an empty column means no rules.
    let size_limit = row
        .values
        .size_limit
        .map(|bytes| u64::try_from(bytes).map_err(|_| Error::InvalidStored("size limit")))
        .transpose()?;
    let filters = Filters::new(match row.values.filters.as_str() {
        "" => Vec::new(),
        text => text.split('\n').map(str::to_owned).collect(),
    })?
    .with_size_limit(size_limit)?
    .with_age_limit(
        row.values
            .age_limit
            .map(|days| u32::try_from(days).map_err(|_| Error::InvalidStored("age limit")))
            .transpose()?,
    )?;
    let durability = match row.values.durability.as_str() {
        STANDARD_DURABILITY => Durability::Standard,
        FULL_DURABILITY => Durability::Full,
        _ => return Err(Error::InvalidStored("durability")),
    };
    let records = match row.values.records.as_str() {
        RECORDS_IN_BOTH => FolderRecords::Both,
        RECORDS_IN_SOURCE => FolderRecords::SourceOnly,
        _ => return Err(Error::InvalidStored("records")),
    };
    let canary = row
        .values
        .canary
        .map(|bytes| SyncPath::try_from(bytes).map_err(|_| Error::InvalidStored("canary")))
        .transpose()?;
    let bandwidth_limit = row
        .values
        .bandwidth_limit
        .map(|rate| u64::try_from(rate).map_err(|_| Error::InvalidStored("bandwidth limit")))
        .transpose()?;
    let review_policy = match row.values.review_policy.as_str() {
        REVIEW_AUTOMATIC => ReviewPolicy::Automatic,
        REVIEW_REQUIRED => ReviewPolicy::Required,
        _ => return Err(Error::InvalidStored("review policy")),
    };
    let removal_protection = match row.values.removal_protection.as_str() {
        REMOVAL_ENABLED => RemovalProtection::Enabled,
        REMOVAL_DISABLED => RemovalProtection::Disabled,
        _ => return Err(Error::InvalidStored("removal protection")),
    };
    let draft = PairDraft::new(
        row.values.name,
        PairRoot::from_bytes(row.values.left_root)?,
        PairRoot::from_bytes(row.values.right_root)?,
        mode,
    )?
    .with_options(
        PairOptions::new(metadata, durability)
            .with_schedule(schedule)
            .with_filters(filters)
            .with_canary(canary)?
            .with_records(records)
            .with_bandwidth_limit(bandwidth_limit)?
            .with_review_policy(review_policy)
            .with_removal_protection(removal_protection),
    );
    let revision = u64::try_from(row.revision).map_err(|_| Error::InvalidStored("revision"))?;
    Ok(PairConfig::from_stored(id, revision, draft)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    pub(super) fn location() -> (tempfile::TempDir, StateLocation) {
        let directory = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let location = StateLocation::Directory(directory.path().to_owned());
        (directory, location)
    }

    fn draft(mode: SyncMode) -> PairDraft {
        PairDraft::new(
            "Photos".into(),
            LocalRoot::from_bytes(b"/left/\xff".to_vec()).unwrap(),
            LocalRoot::from_bytes(b"/right".to_vec()).unwrap(),
            mode,
        )
        .unwrap()
    }

    #[test]
    fn every_mode_and_raw_root_survives_reopen() {
        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let mut saved = Vec::new();
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
            saved.push(store.create(&draft(mode)).unwrap());
        }
        drop(store);
        let store = PairStore::open(&location).unwrap();
        assert_eq!(store.pairs().unwrap().len(), saved.len());
        for pair in saved {
            assert_eq!(store.pair(pair.id()).unwrap(), Some(pair));
        }
    }

    #[test]
    fn sftp_roots_survive_creation_updates_and_reopening() {
        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let root = |bytes: &[u8]| PairRoot::from_bytes(bytes.to_vec()).unwrap();
        let local = root(b"/local/\xff");
        let remote = root(b"sftp://alice@nas:2222/~/photos/\xff");
        let other = root(b"sftp://backup/srv/photos");
        let mut saved = Vec::new();
        for (left, right) in [
            (remote.clone(), local.clone()),
            (local, remote.clone()),
            (remote, other.clone()),
        ] {
            let draft = PairDraft::new("Server".into(), left, right, SyncMode::Sync).unwrap();
            saved.push(store.create(&draft).unwrap());
        }

        let original = &saved[0];
        let replacement = PairDraft::new(
            "Edited server".into(),
            other,
            original.draft().right().clone(),
            SyncMode::Backup { source: Side::Left },
        )
        .unwrap();
        let updated = store
            .update(original.id(), original.revision(), &replacement)
            .unwrap();
        assert_eq!(updated.revision(), original.revision() + 1);
        assert!(matches!(
            store.update(original.id(), original.revision(), original.draft()),
            Err(Error::StaleRevision)
        ));
        saved[0] = updated;

        drop(store);
        let store = PairStore::open(&location).unwrap();
        assert_eq!(store.pairs().unwrap().len(), saved.len());
        for pair in saved {
            assert_eq!(store.pair(pair.id()).unwrap(), Some(pair));
        }
    }

    #[test]
    fn stale_updates_do_not_replace_new_configuration() {
        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let original = store.create(&draft(SyncMode::Sync)).unwrap();
        let replacement = draft(SyncMode::Backup {
            source: Side::Right,
        });
        let updated = store
            .update(original.id(), original.revision(), &replacement)
            .unwrap();
        assert_eq!(updated.revision(), original.revision() + 1);
        assert!(matches!(
            store.update(original.id(), original.revision(), original.draft()),
            Err(Error::StaleRevision)
        ));
        assert_eq!(store.pair(original.id()).unwrap(), Some(updated));
    }

    #[test]
    fn saved_settings_survive_reopen_without_switching_the_active_pair() {
        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let original = store.create(&draft(SyncMode::Sync)).unwrap();
        let replacement = PairDraft::new(
            "Next home backup".into(),
            LocalRoot::from_bytes(b"/home/\xff".to_vec()).unwrap(),
            PairRoot::from_bytes(b"sftp://nas/backup".to_vec()).unwrap(),
            SyncMode::Backup { source: Side::Left },
        )
        .unwrap()
        .with_options(
            PairOptions::default()
                .with_filters(Filters::new(vec!["/Library/Caches/".into()]).unwrap()),
        );
        let pending = store
            .defer_update(original.id(), original.revision(), &replacement)
            .unwrap();
        assert_eq!(pending.revision(), 2);
        assert_eq!(
            store.active_pair(original.id()).unwrap(),
            Some(original.clone())
        );
        assert_eq!(store.pair(original.id()).unwrap(), Some(pending.clone()));
        drop(store);

        let mut store = PairStore::open(&location).unwrap();
        assert_eq!(store.pair(original.id()).unwrap(), Some(pending.clone()));
        assert_eq!(store.active_pair(original.id()).unwrap(), Some(original));
        store.activate_updates().unwrap();
        assert_eq!(
            store.active_pair(pending.id()).unwrap(),
            Some(pending.clone())
        );
        assert_eq!(store.pairs().unwrap(), [pending]);
    }

    #[test]
    fn invalid_stored_domain_values_are_errors_not_silently_skipped() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        database
            .create_pair(&PairValues {
                name: "\u{2003}".into(),
                left_root: b"/left".to_vec(),
                right_root: b"/right".to_vec(),
                mode: SYNC.into(),
                modification_times: SYNC_CATEGORY.into(),
                extended_attributes: SYNC_CATEGORY.into(),
                permissions: IGNORE_CATEGORY.into(),
                durability: STANDARD_DURABILITY.into(),
                schedule: MANUAL_SCHEDULE.into(),
                schedule_minutes: None,
                filters: String::new(),
                size_limit: None,
                age_limit: None,
                canary: None,
                schedule_time: None,
                schedule_days: None,
                records: RECORDS_IN_BOTH.into(),
                bandwidth_limit: None,
                review_policy: REVIEW_AUTOMATIC.into(),
                removal_protection: REMOVAL_ENABLED.into(),
            })
            .unwrap();
        drop(database);
        let store = PairStore::open(&location).unwrap();
        assert!(matches!(
            store.pairs(),
            Err(Error::InvalidPair(InvalidPair::Name))
        ));
    }

    #[test]
    fn removal_does_not_silently_discard_a_corrupt_domain_record() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let row = database
            .create_pair(&PairValues {
                name: "\u{2003}".into(),
                left_root: b"/a".to_vec(),
                right_root: b"/b".to_vec(),
                mode: SYNC.into(),
                modification_times: SYNC_CATEGORY.into(),
                extended_attributes: SYNC_CATEGORY.into(),
                permissions: IGNORE_CATEGORY.into(),
                durability: STANDARD_DURABILITY.into(),
                schedule: MANUAL_SCHEDULE.into(),
                schedule_minutes: None,
                filters: String::new(),
                size_limit: None,
                age_limit: None,
                canary: None,
                schedule_time: None,
                schedule_days: None,
                records: RECORDS_IN_BOTH.into(),
                bandwidth_limit: None,
                review_policy: REVIEW_AUTOMATIC.into(),
                removal_protection: REMOVAL_ENABLED.into(),
            })
            .unwrap();
        let id = PairId::from_bytes(row.id.try_into().unwrap()).unwrap();
        drop(database);
        let mut store = PairStore::open(&location).unwrap();
        assert!(matches!(
            store.remove(id, 1),
            Err(Error::InvalidPair(InvalidPair::Name))
        ));
        assert!(matches!(
            store.pair(id),
            Err(Error::InvalidPair(InvalidPair::Name))
        ));
    }

    #[test]
    fn base_round_trips_and_is_bound_to_the_pair_revision() {
        use foldover_types::{Digest, EntryContent, EntryVersion};

        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let pair = store.create(&draft(SyncMode::Sync)).unwrap();
        let path = |bytes: &[u8]| SyncPath::try_from(bytes).unwrap();
        let version = |content, metadata| {
            EntryVersion::new(content, Digest::from_bytes([metadata; Digest::BYTE_LEN]))
        };
        let file = EntryContent::File(Digest::from_bytes([1; Digest::BYTE_LEN]));
        let folder = BaselineEntry::new(
            version(EntryContent::Directory, 2),
            version(EntryContent::Directory, 3),
        )
        .unwrap();
        let photo = BaselineEntry::new(version(file, 4), version(file, 5)).unwrap();
        store
            .update_base(
                pair.id(),
                pair.revision(),
                &[
                    BaseUpdate::Record(path(b"folder/photo"), photo),
                    BaseUpdate::Record(path(b"folder"), folder),
                ],
                JournalUpdate::None,
            )
            .unwrap();
        let base = store.base(pair.id()).unwrap();
        assert_eq!(
            base.into_iter().collect::<Vec<_>>(),
            [(path(b"folder"), folder), (path(b"folder/photo"), photo)]
        );

        store
            .update_base(
                pair.id(),
                pair.revision(),
                &[BaseUpdate::Forget(path(b"folder/photo"))],
                JournalUpdate::None,
            )
            .unwrap();
        assert_eq!(store.base(pair.id()).unwrap().len(), 1);

        let moved = PairDraft::new(
            "Photos".into(),
            LocalRoot::from_bytes(b"/left/\xff".to_vec()).unwrap(),
            LocalRoot::from_bytes(b"/moved".to_vec()).unwrap(),
            SyncMode::Sync,
        )
        .unwrap();
        let updated = store.update(pair.id(), pair.revision(), &moved).unwrap();
        assert!(matches!(
            store.update_base(
                pair.id(),
                pair.revision(),
                &[BaseUpdate::Record(path(b"folder"), folder)],
                JournalUpdate::None
            ),
            Err(Error::StaleRevision)
        ));
        assert!(store.base(updated.id()).unwrap().is_empty());
    }

    #[test]
    /// A move without its source is refused when written (the schema
    /// checks it), so every journal reads back.
    fn a_move_without_its_source_is_never_journaled() {
        use foldover_types::{Digest, EntryContent, EntryVersion};

        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let pair = store.create(&draft(SyncMode::Sync)).unwrap();
        let version = EntryVersion::new(
            EntryContent::File(Digest::from_bytes([5; Digest::BYTE_LEN])),
            Digest::from_bytes([5; Digest::BYTE_LEN]),
        );
        let unsourced = JournalEntry {
            seq: 0,
            path: SyncPath::try_from(&b"moved/here"[..]).unwrap(),
            side: Side::Right,
            action: JournalAction::Move(version),
            before: None,
            moved_from: None,
        };
        let recorded = store.update_base(
            pair.id(),
            pair.revision(),
            &[],
            JournalUpdate::Record {
                run: "2026-10-19T10-00-00.000Z",
                entries: &[unsourced],
            },
        );
        assert!(recorded.is_err());
        assert_eq!(store.journal(pair.id()).unwrap(), None);
    }

    #[test]
    fn journals_round_trip_with_the_base_and_root_changes_discard_them() {
        use foldover_types::{Digest, EntryContent, EntryVersion};

        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let pair = store.create(&draft(SyncMode::Sync)).unwrap();
        let path = |bytes: &[u8]| SyncPath::try_from(bytes).unwrap();
        let file = |content: u8, metadata: u8| {
            EntryVersion::new(
                EntryContent::File(Digest::from_bytes([content; Digest::BYTE_LEN])),
                Digest::from_bytes([metadata; Digest::BYTE_LEN]),
            )
        };
        let old = BaselineEntry::new(file(1, 1), file(1, 2)).unwrap();
        let new = BaselineEntry::new(file(3, 1), file(3, 2)).unwrap();
        let entries = [
            JournalEntry {
                seq: 0,
                path: path(b"new"),
                side: Side::Right,
                action: JournalAction::Create(file(4, 4)),
                before: None,
                moved_from: None,
            },
            JournalEntry {
                seq: 1,
                path: path(b"edited"),
                side: Side::Left,
                action: JournalAction::Replace(new.left()),
                before: Some(old),
                moved_from: None,
            },
            JournalEntry {
                seq: 2,
                path: path(b"gone"),
                side: Side::Right,
                action: JournalAction::Archive(old.right()),
                before: Some(old),
                moved_from: None,
            },
            JournalEntry {
                seq: 3,
                path: path(b"moved/here"),
                side: Side::Right,
                action: JournalAction::Move(file(5, 5)),
                before: Some(old),
                moved_from: Some(path(b"was/there")),
            },
        ];
        store
            .update_base(
                pair.id(),
                pair.revision(),
                &[BaseUpdate::Record(path(b"edited"), new)],
                JournalUpdate::Record {
                    run: "2026-10-19T10-00-00.000Z",
                    entries: &entries,
                },
            )
            .unwrap();
        assert_eq!(
            store.journal(pair.id()).unwrap(),
            Some(Journal {
                run: "2026-10-19T10-00-00.000Z".into(),
                entries: entries.to_vec(),
            })
        );
        assert_eq!(store.base(pair.id()).unwrap()[&path(b"edited")], new);

        // Undoing restores the base and spends the journal together.
        store
            .update_base(
                pair.id(),
                pair.revision(),
                &[BaseUpdate::Record(path(b"edited"), old)],
                JournalUpdate::Spend {
                    run: "2026-10-19T10-00-00.000Z",
                },
            )
            .unwrap();
        assert_eq!(store.journal(pair.id()).unwrap(), None);
        assert_eq!(store.base(pair.id()).unwrap()[&path(b"edited")], old);

        // A journal describes these roots; moving one discards it.
        store
            .update_base(
                pair.id(),
                pair.revision(),
                &[],
                JournalUpdate::Record {
                    run: "2026-10-19T11-00-00.000Z",
                    entries: &entries[..1],
                },
            )
            .unwrap();
        let moved = PairDraft::new(
            "Photos".into(),
            LocalRoot::from_bytes(b"/left/\xff".to_vec()).unwrap(),
            LocalRoot::from_bytes(b"/moved".to_vec()).unwrap(),
            SyncMode::Sync,
        )
        .unwrap();
        store.update(pair.id(), pair.revision(), &moved).unwrap();
        assert_eq!(store.journal(pair.id()).unwrap(), None);
    }

    /// PLAN.md §5.2: the per-batch journal cleanup must stay linear in a
    /// run's writes. Ignored by default; run explicitly in release mode
    /// on an idle host:
    ///   cargo test --release -p foldover-state journal_batches_commit_linearly -- --ignored --nocapture
    #[test]
    #[ignore = "performance budget; run explicitly in release mode on an idle host"]
    fn journal_batches_commit_linearly() {
        use std::time::{Duration, Instant};

        const BATCH: usize = 256;
        const BASE: usize = 100_000;
        // 1x, 2x, 4x and 8x the writes. The first measurement sets the
        // linear expectation; the old per-batch cleanup grew as 1x, 4x,
        // 16x and 64x, which this catches from 4x on.
        let mut first: Option<(usize, Duration)> = None;
        for entries in [BASE, 2 * BASE, 4 * BASE, 8 * BASE] {
            let (_directory, location) = location();
            let mut store = PairStore::open(&location).unwrap();
            let pair = store.create(&draft(SyncMode::Sync)).unwrap();
            let (updates, journal) = batch_writes(entries);
            let started = Instant::now();
            for (index, batch) in updates.chunks(BATCH).enumerate() {
                let start = index * BATCH;
                store
                    .update_base(
                        pair.id(),
                        pair.revision(),
                        batch,
                        JournalUpdate::Record {
                            run: "2026-10-20T10-00-00.000Z",
                            entries: &journal[start..start + batch.len()],
                        },
                    )
                    .unwrap();
            }
            let elapsed = started.elapsed();
            eprintln!("{entries} writes in {BATCH}-row batches took {elapsed:?}");
            if let Some((base, expected)) = first {
                // Allow 3x per write for B-tree depth, WAL growth and host
                // noise; the quadratic term would cost entries/base
                // squared.
                let allowed = expected * ((entries / base) as u32 * 3);
                assert!(
                    elapsed < allowed,
                    "{entries} writes took {elapsed:?}; over 3x the linear {base}-write expectation"
                );
            } else {
                first = Some((entries, elapsed));
            }
        }
    }

    /// `entries` writes and the journal entries a run records with them.
    fn batch_writes(entries: usize) -> (Vec<BaseUpdate>, Vec<JournalEntry>) {
        use foldover_types::{Digest, EntryContent, EntryVersion};

        let version = EntryVersion::new(
            EntryContent::File(Digest::from_bytes([7; Digest::BYTE_LEN])),
            Digest::from_bytes([9; Digest::BYTE_LEN]),
        );
        let entry = BaselineEntry::new(version, version).unwrap();
        let mut updates = Vec::with_capacity(entries);
        let mut journal = Vec::with_capacity(entries);
        for index in 0..entries {
            let path = SyncPath::try_from(
                format!("d-{:05}/f-{:05}", index / 1000, index % 1000).as_bytes(),
            )
            .unwrap();
            updates.push(BaseUpdate::Record(path.clone(), entry));
            journal.push(JournalEntry {
                seq: index as u64,
                path,
                side: Side::Left,
                action: JournalAction::Replace(version),
                before: Some(entry),
                moved_from: None,
            });
        }
        (updates, journal)
    }

    #[test]
    fn options_round_trip_and_metadata_changes_discard_the_base() {
        use foldover_types::{Digest, EntryContent, EntryVersion};

        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let options = PairOptions::new(
            MetadataPolicy::new(Propagation::Ignore, Propagation::Sync, Propagation::Sync),
            Durability::Full,
        )
        .with_schedule(Schedule::Every(ScheduleInterval::from_minutes(15).unwrap()))
        .with_filters(
            Filters::new(vec!["*.tmp".into(), "!keep.tmp".into(), String::new()])
                .and_then(|filters| filters.with_size_limit(Some(5_000_000)))
                .and_then(|filters| filters.with_age_limit(Some(90)))
                .unwrap(),
        )
        .with_canary(Some(
            SyncPath::try_from(b"docs/canary.txt".as_slice()).unwrap(),
        ))
        .unwrap()
        .with_records(FolderRecords::SourceOnly)
        .with_bandwidth_limit(Some(2_500_000))
        .unwrap();
        let pair = store
            .create(&draft(SyncMode::Sync).with_options(options.clone()))
            .unwrap();
        assert_eq!(
            store.pair(pair.id()).unwrap().unwrap().draft().options(),
            &options
        );
        assert_eq!(
            store
                .create(&draft(SyncMode::Sync))
                .unwrap()
                .draft()
                .options(),
            &PairOptions::default()
        );

        let version = EntryVersion::new(
            EntryContent::Directory,
            Digest::from_bytes([1; Digest::BYTE_LEN]),
        );
        let entry = BaselineEntry::new(version, version).unwrap();
        let path = SyncPath::try_from(b"folder".as_slice()).unwrap();
        let record = |store: &mut PairStore, revision| {
            store
                .update_base(
                    pair.id(),
                    revision,
                    &[BaseUpdate::Record(path.clone(), entry)],
                    JournalUpdate::None,
                )
                .unwrap();
        };
        record(&mut store, 1);

        // Durability does not affect stored versions; the base stays.
        let standard = PairOptions::new(options.metadata(), Durability::Standard);
        store
            .update(pair.id(), 1, &draft(SyncMode::Sync).with_options(standard))
            .unwrap();
        assert_eq!(store.base(pair.id()).unwrap().len(), 1);

        let defaults = draft(SyncMode::Sync);
        let updated = store.update(pair.id(), 2, &defaults).unwrap();
        assert_eq!(updated.draft().options(), &PairOptions::default());
        assert!(store.base(pair.id()).unwrap().is_empty());
    }

    #[test]
    fn a_daily_schedule_round_trips() {
        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let daily = Schedule::Daily(DailySchedule::new(
            TimeOfDay::from_minutes(18 * 60).unwrap(),
            Weekdays::MONDAY_TO_FRIDAY,
        ));
        let pair = store
            .create(
                &draft(SyncMode::Sync).with_options(PairOptions::default().with_schedule(daily)),
            )
            .unwrap();
        assert_eq!(
            store
                .pair(pair.id())
                .unwrap()
                .unwrap()
                .draft()
                .options()
                .schedule(),
            daily
        );
    }

    #[test]
    fn resolutions_round_trip_with_absent_sides() {
        use foldover_types::{Digest, EntryContent, EntryVersion};

        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let pair = store.create(&draft(SyncMode::Sync)).unwrap();
        let version = EntryVersion::new(
            EntryContent::File(Digest::from_bytes([3; Digest::BYTE_LEN])),
            Digest::from_bytes([4; Digest::BYTE_LEN]),
        );
        let path = SyncPath::try_from(b"notes.txt".as_slice()).unwrap();
        let resolution = Resolution::new(Side::Right, None, Some(version));
        store
            .record_resolution(pair.id(), pair.revision(), &path, resolution)
            .unwrap();
        assert_eq!(
            store
                .resolutions(pair.id())
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
            [(path, resolution)]
        );
    }

    #[test]
    fn malformed_snapshot_versions_are_cache_misses() {
        use foldover_types::{Digest, EntryContent, EntryVersion};

        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let pair = store.create(&draft(SyncMode::Sync)).unwrap();
        let version = EntryVersion::new(
            EntryContent::Directory,
            Digest::from_bytes([7; Digest::BYTE_LEN]),
        );
        let expected = SnapshotEntry {
            path: SyncPath::try_from(b"kept".as_slice()).unwrap(),
            fingerprint: b"opaque fingerprint".to_vec(),
            version,
        };
        let mut unknown_kind = encode(version).to_vec();
        unknown_kind[0] = u8::MAX;
        let row = |path: &[u8], version: Vec<u8>| SnapshotRow {
            path: path.to_vec(),
            fingerprint: expected.fingerprint.clone(),
            version,
        };
        store
            .database
            .replace_snapshot(
                pair.id().as_bytes(),
                database_revision(pair.revision()).unwrap(),
                side_name(Side::Left),
                &[
                    row(expected.path.as_bytes(), encode(version).to_vec()),
                    row(b"truncated", Vec::new()),
                    row(b"unknown-kind", unknown_kind),
                ],
            )
            .unwrap();

        assert_eq!(store.snapshot(pair.id(), Side::Left).unwrap(), [expected]);
    }

    #[test]
    fn invalid_snapshot_paths_and_authoritative_base_versions_still_fail() {
        use foldover_types::{Digest, EntryContent, EntryVersion};

        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let pair = store.create(&draft(SyncMode::Sync)).unwrap();
        let version = encode(EntryVersion::new(
            EntryContent::Directory,
            Digest::from_bytes([3; Digest::BYTE_LEN]),
        ));
        store
            .database
            .replace_snapshot(
                pair.id().as_bytes(),
                database_revision(pair.revision()).unwrap(),
                side_name(Side::Left),
                &[SnapshotRow {
                    path: b"../outside".to_vec(),
                    fingerprint: Vec::new(),
                    version: version.to_vec(),
                }],
            )
            .unwrap();
        assert!(matches!(
            store.snapshot(pair.id(), Side::Left),
            Err(Error::InvalidStored("snapshot path"))
        ));

        store
            .database
            .change_base(
                pair.id().as_bytes(),
                database_revision(pair.revision()).unwrap(),
                &[BaseChange::Put {
                    path: b"kept",
                    left_version: &[],
                    right_version: &version,
                }],
                foldover_db::JournalChange::None,
            )
            .unwrap();
        assert!(matches!(
            store.base(pair.id()),
            Err(Error::InvalidStored(_))
        ));
    }

    #[test]
    fn replace_snapshot_iter_matches_the_slice_api_across_pages() {
        use foldover_types::{Digest, EntryContent, EntryVersion};

        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let pair = store.create(&draft(SyncMode::Sync)).unwrap();
        let version = EntryVersion::new(
            EntryContent::File(Digest::from_bytes([3; Digest::BYTE_LEN])),
            Digest::from_bytes([4; Digest::BYTE_LEN]),
        );
        let entry = |path: &[u8], salt: u8| SnapshotEntry {
            path: SyncPath::try_from(path).unwrap(),
            fingerprint: vec![salt; 20],
            version,
        };
        let stored: Vec<SnapshotEntry> = (0..400)
            .map(|index| entry(format!("p-{index:04}").as_bytes(), 1))
            .collect();
        store
            .replace_snapshot(pair.id(), pair.revision(), Side::Left, &stored)
            .unwrap();

        // Remove page-boundary rows, change one row, and add paths before,
        // between, and after pages.
        let mut replacement: Vec<SnapshotEntry> = (0..400)
            .filter(|index| !matches!(*index, 0 | 255 | 256 | 399))
            .map(|index| {
                let salt = if index == 100 { 2 } else { 1 };
                entry(format!("p-{index:04}").as_bytes(), salt)
            })
            .collect();
        replacement.push(entry(b"p-0000a", 1));
        replacement.push(entry(b"p-0255a", 1));
        replacement.push(entry(b"p-0400", 1));
        replacement.sort_by(|a, b| a.path.cmp(&b.path));

        store
            .replace_snapshot_iter(pair.id(), pair.revision(), Side::Left, replacement.clone())
            .unwrap();
        assert_eq!(store.snapshot(pair.id(), Side::Left).unwrap(), replacement);

        // Empty iterator clears the side.
        store
            .replace_snapshot_iter(pair.id(), pair.revision(), Side::Left, std::iter::empty())
            .unwrap();
        assert!(store.snapshot(pair.id(), Side::Left).unwrap().is_empty());

        // All-added through the iterator.
        store
            .replace_snapshot_iter(pair.id(), pair.revision(), Side::Left, replacement.clone())
            .unwrap();
        assert_eq!(store.snapshot(pair.id(), Side::Left).unwrap(), replacement);
    }

    #[test]
    fn replace_snapshot_iter_rejects_unsorted_and_duplicate_input() {
        use foldover_types::{Digest, EntryContent, EntryVersion};

        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let pair = store.create(&draft(SyncMode::Sync)).unwrap();
        let version = EntryVersion::new(
            EntryContent::Directory,
            Digest::from_bytes([7; Digest::BYTE_LEN]),
        );
        let entry = |path: &[u8]| SnapshotEntry {
            path: SyncPath::try_from(path).unwrap(),
            fingerprint: b"fingerprint".to_vec(),
            version,
        };
        let stored: Vec<SnapshotEntry> = (0..300)
            .map(|index| entry(format!("p-{index:04}").as_bytes()))
            .collect();
        store
            .replace_snapshot(pair.id(), pair.revision(), Side::Left, &stored)
            .unwrap();

        // Out of order in the middle, after some page writes.
        let mut unsorted = stored.clone();
        unsorted.swap(10, 20);
        assert!(matches!(
            store.replace_snapshot_iter(pair.id(), pair.revision(), Side::Left, unsorted,),
            Err(Error::Storage(foldover_db::Error::UnsortedSnapshot))
        ));
        // A duplicate last row, after a real update write.
        let mut duplicate = stored.clone();
        duplicate.last_mut().unwrap().fingerprint = b"changed".to_vec();
        let last = duplicate.last().unwrap().clone();
        duplicate.push(last);
        assert!(matches!(
            store.replace_snapshot_iter(pair.id(), pair.revision(), Side::Left, duplicate,),
            Err(Error::Storage(foldover_db::Error::UnsortedSnapshot))
        ));
        // Both rejected replacements rolled back.
        assert_eq!(store.snapshot(pair.id(), Side::Left).unwrap(), stored);

        // A stale revision changes nothing.
        assert!(matches!(
            store.replace_snapshot_iter(
                pair.id(),
                pair.revision() + 1,
                Side::Left,
                std::iter::empty(),
            ),
            Err(Error::StaleRevision)
        ));
        assert_eq!(store.snapshot(pair.id(), Side::Left).unwrap(), stored);
    }

    #[test]
    fn captured_run_records_decode_like_the_pair_store() {
        use foldover_types::{Digest, EntryContent, EntryVersion};

        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let pair = store.create(&draft(SyncMode::Sync)).unwrap();
        let version = EntryVersion::new(
            EntryContent::File(Digest::from_bytes([3; Digest::BYTE_LEN])),
            Digest::from_bytes([4; Digest::BYTE_LEN]),
        );
        let base_path = SyncPath::try_from(b"file".as_slice()).unwrap();
        store
            .update_base(
                pair.id(),
                pair.revision(),
                &[BaseUpdate::Record(
                    base_path.clone(),
                    BaselineEntry::new(version, version).unwrap(),
                )],
                JournalUpdate::None,
            )
            .unwrap();
        let left = SnapshotEntry {
            path: SyncPath::try_from(b"left".as_slice()).unwrap(),
            fingerprint: b"left fingerprint".to_vec(),
            version,
        };
        let right = SnapshotEntry {
            path: SyncPath::try_from(b"right".as_slice()).unwrap(),
            fingerprint: b"right fingerprint".to_vec(),
            version,
        };
        store
            .replace_snapshot(
                pair.id(),
                pair.revision(),
                Side::Left,
                std::slice::from_ref(&left),
            )
            .unwrap();
        store
            .replace_snapshot(
                pair.id(),
                pair.revision(),
                Side::Right,
                std::slice::from_ref(&right),
            )
            .unwrap();

        let decoded = store.run_records(pair.id()).unwrap().decode().unwrap();
        assert_eq!(decoded.base, store.base(pair.id()).unwrap());
        assert_eq!(
            decoded.base[&base_path],
            BaselineEntry::new(version, version).unwrap()
        );
        assert_eq!(decoded.left_snapshot, [left]);
        assert_eq!(decoded.right_snapshot, [right]);
    }

    #[test]
    fn run_records_decode_keeps_snapshot_cache_rules_and_path_errors() {
        use foldover_types::{Digest, EntryContent, EntryVersion};

        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let pair = store.create(&draft(SyncMode::Sync)).unwrap();
        let version = EntryVersion::new(
            EntryContent::Directory,
            Digest::from_bytes([7; Digest::BYTE_LEN]),
        );
        let expected = SnapshotEntry {
            path: SyncPath::try_from(b"kept".as_slice()).unwrap(),
            fingerprint: b"opaque fingerprint".to_vec(),
            version,
        };
        let mut unknown_kind = encode(version).to_vec();
        unknown_kind[0] = u8::MAX;
        let row = |path: &[u8], version: Vec<u8>| SnapshotRow {
            path: path.to_vec(),
            fingerprint: expected.fingerprint.clone(),
            version,
        };
        store
            .database
            .replace_snapshot(
                pair.id().as_bytes(),
                database_revision(pair.revision()).unwrap(),
                side_name(Side::Left),
                &[
                    row(expected.path.as_bytes(), encode(version).to_vec()),
                    row(b"truncated", Vec::new()),
                    row(b"unknown-kind", unknown_kind),
                ],
            )
            .unwrap();
        let decoded = store.run_records(pair.id()).unwrap().decode().unwrap();
        assert_eq!(decoded.left_snapshot, std::slice::from_ref(&expected));
        assert!(decoded.right_snapshot.is_empty());

        store
            .database
            .replace_snapshot(
                pair.id().as_bytes(),
                database_revision(pair.revision()).unwrap(),
                side_name(Side::Right),
                &[SnapshotRow {
                    path: b"../outside".to_vec(),
                    fingerprint: Vec::new(),
                    version: encode(version).to_vec(),
                }],
            )
            .unwrap();
        assert!(matches!(
            store.run_records(pair.id()).unwrap().decode(),
            Err(Error::InvalidStored("snapshot path"))
        ));
        assert!(store.snapshot(pair.id(), Side::Right).is_err());

        let base_version = encode(version);
        store
            .database
            .change_base(
                pair.id().as_bytes(),
                database_revision(pair.revision()).unwrap(),
                &[BaseChange::Put {
                    path: b"kept",
                    left_version: &[],
                    right_version: &base_version,
                }],
                foldover_db::JournalChange::None,
            )
            .unwrap();
        assert!(matches!(
            store.run_records(pair.id()).unwrap().decode(),
            Err(Error::InvalidStored(_))
        ));
        assert!(store.base(pair.id()).is_err());
    }

    #[test]
    fn captured_run_records_stay_behind_a_config_change() {
        use foldover_types::{Digest, EntryContent, EntryVersion};

        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let pair = store.create(&draft(SyncMode::Sync)).unwrap();
        let version = EntryVersion::new(
            EntryContent::Directory,
            Digest::from_bytes([9; Digest::BYTE_LEN]),
        );
        let base_path = SyncPath::try_from(b"file".as_slice()).unwrap();
        store
            .update_base(
                pair.id(),
                pair.revision(),
                &[BaseUpdate::Record(
                    base_path.clone(),
                    BaselineEntry::new(version, version).unwrap(),
                )],
                JournalUpdate::None,
            )
            .unwrap();
        let entry = SnapshotEntry {
            path: SyncPath::try_from(b"file".as_slice()).unwrap(),
            fingerprint: b"fingerprint".to_vec(),
            version,
        };
        store
            .replace_snapshot(
                pair.id(),
                pair.revision(),
                Side::Left,
                std::slice::from_ref(&entry),
            )
            .unwrap();
        let captured = store.run_records(pair.id()).unwrap();

        // The pair's roots move and the next revision forgets the base.
        let moved = PairDraft::new(
            "Photos".into(),
            LocalRoot::from_bytes(b"/elsewhere/\xff".to_vec()).unwrap(),
            LocalRoot::from_bytes(b"/right".to_vec()).unwrap(),
            SyncMode::Sync,
        )
        .unwrap();
        let updated = store.update(pair.id(), pair.revision(), &moved).unwrap();
        assert_eq!(updated.revision(), pair.revision() + 1);
        assert!(store.base(pair.id()).unwrap().is_empty());

        // Decoding the captured rows still returns the run's own inputs.
        let decoded = captured.decode().unwrap();
        assert_eq!(
            decoded.base[&base_path],
            BaselineEntry::new(version, version).unwrap()
        );
        assert_eq!(decoded.left_snapshot, [entry]);
    }

    #[test]
    fn captured_run_records_stay_original_after_pair_removal_and_keep_the_writer_lease() {
        use foldover_types::{Digest, EntryContent, EntryVersion};

        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let pair = store.create(&draft(SyncMode::Sync)).unwrap();
        let version = EntryVersion::new(
            EntryContent::File(Digest::from_bytes([8; Digest::BYTE_LEN])),
            Digest::from_bytes([9; Digest::BYTE_LEN]),
        );
        let base_path = SyncPath::try_from(b"file".as_slice()).unwrap();
        store
            .update_base(
                pair.id(),
                pair.revision(),
                &[BaseUpdate::Record(
                    base_path.clone(),
                    BaselineEntry::new(version, version).unwrap(),
                )],
                JournalUpdate::None,
            )
            .unwrap();
        let entry = SnapshotEntry {
            path: SyncPath::try_from(b"left".as_slice()).unwrap(),
            fingerprint: b"left fingerprint".to_vec(),
            version,
        };
        store
            .replace_snapshot(
                pair.id(),
                pair.revision(),
                Side::Left,
                std::slice::from_ref(&entry),
            )
            .unwrap();
        let captured = store.run_records(pair.id()).unwrap();

        // The primary forgets the base, moves the roots, then removes the
        // pair outright. The pinned view keeps the revision it captured.
        store
            .update_base(
                pair.id(),
                pair.revision(),
                &[BaseUpdate::Forget(base_path.clone())],
                JournalUpdate::None,
            )
            .unwrap();
        let moved = PairDraft::new(
            "Moved".into(),
            LocalRoot::from_bytes(b"/moved/\xff".to_vec()).unwrap(),
            LocalRoot::from_bytes(b"/right".to_vec()).unwrap(),
            SyncMode::Sync,
        )
        .unwrap();
        let updated = store.update(pair.id(), pair.revision(), &moved).unwrap();
        store.remove(pair.id(), updated.revision()).unwrap();

        // A pinned view keeps the single writer lease until it drops.
        drop(store);
        assert!(matches!(
            PairStore::open(&location),
            Err(Error::Storage(foldover_db::Error::WriterLocked(_)))
        ));

        // Decoding the captured rows still returns the run's own inputs.
        let decoded = captured.decode().unwrap();
        assert_eq!(
            decoded.base[&base_path],
            BaselineEntry::new(version, version).unwrap()
        );
        assert_eq!(decoded.left_snapshot, [entry]);

        // Dropping the view in `decode` released the lease.
        PairStore::open(&location).unwrap();
    }

    #[test]
    fn decoded_snapshot_paths_share_base_backing() {
        use foldover_types::{Digest, EntryContent, EntryVersion};

        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let pair = store.create(&draft(SyncMode::Sync)).unwrap();
        let version = EntryVersion::new(
            EntryContent::Directory,
            Digest::from_bytes([11; Digest::BYTE_LEN]),
        );
        let base_path = SyncPath::try_from(b"shared/\xff".as_slice()).unwrap();
        store
            .update_base(
                pair.id(),
                pair.revision(),
                &[BaseUpdate::Record(
                    base_path.clone(),
                    BaselineEntry::new(version, version).unwrap(),
                )],
                JournalUpdate::None,
            )
            .unwrap();
        let entry = SnapshotEntry {
            path: base_path,
            fingerprint: b"fingerprint".to_vec(),
            version,
        };
        store
            .replace_snapshot(
                pair.id(),
                pair.revision(),
                Side::Left,
                std::slice::from_ref(&entry),
            )
            .unwrap();

        let decoded = store.run_records(pair.id()).unwrap().decode().unwrap();
        let base_key = decoded.base.keys().next().unwrap();
        assert_eq!(base_key.as_bytes(), b"shared/\xff");
        assert_eq!(
            decoded.left_snapshot[0].path.as_bytes().as_ptr(),
            base_key.as_bytes().as_ptr(),
            "an equal raw snapshot path reuses the base Arc backing"
        );
    }

    #[test]
    fn run_records_visitors_keep_cache_rules() {
        use foldover_types::{Digest, EntryContent, EntryVersion};

        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let pair = store.create(&draft(SyncMode::Sync)).unwrap();
        let version = EntryVersion::new(
            EntryContent::Directory,
            Digest::from_bytes([7; Digest::BYTE_LEN]),
        );
        let expected = SnapshotEntry {
            path: SyncPath::try_from(b"kept".as_slice()).unwrap(),
            fingerprint: b"opaque fingerprint".to_vec(),
            version,
        };
        let mut unknown_kind = encode(version).to_vec();
        unknown_kind[0] = u8::MAX;
        let row = |path: &[u8], version: Vec<u8>| SnapshotRow {
            path: path.to_vec(),
            fingerprint: expected.fingerprint.clone(),
            version,
        };
        store
            .database
            .replace_snapshot(
                pair.id().as_bytes(),
                database_revision(pair.revision()).unwrap(),
                side_name(Side::Left),
                &[
                    row(expected.path.as_bytes(), encode(version).to_vec()),
                    row(b"truncated", Vec::new()),
                    row(b"unknown-kind", unknown_kind),
                ],
            )
            .unwrap();
        let records = store.run_records(pair.id()).unwrap();
        let mut left = Vec::new();
        records
            .visit_snapshot(Side::Left, |entry| left.push(entry))
            .unwrap();
        assert_eq!(left, [expected]);
        let mut base = Vec::new();
        records
            .visit_base(|path, entry| base.push((path, entry)))
            .unwrap();
        assert!(base.is_empty());

        // An invalid snapshot path is an error, not a cache miss.
        store
            .database
            .replace_snapshot(
                pair.id().as_bytes(),
                database_revision(pair.revision()).unwrap(),
                side_name(Side::Right),
                &[SnapshotRow {
                    path: b"../outside".to_vec(),
                    fingerprint: Vec::new(),
                    version: encode(version).to_vec(),
                }],
            )
            .unwrap();
        let records = store.run_records(pair.id()).unwrap();
        assert!(matches!(
            records.visit_snapshot(Side::Right, |_| ()),
            Err(Error::InvalidStored("snapshot path"))
        ));

        // A bad base row stays an error rather than being skipped.
        let base_version = encode(version);
        store
            .database
            .change_base(
                pair.id().as_bytes(),
                database_revision(pair.revision()).unwrap(),
                &[BaseChange::Put {
                    path: b"kept",
                    left_version: &[],
                    right_version: &base_version,
                }],
                foldover_db::JournalChange::None,
            )
            .unwrap();
        let records = store.run_records(pair.id()).unwrap();
        assert!(matches!(
            records.visit_base(|_, _| ()),
            Err(Error::InvalidStored(_))
        ));
    }

    #[test]
    fn safety_settings_round_trip_all_combinations_through_db_and_state() {
        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        for review in [ReviewPolicy::Automatic, ReviewPolicy::Required] {
            for removal in [RemovalProtection::Enabled, RemovalProtection::Disabled] {
                let d = draft(SyncMode::Sync).with_options(
                    PairOptions::default()
                        .with_review_policy(review)
                        .with_removal_protection(removal),
                );
                let saved = store.create(&d).unwrap();
                assert_eq!(saved.draft().options().review_policy(), review);
                assert_eq!(saved.draft().options().removal_protection(), removal);
            }
        }
        // Reopen and verify all four pairs.
        drop(store);
        let store = PairStore::open(&location).unwrap();
        let pairs = store.pairs().unwrap();
        assert_eq!(pairs.len(), 4);
        let combos: Vec<_> = pairs
            .iter()
            .map(|p| {
                (
                    p.draft().options().review_policy(),
                    p.draft().options().removal_protection(),
                )
            })
            .collect();
        assert!(combos.contains(&(ReviewPolicy::Automatic, RemovalProtection::Enabled)));
        assert!(combos.contains(&(ReviewPolicy::Automatic, RemovalProtection::Disabled)));
        assert!(combos.contains(&(ReviewPolicy::Required, RemovalProtection::Enabled)));
        assert!(combos.contains(&(ReviewPolicy::Required, RemovalProtection::Disabled)));
    }

    #[test]
    fn new_pairs_preserve_default_policies_after_reopening() {
        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let d = draft(SyncMode::Sync).with_options(PairOptions::default());
        let saved = store.create(&d).unwrap();
        assert_eq!(
            saved.draft().options().review_policy(),
            ReviewPolicy::Automatic
        );
        assert_eq!(
            saved.draft().options().removal_protection(),
            RemovalProtection::Enabled
        );
        drop(store);
        // Reopen and verify defaults persist.
        let store = PairStore::open(&location).unwrap();
        let pairs = store.pairs().unwrap();
        assert_eq!(pairs.len(), 1);
        let p = &pairs[0];
        assert_eq!(p.draft().options().review_policy(), ReviewPolicy::Automatic);
        assert_eq!(
            p.draft().options().removal_protection(),
            RemovalProtection::Enabled
        );
    }
}
