//! Saved configuration for a later run. Active rows remain the authority for
//! in-flight base, snapshot and journal commits until explicit activation.

use super::*;

pub(super) fn visible_pairs() -> String {
    format!(
        "(SELECT {PAIR_COLUMNS} FROM pending_pair WHERE id NOT IN (SELECT pair_id FROM pair_removal) UNION ALL SELECT {PAIR_COLUMNS} FROM pair WHERE NOT EXISTS (SELECT 1 FROM pending_pair WHERE pending_pair.id=pair.id) AND id NOT IN (SELECT pair_id FROM pair_removal))"
    )
}

pub(super) fn active_revision(connection: &Connection, id: &[u8]) -> Result<i64, Error> {
    connection
        .query_row(
            "SELECT revision FROM pair WHERE id=?1 AND id NOT IN (SELECT pair_id FROM pair_removal)",
            [id],
            |row| {
            row.get(0)
        })
        .optional()?
        .ok_or(Error::PairNotFound)
}

pub(super) fn check_revision(
    connection: &Connection,
    id: &[u8],
    expected: i64,
) -> Result<(), Error> {
    let revision: Option<i64> = connection
        .query_row(
            &format!("SELECT revision FROM {} WHERE id=?1", visible_pairs()),
            [id],
            |row| row.get(0),
        )
        .optional()?;
    match revision {
        Some(revision) if revision == expected => Ok(()),
        Some(_) => Err(Error::StaleRevision),
        None => Err(Error::PairNotFound),
    }
}

impl Database {
    /// Configuration held by a running job, before any saved replacement.
    pub fn active_pair(&self, id: &[u8]) -> Result<Option<PairRow>, Error> {
        Ok(self
            .connection
            .query_row(
                &format!("SELECT {PAIR_COLUMNS} FROM pair WHERE id=?1 AND id NOT IN (SELECT pair_id FROM pair_removal)"),
                [id],
                pair_row,
            )
            .optional()?)
    }

    /// Persist a replacement for the next run without changing an active run.
    pub fn defer_pair_update(
        &mut self,
        id: &[u8],
        revision: i64,
        values: &PairValues,
    ) -> Result<PairRow, Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_revision(&transaction, id, revision)?;
        let next_revision = revision
            .checked_add(1)
            .ok_or(Error::InvalidState("Pair revision overflow"))?;
        transaction.execute(
            &format!("INSERT OR REPLACE INTO pending_pair ({PAIR_COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)"),
            params![id, next_revision, values.name, values.left_root, values.right_root, values.mode,
                values.modification_times, values.extended_attributes, values.permissions, values.durability,
                values.schedule, values.schedule_minutes, values.filters, values.size_limit, values.age_limit,
                values.canary, values.schedule_time, values.schedule_days, values.records, values.bandwidth_limit,
                values.review_policy, values.removal_protection],
        )?;
        let row = transaction.query_row(
            &format!("SELECT {PAIR_COLUMNS} FROM pending_pair WHERE id=?1"),
            [id],
            pair_row,
        )?;
        transaction.commit()?;
        Ok(row)
    }

    /// Apply the last saved replacement before admitting the pair's next run.
    pub fn activate_pair_update(&mut self, id: &[u8]) -> Result<(), Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        activate(&transaction, id)?;
        transaction.commit()?;
        Ok(())
    }

    /// Recover saved replacements after a restart, when no old jobs exist.
    pub fn activate_pair_updates(&mut self) -> Result<(), Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = {
            let mut statement = transaction.prepare("SELECT id FROM pending_pair ORDER BY id")?;
            statement
                .query_map([], |row| row.get::<_, Vec<u8>>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for id in ids {
            activate(&transaction, &id)?;
        }
        transaction.commit()?;
        Ok(())
    }
}

