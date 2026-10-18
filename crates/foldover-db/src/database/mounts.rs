//! Where each side's network share is mounted from, so an unmounted share
//! can be mounted again. A side's source is forgotten when its root changes.

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{Database, Error, revision_error};

/// Mount source URLs, e.g. `smb://alice@nas.local/Photos`; `None` for a
/// local volume or one not observed yet.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MountRow {
    pub left: Option<String>,
    pub right: Option<String>,
}

impl Database {
    pub fn mounts(&self, pair: &[u8]) -> Result<MountRow, Error> {
        self.connection
            .query_row(
                "SELECT left_mount,right_mount FROM pair WHERE id=?1",
                [pair],
                |row| {
                    Ok(MountRow {
                        left: row.get(0)?,
                        right: row.get(1)?,
                    })
                },
            )
            .optional()?
            .ok_or(Error::PairNotFound)
    }

    /// Record observed sources, only while the pair keeps `revision`.
    pub fn record_mounts(
        &mut self,
        pair: &[u8],
        revision: i64,
        mounts: &MountRow,
    ) -> Result<(), Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if transaction.execute(
            "UPDATE pair SET left_mount=?3,right_mount=?4 WHERE id=?1 AND revision=?2",
            params![pair, revision, mounts.left, mounts.right],
        )? == 0
        {
            return Err(revision_error(&transaction, pair)?);
        }
        transaction.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{location, values};
    use super::*;

    #[test]
    fn sources_are_revision_bound_and_forgotten_with_their_root() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let mounts = MountRow {
            left: Some("smb://nas/a".into()),
            right: Some("smb://nas/b".into()),
        };
        assert!(matches!(
            database.record_mounts(&pair.id, pair.revision + 1, &mounts),
            Err(Error::StaleRevision)
        ));
        database
            .record_mounts(&pair.id, pair.revision, &mounts)
            .unwrap();
        assert_eq!(database.mounts(&pair.id).unwrap(), mounts);

        let mut moved = values();
        moved.right_root = b"/elsewhere".to_vec();
        database
            .update_pair(&pair.id, pair.revision, &moved)
            .unwrap();
        assert_eq!(
            database.mounts(&pair.id).unwrap(),
            MountRow {
                left: mounts.left,
                right: None,
            }
        );
    }
}
