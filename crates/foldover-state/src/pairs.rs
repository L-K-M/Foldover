use std::collections::BTreeMap;
use std::os::unix::ffi::OsStrExt;

use foldover_db::{BaseChange, Database, PairRow, PairValues, StateLocation};
use foldover_types::{
    BaselineEntry, Durability, EndpointId, InvalidPair, LocalRoot, MetadataPolicy, PairConfig,
    PairDraft, PairId, PairOptions, Propagation, Side, SyncMode, SyncPath,
};

use crate::BaseUpdate;
use crate::base::{decode_entry, encode};

const SYNC: &str = "sync";
const MIRROR_LEFT: &str = "mirror-left";
const MIRROR_RIGHT: &str = "mirror-right";
const BACKUP_LEFT: &str = "backup-left";
const BACKUP_RIGHT: &str = "backup-right";
const SYNC_CATEGORY: &str = "sync";
const IGNORE_CATEGORY: &str = "ignore";
const STANDARD_DURABILITY: &str = "standard";
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

    /// Apply updates atomically, only while the pair keeps `revision`, so a
    /// run planned for other roots or another mode cannot commit its base.
    pub fn update_base(
        &mut self,
        id: PairId,
        revision: u64,
        updates: &[BaseUpdate],
    ) -> Result<(), Error> {
        let revision = database_revision(revision)?;
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
            .change_base(id.as_bytes(), revision, &changes)?;
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
    let durability = match row.values.durability.as_str() {
        STANDARD_DURABILITY => Durability::Standard,
        FULL_DURABILITY => Durability::Full,
        _ => return Err(Error::InvalidStored("durability")),
    };
    let draft = PairDraft::new(
        row.values.name,
        LocalRoot::from_bytes(row.values.left_root)?,
        LocalRoot::from_bytes(row.values.right_root)?,
        mode,
    )?
    .with_options(PairOptions::new(metadata, durability));
    let revision = u64::try_from(row.revision).map_err(|_| Error::InvalidStored("revision"))?;
    Ok(PairConfig::from_stored(id, revision, draft)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn location() -> (tempfile::TempDir, StateLocation) {
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
                &[BaseUpdate::Record(path(b"folder"), folder)]
            ),
            Err(Error::StaleRevision)
        ));
        assert!(store.base(updated.id()).unwrap().is_empty());
    }

    #[test]
    fn options_round_trip_and_metadata_changes_discard_the_base() {
        use foldover_types::{Digest, EntryContent, EntryVersion};

        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let options = PairOptions::new(
            MetadataPolicy::new(Propagation::Ignore, Propagation::Sync, Propagation::Sync),
            Durability::Full,
        );
        let pair = store
            .create(&draft(SyncMode::Sync).with_options(options))
            .unwrap();
        assert_eq!(
            store.pair(pair.id()).unwrap().unwrap().draft().options(),
            options
        );
        assert_eq!(
            store
                .create(&draft(SyncMode::Sync))
                .unwrap()
                .draft()
                .options(),
            PairOptions::default()
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
        assert_eq!(updated.draft().options(), PairOptions::default());
        assert!(store.base(pair.id()).unwrap().is_empty());
    }
}
