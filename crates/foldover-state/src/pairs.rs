use foldover_db::{Database, PairRow, PairValues, StateLocation};
use foldover_types::{InvalidPair, LocalRoot, PairConfig, PairDraft, PairId, Side, SyncMode};
use std::os::unix::ffi::OsStrExt;

const SYNC: &str = "sync";
const MIRROR_LEFT: &str = "mirror-left";
const MIRROR_RIGHT: &str = "mirror-right";
const BACKUP_LEFT: &str = "backup-left";
const BACKUP_RIGHT: &str = "backup-right";

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
    PairValues {
        name: draft.name().to_owned(),
        left_root: draft.left().as_bytes().to_vec(),
        right_root: draft.right().as_bytes().to_vec(),
        mode: mode.to_owned(),
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
    let draft = PairDraft::new(
        row.values.name,
        LocalRoot::from_bytes(row.values.left_root)?,
        LocalRoot::from_bytes(row.values.right_root)?,
        mode,
    )?;
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
}