fn activate(connection: &Connection, id: &[u8]) -> Result<(), Error> {
    let revision = active_revision(connection, id)?;
    let pending = connection
        .query_row(
            &format!("SELECT {PAIR_COLUMNS} FROM pending_pair WHERE id=?1"),
            [id],
            pair_row,
        )
        .optional()?;
    if let Some(pending) = pending {
        if pending.revision <= revision {
            return Err(Error::InvalidState("Pending pair revision is not newer"));
        }
        apply_update(connection, id, revision, pending.revision, &pending.values)?;
    }
    Ok(())
}

pub(super) fn apply_update(
    transaction: &Connection,
    id: &[u8],
    revision: i64,
    next_revision: i64,
    values: &PairValues,
) -> Result<PairRow, Error> {
    // A base and conflict choices hold versions of these roots under
    // these metadata categories; changing either invalidates them. So do
    // a changed mode or records, which reroute what the target must hold.
    // The target catalog marker is reset with them, so the next job
    // imports a fresh baseline under a new epoch. Name, schedule and the
    // other display-only edits keep the marker and the caches.
    for table in [
        "base_entry",
        "resolution",
        "snapshot_entry",
        "run_journal",
        "target_catalog",
    ] {
        transaction.execute(
                &format!("DELETE FROM {table} WHERE pair_id=?1 AND EXISTS (SELECT 1 FROM pair WHERE id=?1 AND revision=?2 AND (left_root!=?3 OR right_root!=?4 OR modification_times!=?5 OR extended_attributes!=?6 OR permissions!=?7 OR mode!=?8 OR records!=?9))"),
                params![
                    id,
                    revision,
                    values.left_root,
                    values.right_root,
                    values.modification_times,
                    values.extended_attributes,
                    values.permissions,
                    values.mode,
                    values.records
                ],
            )?;
    }
    let changed = transaction.execute(
            // A changed root is a different endpoint; forget its identity.
            // So do changed records, or a changed mode while records stay
            // with the source: a folder without records has an identity
            // only this agent knows. SET expressions read the row as it
            // was, so `records=?19` and `mode=?6` hold only when unchanged.
            "UPDATE pair SET revision=?23,name=?3,left_root=?4,right_root=?5,mode=?6,modification_times=?7,extended_attributes=?8,permissions=?9,durability=?10,schedule=?11,schedule_minutes=?12,filters=?13,size_limit=?14,age_limit=?15,canary=?16,schedule_time=?17,schedule_days=?18,records=?19,bandwidth_limit=?20,review_policy=?21,removal_protection=?22,left_identity=CASE WHEN left_root=?4 AND records=?19 AND (records='both' OR mode=?6) THEN left_identity END,right_identity=CASE WHEN right_root=?5 AND records=?19 AND (records='both' OR mode=?6) THEN right_identity END,left_mount=CASE WHEN left_root=?4 THEN left_mount END,right_mount=CASE WHEN right_root=?5 THEN right_mount END WHERE id=?1 AND revision=?2",
            params![
                id,
                revision,
                values.name,
                values.left_root,
                values.right_root,
                values.mode,
                values.modification_times,
                values.extended_attributes,
                values.permissions,
                values.durability,
                values.schedule,
                values.schedule_minutes,
                values.filters,
                values.size_limit,
                values.age_limit,
                values.canary,
                values.schedule_time,
                values.schedule_days,
                values.records,
                values.bandwidth_limit,
                values.review_policy,
                values.removal_protection,
                next_revision
            ],
        )?;
    if changed == 0 {
        return Err(revision_error(transaction, id)?);
    }
    transaction.execute("DELETE FROM pending_pair WHERE id=?1", [id])?;
    Ok(transaction.query_row(
        &format!("SELECT {PAIR_COLUMNS} FROM pair WHERE id=?1"),
        [id],
        pair_row,
    )?)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{location, values};
    use super::*;

    fn put<'a>(path: &'a [u8], version: &'a [u8]) -> BaseChange<'a> {
        BaseChange::Put {
            path,
            left_version: version,
            right_version: version,
        }
    }

    #[test]
    fn deferred_edits_preserve_the_running_revision() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let original = database.create_pair(&values()).unwrap();
        let mut changed = values();
        changed.name = "Next run".into();
        let saved = database
            .defer_pair_update(&original.id, 1, &changed)
            .unwrap();
        assert_eq!(saved.revision, 2);
        assert_eq!(
            database.pair(&original.id).unwrap().unwrap().values.name,
            "Next run"
        );
        database
            .change_base(
                &original.id,
                1,
                &[put(b"finished", b"v1")],
                JournalChange::Record {
                    run: "old-run",
                    rows: &[JournalRow {
                        seq: 0,
                        path: b"finished".to_vec(),
                        side: "right".into(),
                        action: "create".into(),
                        version: b"v1".to_vec(),
                        base: None,
                        source: None,
                    }],
                },
            )
            .unwrap();
        database
            .replace_snapshot(
                &original.id,
                1,
                "left",
                &[SnapshotRow {
                    path: b"finished".to_vec(),
                    fingerprint: b"stat".to_vec(),
                    version: b"v1".to_vec(),
                }],
            )
            .unwrap();
        database
            .record_identities(&original.id, 1, &[1; 16], &[2; 16])
            .unwrap();
        assert_eq!(database.base_rows(&original.id).unwrap().len(), 1);
        assert_eq!(
            database.journal(&original.id).unwrap().unwrap().0,
            "old-run"
        );
        assert_eq!(database.snapshot(&original.id, "left").unwrap().len(), 1);
        database.activate_pair_update(&original.id).unwrap();
        assert_eq!(
            database
                .active_pair(&original.id)
                .unwrap()
                .unwrap()
                .revision,
            2
        );
        assert_eq!(database.base_rows(&original.id).unwrap().len(), 1);
        assert!(database.journal(&original.id).unwrap().is_some());
        assert_eq!(database.snapshot(&original.id, "left").unwrap().len(), 1);
        assert_eq!(
            database.identities(&original.id).unwrap().left,
            Some(vec![1; 16])
        );
        assert!(matches!(
            database.change_base(&original.id, 1, &[], JournalChange::None),
            Err(Error::StaleRevision)
        ));
    }

    #[test]
    fn the_latest_saved_edit_survives_reopening_and_activates_at_its_revision() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let original = database.create_pair(&values()).unwrap();
        let mut next = values();
        next.name = "First saved edit".into();
        database.defer_pair_update(&original.id, 1, &next).unwrap();
        next.name = "Latest saved edit".into();
        next.left_root = b"/new-home/\xff".to_vec();
        database.defer_pair_update(&original.id, 2, &next).unwrap();
        assert!(matches!(
            database.defer_pair_update(&original.id, 1, &values()),
            Err(Error::StaleRevision)
        ));
        assert!(matches!(
            database.update_pair(&original.id, 2, &values()),
            Err(Error::StaleRevision)
        ));
        assert!(matches!(
            database.remove_pair(&original.id, 2),
            Err(Error::StaleRevision)
        ));
        assert_eq!(
            database
                .active_pair(&original.id)
                .unwrap()
                .unwrap()
                .revision,
            1
        );
        drop(database);

        let mut database = Database::open(&location).unwrap();
        let pending = database.pair(&original.id).unwrap().unwrap();
        assert_eq!(pending.revision, 3);
        assert_eq!(pending.values.name, next.name);
        assert_eq!(pending.values.left_root, next.left_root);
        assert_eq!(database.pairs().unwrap()[0].revision, 3);
        assert_eq!(database.pairs_after(None, 1).unwrap()[0].revision, 3);
        assert_eq!(
            database
                .active_pair(&original.id)
                .unwrap()
                .unwrap()
                .revision,
            1
        );
        database.activate_pair_updates().unwrap();
        assert_eq!(
            database
                .active_pair(&original.id)
                .unwrap()
                .unwrap()
                .revision,
            3
        );
        assert_eq!(
            database
                .active_pair(&original.id)
                .unwrap()
                .unwrap()
                .values
                .left_root,
            next.left_root
        );
        database.activate_pair_update(&original.id).unwrap();
        assert_eq!(
            database
                .active_pair(&original.id)
                .unwrap()
                .unwrap()
                .revision,
            3
        );
    }

    #[test]
    fn activation_invalidates_versions_only_after_the_old_run_finishes() {
        for change_root in [false, true] {
            let (_directory, location) = location();
            let mut database = Database::open(&location).unwrap();
            let original = database.create_pair(&values()).unwrap();
            let mut next = values();
            if change_root {
                next.right_root = b"/new-target".to_vec();
            } else {
                next.extended_attributes = "ignore".into();
            }
            database.defer_pair_update(&original.id, 1, &next).unwrap();
            database
                .change_base(
                    &original.id,
                    1,
                    &[put(b"old", b"v")],
                    JournalChange::Record {
                        run: "old-run",
                        rows: &[JournalRow {
                            seq: 0,
                            path: b"old".to_vec(),
                            side: "right".into(),
                            action: "create".into(),
                            version: b"v".to_vec(),
                            base: None,
                            source: None,
                        }],
                    },
                )
                .unwrap();
            database
                .replace_snapshot(
                    &original.id,
                    1,
                    "right",
                    &[SnapshotRow {
                        path: b"old".to_vec(),
                        fingerprint: vec![1],
                        version: vec![2],
                    }],
                )
                .unwrap();
            database
                .record_resolution(
                    &original.id,
                    1,
                    &ResolutionRow {
                        path: b"conflict".to_vec(),
                        keep: "left".into(),
                        left_version: Some(vec![1]),
                        right_version: None,
                    },
                )
                .unwrap();
            assert_eq!(database.base_rows(&original.id).unwrap().len(), 1);
            database.activate_pair_update(&original.id).unwrap();
            assert!(database.base_rows(&original.id).unwrap().is_empty());
            assert!(database.snapshot(&original.id, "right").unwrap().is_empty());
            assert!(database.journal(&original.id).unwrap().is_none());
            assert!(database.resolutions(&original.id).unwrap().is_empty());
        }
    }

    #[test]
    fn reset_vs_preserved_marker() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        database
            .change_base(
                &pair.id,
                1,
                &[put(b"base", b"1")],
                JournalChange::Record {
                    run: "old-run",
                    rows: &[JournalRow {
                        seq: 0,
                        path: b"base".to_vec(),
                        side: "left".into(),
                        action: "create".into(),
                        version: b"1".to_vec(),
                        base: None,
                        source: None,
                    }],
                },
            )
            .unwrap();
        database
            .replace_snapshot(
                &pair.id,
                1,
                "left",
                &[SnapshotRow {
                    path: b"base".to_vec(),
                    fingerprint: b"f".to_vec(),
                    version: b"1".to_vec(),
                }],
            )
            .unwrap();
        database
            .record_resolution(
                &pair.id,
                1,
                &ResolutionRow {
                    path: b"base".to_vec(),
                    keep: "left".into(),
                    left_version: Some(b"l".to_vec()),
                    right_version: None,
                },
            )
            .unwrap();
        let epoch = [4_u8; 16];
        let marker = database.begin_target_catalog(&pair.id, 1, epoch).unwrap();
        let acknowledged = TargetCatalogRow {
            epoch,
            generation: 1,
            digest: [9_u8; 32],
        };
        database
            .acknowledge_target_catalog(&pair.id, 1, &marker, &acknowledged)
            .unwrap();

        // A name-only deferred edit keeps the marker and every local cache.
        let mut renamed = values();
        renamed.name = "Renamed".into();
        database.defer_pair_update(&pair.id, 1, &renamed).unwrap();
        database.activate_pair_update(&pair.id).unwrap();
        assert_eq!(
            database.target_catalog(&pair.id).unwrap(),
            Some(acknowledged)
        );
        assert_eq!(database.active_pair(&pair.id).unwrap().unwrap().revision, 2);
        assert_eq!(database.base_rows(&pair.id).unwrap().len(), 1);
        assert_eq!(database.snapshot(&pair.id, "left").unwrap().len(), 1);
        assert_eq!(database.journal(&pair.id).unwrap().unwrap().1.len(), 1);
        assert_eq!(database.resolutions(&pair.id).unwrap().len(), 1);

        // So does a schedule-only direct edit.
        let mut scheduled = renamed.clone();
        scheduled.schedule = "every".into();
        scheduled.schedule_minutes = Some(30);
        database.update_pair(&pair.id, 2, &scheduled).unwrap();
        assert_eq!(
            database.target_catalog(&pair.id).unwrap(),
            Some(acknowledged)
        );
        assert_eq!(database.base_rows(&pair.id).unwrap().len(), 1);
        assert_eq!(database.snapshot(&pair.id, "left").unwrap().len(), 1);
        assert_eq!(database.journal(&pair.id).unwrap().unwrap().1.len(), 1);
        assert_eq!(database.resolutions(&pair.id).unwrap().len(), 1);

        // Changing a metadata policy resets the marker and invalidates every
        // local cache.
        let mut metadata = scheduled.clone();
        metadata.permissions = "sync".into();
        database.update_pair(&pair.id, 3, &metadata).unwrap();
        assert_eq!(database.target_catalog(&pair.id).unwrap(), None);
        assert!(database.base_rows(&pair.id).unwrap().is_empty());
        assert!(database.snapshot(&pair.id, "left").unwrap().is_empty());
        assert!(database.journal(&pair.id).unwrap().is_none());
        assert!(database.resolutions(&pair.id).unwrap().is_empty());

        // A new migration begins at generation zero under the new epoch,
        // and legacy writers work again while it is unacknowledged.
        let epoch2 = [5_u8; 16];
        let begun = database.begin_target_catalog(&pair.id, 4, epoch2).unwrap();
        assert_eq!(
            begun,
            TargetCatalogRow {
                epoch: epoch2,
                generation: 0,
                digest: [0; 32]
            }
        );
        database
            .change_base(&pair.id, 4, &[put(b"fresh", b"1")], JournalChange::None)
            .unwrap();

        // A mode change resets the marker too.
        let second = TargetCatalogRow {
            epoch: epoch2,
            generation: 1,
            digest: [1_u8; 32],
        };
        database
            .acknowledge_target_catalog(&pair.id, 4, &begun, &second)
            .unwrap();
        let mut mirrored = metadata.clone();
        mirrored.mode = "mirror-left".into();
        database.update_pair(&pair.id, 4, &mirrored).unwrap();
        assert_eq!(database.target_catalog(&pair.id).unwrap(), None);

        // So does a records change.
        let epoch3 = [6_u8; 16];
        let third_begin = database.begin_target_catalog(&pair.id, 5, epoch3).unwrap();
        let third = TargetCatalogRow {
            epoch: epoch3,
            generation: 1,
            digest: [2_u8; 32],
        };
        database
            .acknowledge_target_catalog(&pair.id, 5, &third_begin, &third)
            .unwrap();
        let mut sourced = mirrored.clone();
        sourced.records = "source".into();
        database.update_pair(&pair.id, 5, &sourced).unwrap();
        assert_eq!(database.target_catalog(&pair.id).unwrap(), None);

        // And a root change.
        let epoch4 = [7_u8; 16];
        let fourth_begin = database.begin_target_catalog(&pair.id, 6, epoch4).unwrap();
        let fourth = TargetCatalogRow {
            epoch: epoch4,
            generation: 1,
            digest: [3_u8; 32],
        };
        database
            .acknowledge_target_catalog(&pair.id, 6, &fourth_begin, &fourth)
            .unwrap();
        let mut moved = sourced.clone();
        moved.right_root = b"/new-target".to_vec();
        database.update_pair(&pair.id, 6, &moved).unwrap();
        assert_eq!(database.target_catalog(&pair.id).unwrap(), None);

        // A name-only edit after a reset keeps the new marker, and the
        // marker survives reopening.
        let epoch5 = [8_u8; 16];
        let fifth_begin = database.begin_target_catalog(&pair.id, 7, epoch5).unwrap();
        let fifth = TargetCatalogRow {
            epoch: epoch5,
            generation: 1,
            digest: [4_u8; 32],
        };
        database
            .acknowledge_target_catalog(&pair.id, 7, &fifth_begin, &fifth)
            .unwrap();
        let mut renamed_again = moved.clone();
        renamed_again.name = "Final".into();
        database.update_pair(&pair.id, 7, &renamed_again).unwrap();
        assert_eq!(database.target_catalog(&pair.id).unwrap(), Some(fifth));
        drop(database);
        let database = Database::open(&location).unwrap();
        assert_eq!(database.target_catalog(&pair.id).unwrap(), Some(fifth));
    }

    #[test]
    fn an_idle_update_or_removal_uses_the_saved_revision() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let first = database.create_pair(&values()).unwrap();
        let second = database.create_pair(&values()).unwrap();
        database.defer_pair_update(&first.id, 1, &values()).unwrap();
        database
            .defer_pair_update(&second.id, 1, &values())
            .unwrap();
        let mut replacement = values();
        replacement.name = "Now active".into();
        assert_eq!(
            database
                .update_pair(&first.id, 2, &replacement)
                .unwrap()
                .revision,
            3
        );
        assert_eq!(
            database
                .active_pair(&first.id)
                .unwrap()
                .unwrap()
                .values
                .name,
            "Now active"
        );
        database.remove_pair(&second.id, 2).unwrap();
        database.activate_pair_updates().unwrap();
        assert!(database.pair(&second.id).unwrap().is_none());
        assert_eq!(
            database.active_pair(&first.id).unwrap().unwrap().revision,
            3
        );
    }

    #[test]
    fn an_invalid_deferred_edit_preserves_the_previous_saved_edit() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let mut next = values();
        next.name = "Kept edit".into();
        database.defer_pair_update(&pair.id, 1, &next).unwrap();
        next.right_root = next.left_root.clone();
        assert!(database.defer_pair_update(&pair.id, 2, &next).is_err());
        let kept = database.pair(&pair.id).unwrap().unwrap();
        assert_eq!((kept.revision, kept.values.name.as_str()), (2, "Kept edit"));
        assert_eq!(database.active_pair(&pair.id).unwrap().unwrap().revision, 1);
    }

    #[test]
    fn v26_state_migrates_without_changing_active_records() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        database
            .change_base(&pair.id, 1, &[put(b"kept", b"v")], JournalChange::None)
            .unwrap();
        // V27 to V31 added tables and one column. Removing them and
        // restoring V26's metadata recreates a V26 state.
        database.connection.execute_batch("ALTER TABLE automation_setting DROP COLUMN automatic_effort; DROP TABLE pair_pause; DROP TABLE pair_removal; DROP TABLE pending_pair; DROP TABLE target_catalog; UPDATE state_meta SET minimum_reader=26; PRAGMA user_version=26;").unwrap();
        drop(database);

        let mut database = Database::open(&location).unwrap();
        assert_eq!(
            database.pair(&pair.id).unwrap().unwrap().values.name,
            "pair"
        );
        assert_eq!(database.base_rows(&pair.id).unwrap().len(), 1);
        let next = database.defer_pair_update(&pair.id, 1, &values()).unwrap();
        assert_eq!(next.revision, 2);
        assert_eq!(database.active_pair(&pair.id).unwrap().unwrap().revision, 1);
        let floor: i64 = database
            .connection
            .query_row(
                "SELECT minimum_reader FROM state_meta WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(floor, 31);
    }
}
