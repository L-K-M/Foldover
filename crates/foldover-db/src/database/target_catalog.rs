//! A pair's target-owned catalog marker. Once the target acknowledges a
//! migration, the pair's state is read from the target's immutable catalog;
//! the local base, snapshot and journal are only caches of that epoch and
//! generation, and legacy local writers are refused.

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{Database, Error, Purge};

/// The target catalog epoch and generation a pair was migrated to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetCatalogRow {
    pub epoch: [u8; 16],
    pub generation: u64,
    pub digest: [u8; 32],
}

/// The digest of a generation the target has not acknowledged.
const UNACKNOWLEDGED: [u8; 32] = [0; 32];

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TargetCatalogRow> {
    let generation: i64 = row.get(1)?;
    Ok(TargetCatalogRow {
        epoch: fixed(row, 0, "epoch")?,
        generation: u64::try_from(generation)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, generation))?,
        digest: fixed(row, 2, "digest")?,
    })
}

/// Read one fixed-width blob column, refusing a stored row whose length
/// disagrees with the schema's marker shape.
fn fixed<const N: usize>(
    row: &rusqlite::Row<'_>,
    index: usize,
    name: &str,
) -> rusqlite::Result<[u8; N]> {
    let bytes: Vec<u8> = row.get(index)?;
    bytes.as_slice().try_into().map_err(|_| {
        rusqlite::Error::InvalidColumnType(index, name.to_string(), rusqlite::types::Type::Blob)
    })
}

/// Refuse a legacy local write while the target catalog acknowledges the
/// migration. A generation-0 marker is still unacknowledged, so the job
/// that began it may keep writing its old local state.
pub(super) fn require_no_acknowledged_target(
    connection: &rusqlite::Connection,
    id: &[u8],
) -> Result<(), Error> {
    // A schema before V31 cannot hold a marker. Skipping the probe keeps a
    // legacy database readable and writable until it is upgraded.
    let schema: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if schema < super::SCHEMA_VERSION {
        return Ok(());
    }
    let acknowledged: Option<bool> = connection
        .query_row(
            "SELECT generation>0 FROM target_catalog WHERE pair_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    match acknowledged {
        Some(true) => Err(Error::InvalidState(
            "Pair is owned by an acknowledged target catalog",
        )),
        _ => Ok(()),
    }
}

impl Database {
    /// The pair's target catalog marker, if a migration has begun.
    pub fn target_catalog(&self, id: &[u8]) -> Result<Option<TargetCatalogRow>, Error> {
        Ok(self
            .connection
            .query_row(
                "SELECT epoch,generation,digest FROM target_catalog WHERE pair_id=?1",
                [id],
                row,
            )
            .optional()?)
    }

