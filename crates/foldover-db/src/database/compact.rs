//! Reclaim the emptied legacy catalog after verified target migration. This
//! runs only when the live database fits in a small bounded local store and
//! no pinned readers hold an older WAL snapshot.

use std::fs::File;
use std::sync::Arc;

use super::{Database, Error, migration_backup};

const LIVE_LIMIT: u64 = 16 * 1024 * 1024;
const RECLAIM_MINIMUM: u64 = 1024 * 1024;

impl Database {
    pub fn compact_migrated_catalog(&mut self) -> Result<(), Error> {
        let migrated: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM target_catalog WHERE generation>0)",
            [],
            |row| row.get(0),
        )?;
        if !migrated {
            return Ok(());
        }
        // Do not retire the recovery copy until every migrated pair's legacy
        // data has been removed. Unmigrated pairs can still keep local state.
        let pending: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM target_catalog t WHERE t.generation>0 AND (
                EXISTS(SELECT 1 FROM base_entry b WHERE b.pair_id=t.pair_id) OR
                EXISTS(SELECT 1 FROM snapshot_entry s WHERE s.pair_id=t.pair_id) OR
                EXISTS(SELECT 1 FROM run_journal j WHERE j.pair_id=t.pair_id)))",
            [],
            |row| row.get(0),
        )?;
        if pending {
            return Ok(());
        }
        let page_size: i64 = self
            .connection
            .pragma_query_value(None, "page_size", |row| row.get(0))?;
        let pages: i64 = self
            .connection
            .pragma_query_value(None, "page_count", |row| row.get(0))?;
        let free: i64 = self
            .connection
            .pragma_query_value(None, "freelist_count", |row| row.get(0))?;
        let live = pages.saturating_sub(free).saturating_mul(page_size) as u64;
        if live > LIVE_LIMIT {
            return Ok(());
        }
        if Arc::strong_count(&self._writer) != 1 {
            return Err(Error::InvalidState(
                "A pinned reader is delaying local catalog compaction",
            ));
        }
        if free.saturating_mul(page_size) as u64 >= RECLAIM_MINIMUM {
            // VACUUM is transactional, retains the inode and writer lease,
            // and writes only the bounded live database into the WAL. SQLite's
            // checkpoint then truncates the old multi-GB main file safely.
            self.connection.execute_batch("VACUUM")?;
        }
        let busy: i64 =
            self.connection
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
        if busy != 0 {
            return Err(Error::InvalidState(
                "A database reader is delaying local catalog compaction",
            ));
        }
        // A compact current-schema recovery copy replaces the large upgrade
        // copy only after the target catalog and local pointer are durable.
        if migration_backup::has_large_backup(&self.directory, RECLAIM_MINIMUM)? {
            migration_backup::create_local_recovery(&self.connection, &self.directory)?;
        }
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{location, values};
    use super::*;
    use std::fs;

    #[test]
    fn emptied_catalog_reclaims_file_and_upgrade_backup() {
        let (directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        database
            .connection
            .execute(
                "INSERT INTO base_entry VALUES (?1,?2,?3,?4)",
                rusqlite::params![pair.id, b"file", vec![1u8; 3 * 1024 * 1024], b"v"],
            )
            .unwrap();
        migration_backup::create(&database.connection, directory.path(), 30).unwrap();
        let first = database
            .begin_target_catalog(&pair.id, pair.revision, [1; 16])
            .unwrap();
        let next = super::super::TargetCatalogRow {
            generation: 1,
            digest: [2; 32],
            ..first
        };
        database
            .acknowledge_target_catalog(&pair.id, pair.revision, &first, &next)
            .unwrap();
        database.compact_migrated_catalog().unwrap();
        assert!(
            directory
                .path()
                .join("state-before-v30-completed.db")
                .exists()
        );
        while database.purge_target_catalog_rows(&pair.id, 100).unwrap()
            == super::super::Purge::Pending
        {}
        database.compact_migrated_catalog().unwrap();
        assert!(
            fs::metadata(directory.path().join("state.db"))
                .unwrap()
                .len()
                < RECLAIM_MINIMUM
        );
        assert!(
            !directory
                .path()
                .join("state-before-v30-completed.db")
                .exists()
        );
        assert!(directory.path().join("state-local-completed.db").exists());
    }
}
