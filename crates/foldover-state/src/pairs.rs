use std::collections::BTreeMap;
use std::os::unix::ffi::OsStrExt;

use foldover_db::{
    BaseChange, Database, PairRow, PairValues, ResolutionRow, SnapshotRow, StateLocation,
};
use foldover_types::{
    BaselineEntry, DailySchedule, Durability, EndpointId, Filters, InvalidPair, LocalRoot,
    MetadataPolicy, PairConfig, PairDraft, PairId, PairOptions, PairRoot, Propagation, Resolution,
    Schedule, ScheduleInterval, Side, SyncMode, SyncPath, TimeOfDay, Weekdays,
};

use crate::BaseUpdate;
use crate::base::{decode, decode_entry, encode};

mod device;
mod history;
mod journal;
mod mounts;
mod pause;
mod settings;
mod share;
pub use device::Device;
pub use history::{HISTORY_LIMIT, HistoryEntry, HistoryOutcome, RunTrigger};
pub use journal::{Journal, JournalAction, JournalEntry, JournalUpdate};
pub use mounts::MountSources;
pub use pause::Pause;
pub use settings::LowPowerSyncs;
pub use share::Share;

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

    pub fn remove(&mut self, id: PairId, revision: u64) -> Result<PairConfig, Error> {
        let revision = database_revision(revision)?;
        // Decode before deleting: corrupt domain records must not disappear silently.
        let current = self.pair(id)?.ok_or(Error::PairNotFound)?;
        self.database.remove_pair(id.as_bytes(), revision)?;
        Ok(current)
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
        self.database
            .snapshot(id.as_bytes(), side_name(side))?
            .into_iter()
            .filter_map(|row| {
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
            })
            .collect()
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
            .map(|entry| SnapshotRow {
                path: entry.path.as_bytes().to_vec(),
                fingerprint: entry.fingerprint.clone(),
                version: encode(entry.version).to_vec(),
            })
            .collect();
        Ok(self
            .database
            .replace_snapshot(id.as_bytes(), revision, side_name(side), &rows)?)
    }
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
            .map(|row| {
                let path =
                    SyncPath::try_from(row.path).map_err(|_| Error::InvalidStored("base path"))?;
                Ok((path, decode_entry(&row.left_version, &row.right_version)?))
            })
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
    let canary = row
        .values
        .canary
        .map(|bytes| SyncPath::try_from(bytes).map_err(|_| Error::InvalidStored("canary")))
        .transpose()?;
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
            .with_canary(canary)?,
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
            },
            JournalEntry {
                seq: 1,
                path: path(b"edited"),
                side: Side::Left,
                action: JournalAction::Replace(new.left()),
                before: Some(old),
            },
            JournalEntry {
                seq: 2,
                path: path(b"gone"),
                side: Side::Right,
                action: JournalAction::Archive(old.right()),
                before: Some(old),
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
}