    /// Begin routing the pair's state to the target catalog. The marker is
    /// inserted at generation 0 with an all-zero digest only when absent;
    /// an existing marker is returned unchanged. The pair's active revision
    /// must still be `revision`, so a saved edit for a later run does not
    /// stop the running job from beginning.
    pub fn begin_target_catalog(
        &mut self,
        id: &[u8],
        revision: i64,
        epoch: [u8; 16],
    ) -> Result<TargetCatalogRow, Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active = super::pending::active_revision(&transaction, id)?;
        if active != revision {
            return Err(Error::StaleRevision);
        }
        transaction.execute(
            "INSERT OR IGNORE INTO target_catalog (pair_id,epoch,generation,digest) \
             VALUES (?1,?2,0,zeroblob(32))",
            params![id, epoch.as_slice()],
        )?;
        let marker = transaction.query_row(
            "SELECT epoch,generation,digest FROM target_catalog WHERE pair_id=?1",
            [id],
            row,
        )?;
        transaction.commit()?;
        Ok(marker)
    }

    /// Acknowledge one generation of the target catalog. `expected` is the
    /// marker the job read; `next` must keep its epoch and raise its
    /// generation by exactly one, with a non-zero digest. The pair's
    /// active revision must still be `revision`, and the stored marker must
    /// still equal `expected`.
    pub fn acknowledge_target_catalog(
        &mut self,
        id: &[u8],
        revision: i64,
        expected: &TargetCatalogRow,
        next: &TargetCatalogRow,
    ) -> Result<(), Error> {
        self.acknowledge_target_catalog_resolving(id, revision, expected, next, &[])
    }

    /// Spending conflict choices and advancing the small local checkpoint are
    /// one transaction. Recovery replays the same paths from the manifest.
    pub fn acknowledge_target_catalog_resolving(
        &mut self,
        id: &[u8],
        revision: i64,
        expected: &TargetCatalogRow,
        next: &TargetCatalogRow,
        spent: &[&[u8]],
    ) -> Result<(), Error> {
        if next.epoch != expected.epoch {
            return Err(Error::InvalidState("Target catalog epoch must not change"));
        }
        let generation = expected
            .generation
            .checked_add(1)
            .ok_or(Error::InvalidState("Target catalog generation overflow"))?;
        if expected.generation > i64::MAX as u64
            || next.generation != generation
            || next.generation > i64::MAX as u64
        {
            return Err(Error::InvalidState(
                "Target catalog generation must advance by exactly one",
            ));
        }
        if next.digest == UNACKNOWLEDGED {
            return Err(Error::InvalidState(
                "Target catalog digest must not be all zero",
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active = super::pending::active_revision(&transaction, id)?;
        if active != revision {
            return Err(Error::StaleRevision);
        }
        let updated = transaction.execute(
            "UPDATE target_catalog SET epoch=?2,generation=?3,digest=?4 \
             WHERE pair_id=?1 AND epoch=?5 AND generation=?6 AND digest=?7",
            params![
                id,
                next.epoch.as_slice(),
                next.generation as i64,
                next.digest.as_slice(),
                expected.epoch.as_slice(),
                expected.generation as i64,
                expected.digest.as_slice()
            ],
        )?;
        if updated == 0 {
            let marker: Option<()> = transaction
                .query_row(
                    "SELECT 1 FROM target_catalog WHERE pair_id=?1",
                    [id],
                    |_| Ok(()),
                )
                .optional()?;
            return Err(if marker.is_some() {
                Error::InvalidState("Target catalog marker changed")
            } else {
                Error::InvalidState("Target catalog marker is absent")
            });
        }
        {
            let mut delete =
                transaction.prepare("DELETE FROM resolution WHERE pair_id=?1 AND path=?2")?;
            for path in spent {
                delete.execute(params![id, path])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Delete up to `limit` local cache rows of a pair whose target catalog
    /// migration is acknowledged. The marker stays, so reads keep routing
    /// to the target; only the local base, snapshot and journal are removed.
    pub fn purge_target_catalog_rows(&mut self, id: &[u8], limit: usize) -> Result<Purge, Error> {
        let limit = i64::try_from(limit)
            .ok()
            .filter(|limit| *limit > 0)
            .ok_or(Error::InvalidState("Purge limit must be positive"))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let generation: Option<i64> = transaction
            .query_row(
                "SELECT generation FROM target_catalog WHERE pair_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        match generation {
            Some(generation) if generation > 0 => {}
            Some(_) => {
                return Err(Error::InvalidState(
                    "Target catalog migration is not acknowledged",
                ));
            }
            None => return Err(Error::InvalidState("Target catalog marker is absent")),
        }
        let mut left = limit;
        for (table, key) in [
            ("base_entry", "path"),
            ("snapshot_entry", "side,path"),
            ("run_journal", "run,seq"),
        ] {
            if left == 0 {
                break;
            }
            let deleted = transaction.execute(
                &format!(
                    "DELETE FROM {table} WHERE pair_id=?1 AND ({key}) IN \
                     (SELECT {key} FROM {table} WHERE pair_id=?1 LIMIT ?2)"
                ),
                params![id, left],
            )?;
            left -= i64::try_from(deleted).unwrap_or(left);
        }
        transaction.commit()?;
        Ok(if left > 0 {
            Purge::Done
        } else {
            Purge::Pending
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{location, put, remove_and_purge, snapshot_row, values};
    use super::super::{JournalChange, JournalRow, ResolutionRow, SnapshotRow};
    use super::*;

    fn journal_row(seq: i64, path: &[u8]) -> JournalRow {
        JournalRow {
            seq,
            path: path.to_vec(),
            side: "left".into(),
            action: "create".into(),
            version: b"v".to_vec(),
            base: None,
            source: None,
        }
    }

    #[test]
    fn begin_and_cas_follow_the_active_revision() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let epoch = [3_u8; 16];
        assert!(matches!(
            database.begin_target_catalog(&pair.id, 2, epoch),
            Err(Error::StaleRevision)
        ));
        assert!(matches!(
            database.begin_target_catalog(&[9; 16], 1, epoch),
            Err(Error::PairNotFound)
        ));
        let marker = database.begin_target_catalog(&pair.id, 1, epoch).unwrap();
        assert_eq!(
            marker,
            TargetCatalogRow {
                epoch,
                generation: 0,
                digest: [0; 32]
            }
        );
        // Beginning again returns the stored marker, even under a different
        // epoch, and a deferred edit does not disturb the active revision.
        let other = [4_u8; 16];
        assert_eq!(
            database.begin_target_catalog(&pair.id, 1, other).unwrap(),
            marker
        );
        let mut renamed = values();
        renamed.name = "Next".into();
        database.defer_pair_update(&pair.id, 1, &renamed).unwrap();
        assert_eq!(
            database.begin_target_catalog(&pair.id, 1, other).unwrap(),
            marker
        );
        assert!(matches!(
            database.begin_target_catalog(&pair.id, 2, epoch),
            Err(Error::StaleRevision)
        ));

        let next = TargetCatalogRow {
            epoch,
            generation: 1,
            digest: [7_u8; 32],
        };
        assert!(matches!(
            database.acknowledge_target_catalog(&pair.id, 2, &marker, &next),
            Err(Error::StaleRevision)
        ));
        database
            .acknowledge_target_catalog(&pair.id, 1, &marker, &next)
            .unwrap();
        assert_eq!(database.target_catalog(&pair.id).unwrap(), Some(next));

        // A replay with the old expected marker fails and changes nothing.
        assert!(matches!(
            database.acknowledge_target_catalog(&pair.id, 1, &marker, &next),
            Err(Error::InvalidState(_))
        ));
        assert_eq!(database.target_catalog(&pair.id).unwrap(), Some(next));

        // A wrong expected digest, or a next epoch, generation skip, or
        // zero digest, fails before the CAS and leaves the marker alone.
        let wrong = TargetCatalogRow {
            epoch,
            generation: 0,
            digest: [0; 32],
        };
        let second = TargetCatalogRow {
            epoch,
            generation: 2,
            digest: [8_u8; 32],
        };
        assert!(matches!(
            database.acknowledge_target_catalog(&pair.id, 1, &wrong, &second),
            Err(Error::InvalidState(_))
        ));
        let changed_epoch = TargetCatalogRow {
            epoch: other,
            generation: 2,
            digest: [8_u8; 32],
        };
        assert!(matches!(
            database.acknowledge_target_catalog(&pair.id, 1, &next, &changed_epoch),
            Err(Error::InvalidState(_))
        ));
        let skipped = TargetCatalogRow {
            epoch,
            generation: 3,
            digest: [8_u8; 32],
        };
        assert!(matches!(
            database.acknowledge_target_catalog(&pair.id, 1, &next, &skipped),
            Err(Error::InvalidState(_))
        ));
        let zero = TargetCatalogRow {
            epoch,
            generation: 2,
            digest: [0; 32],
        };
        assert!(matches!(
            database.acknowledge_target_catalog(&pair.id, 1, &next, &zero),
            Err(Error::InvalidState(_))
        ));
        assert_eq!(database.target_catalog(&pair.id).unwrap(), Some(next));

        // A successful second generation advances from the current marker.
        database
            .acknowledge_target_catalog(&pair.id, 1, &next, &second)
            .unwrap();
        assert_eq!(database.target_catalog(&pair.id).unwrap(), Some(second));

        // An existing pair without a marker, or a missing pair, is refused.
        let unmarked = database.create_pair(&values()).unwrap();
        assert!(matches!(
            database.acknowledge_target_catalog(&unmarked.id, 1, &next, &second),
            Err(Error::InvalidState(_))
        ));
        let probe = TargetCatalogRow {
            epoch,
            generation: 3,
            digest: [9_u8; 32],
        };
        assert!(matches!(
            database.acknowledge_target_catalog(&[9; 16], 1, &second, &probe),
            Err(Error::PairNotFound)
        ));
    }

    #[test]
    fn the_marker_follows_its_pair() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let epoch = [1_u8; 16];
        let marker = database.begin_target_catalog(&pair.id, 1, epoch).unwrap();
        let next = TargetCatalogRow {
            epoch,
            generation: 1,
            digest: [2_u8; 32],
        };
        database
            .acknowledge_target_catalog(&pair.id, 1, &marker, &next)
            .unwrap();
        assert_eq!(database.target_catalog(&pair.id).unwrap(), Some(next));
        remove_and_purge(&mut database, &pair.id, 1);
        assert_eq!(database.target_catalog(&pair.id).unwrap(), None);
        let violations: i64 = database
            .connection
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);
    }

    #[test]
    fn acknowledgement_spends_resolutions_with_the_marker() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let resolution = ResolutionRow {
            path: b"a".to_vec(),
            keep: "left".into(),
            left_version: Some(b"l".to_vec()),
            right_version: None,
        };
        database
            .record_resolution(&pair.id, 1, &resolution)
            .unwrap();
        let epoch = [1_u8; 16];
        let marker = database.begin_target_catalog(&pair.id, 1, epoch).unwrap();
        let next = TargetCatalogRow {
            epoch,
            generation: 1,
            digest: [2_u8; 32],
        };
        database
            .acknowledge_target_catalog_resolving(&pair.id, 1, &marker, &next, &[b"a".as_slice()])
            .unwrap();
        assert_eq!(
            database.resolutions(&pair.id).unwrap(),
            Vec::<ResolutionRow>::new()
        );
        assert_eq!(database.target_catalog(&pair.id).unwrap(), Some(next));

        // A failed CAS spends nothing.
        let remaining = ResolutionRow {
            path: b"b".to_vec(),
            ..resolution
        };
        database.record_resolution(&pair.id, 1, &remaining).unwrap();
        let later = TargetCatalogRow {
            epoch,
            generation: 2,
            digest: [3_u8; 32],
        };
        assert!(
            database
                .acknowledge_target_catalog_resolving(
                    &pair.id,
                    1,
                    &marker,
                    &later,
                    &[b"b".as_slice()],
                )
                .is_err()
        );
        assert_eq!(database.target_catalog(&pair.id).unwrap(), Some(next));
        assert_eq!(database.resolutions(&pair.id).unwrap(), vec![remaining]);
    }

    #[test]
    fn premature_purge_is_refused() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        database
            .change_base(&pair.id, 1, &[put(b"a", b"1")], JournalChange::None)
            .unwrap();
        assert!(matches!(
            database.purge_target_catalog_rows(&pair.id, 10),
            Err(Error::InvalidState(_))
        ));
        let epoch = [5_u8; 16];
        let marker = database.begin_target_catalog(&pair.id, 1, epoch).unwrap();
        assert!(matches!(
            database.purge_target_catalog_rows(&pair.id, 10),
            Err(Error::InvalidState(_))
        ));
        assert!(matches!(
            database.purge_target_catalog_rows(&pair.id, 0),
            Err(Error::InvalidState(_))
        ));
        assert_eq!(database.base_rows(&pair.id).unwrap().len(), 1);

        let next = TargetCatalogRow {
            epoch,
            generation: 1,
            digest: [9_u8; 32],
        };
        database
            .acknowledge_target_catalog(&pair.id, 1, &marker, &next)
            .unwrap();
        assert_eq!(
            database.purge_target_catalog_rows(&pair.id, 10).unwrap(),
            Purge::Done
        );
        assert!(database.base_rows(&pair.id).unwrap().is_empty());
    }

    #[test]
    fn bounded_purge_deletes_only_the_target_pair() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let mut other_values = values();
        other_values.left_root = b"/other-left".to_vec();
        other_values.right_root = b"/other-right".to_vec();
        let other = database.create_pair(&other_values).unwrap();
        let paths: Vec<String> = (0..5).map(|index| format!("b-{index}")).collect();
        let base: Vec<_> = paths
            .iter()
            .map(|path| put(path.as_bytes(), b"v"))
            .collect();
        let snapshots = vec![snapshot_row(b"s"), snapshot_row(b"t")];
        let journal = [
            journal_row(0, b"j-0"),
            journal_row(1, b"j-1"),
            journal_row(2, b"j-2"),
        ];
        for id in [&pair.id, &other.id] {
            database
                .change_base(id, 1, &base, JournalChange::None)
                .unwrap();
            database
                .replace_snapshot(id, 1, "left", &snapshots)
                .unwrap();
            database
                .change_base(
                    id,
                    1,
                    &[],
                    JournalChange::Record {
                        run: "run-a",
                        rows: &journal,
                    },
                )
                .unwrap();
        }
        let epoch = [6_u8; 16];
        let marker = database.begin_target_catalog(&pair.id, 1, epoch).unwrap();
        let next = TargetCatalogRow {
            epoch,
            generation: 1,
            digest: [1_u8; 32],
        };
        database
            .acknowledge_target_catalog(&pair.id, 1, &marker, &next)
            .unwrap();

        // Four rows come from the base, in table order.
        assert_eq!(
            database.purge_target_catalog_rows(&pair.id, 4).unwrap(),
            Purge::Pending
        );
        assert_eq!(database.base_rows(&pair.id).unwrap().len(), 1);
        assert_eq!(database.snapshot(&pair.id, "left").unwrap().len(), 2);
        assert_eq!(database.journal(&pair.id).unwrap().unwrap().1.len(), 3);

        // The last base row and both snapshot rows.
        assert_eq!(
            database.purge_target_catalog_rows(&pair.id, 3).unwrap(),
            Purge::Pending
        );
        assert!(database.base_rows(&pair.id).unwrap().is_empty());
        assert!(database.snapshot(&pair.id, "left").unwrap().is_empty());
        assert_eq!(database.journal(&pair.id).unwrap().unwrap().1.len(), 3);

        // The journal, and the marker survives; the other pair is untouched.
        assert_eq!(
            database.purge_target_catalog_rows(&pair.id, 10).unwrap(),
            Purge::Done
        );
        assert!(database.journal(&pair.id).unwrap().is_none());
        assert_eq!(database.target_catalog(&pair.id).unwrap(), Some(next));
        assert_eq!(database.base_rows(&other.id).unwrap().len(), 5);
        assert_eq!(database.snapshot(&other.id, "left").unwrap().len(), 2);
        assert_eq!(database.journal(&other.id).unwrap().unwrap().1.len(), 3);
    }

    #[test]
    fn legacy_writers_are_refused_after_acknowledgement() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let epoch = [2_u8; 16];
        let marker = database.begin_target_catalog(&pair.id, 1, epoch).unwrap();

        // Generation zero is unacknowledged: local writers still work.
        database
            .change_base(&pair.id, 1, &[put(b"a", b"1")], JournalChange::None)
            .unwrap();
        database
            .replace_snapshot(&pair.id, 1, "left", &[snapshot_row(b"a")])
            .unwrap();

        let next = TargetCatalogRow {
            epoch,
            generation: 1,
            digest: [3_u8; 32],
        };
        database
            .acknowledge_target_catalog(&pair.id, 1, &marker, &next)
            .unwrap();
        assert!(matches!(
            database.change_base(&pair.id, 1, &[put(b"b", b"2")], JournalChange::None),
            Err(Error::InvalidState(_))
        ));
        assert!(matches!(
            database.replace_snapshot(&pair.id, 1, "left", &[]),
            Err(Error::InvalidState(_))
        ));
        assert!(matches!(
            database.replace_snapshot_sorted(&pair.id, 1, "right", Vec::<SnapshotRow>::new()),
            Err(Error::InvalidState(_))
        ));
        assert!(matches!(
            database.replace_snapshot_data(&pair.id, 1, "right", &[] as &[SnapshotRow]),
            Err(Error::InvalidState(_))
        ));
        assert_eq!(database.base_rows(&pair.id).unwrap().len(), 1);
        assert_eq!(database.snapshot(&pair.id, "left").unwrap().len(), 1);
        assert!(database.snapshot(&pair.id, "right").unwrap().is_empty());

        // A stale revision is still a revision error; the acknowledged
        // marker refuses even the job's own revision.
        assert!(matches!(
            database.change_base(&pair.id, 2, &[put(b"b", b"2")], JournalChange::None),
            Err(Error::StaleRevision)
        ));
    }
}
