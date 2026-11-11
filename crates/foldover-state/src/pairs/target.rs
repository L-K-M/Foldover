//! The local database keeps only the target catalog's acknowledged position.

use foldover_db::TargetCatalogRow;
use foldover_types::{PairId, Side, SyncPath};
use std::io;

use super::{Error, PairStore, RunRecords, SnapshotEntry, database_revision, side_name};
use crate::base::{decode, decode_entry};
use crate::target_store::catalog::{base_entry, journal_entry, other, snapshot_entry};
use crate::target_store::{BlobStore, Catalog, Checkpoint, TableRoot};

impl PairStore {
    pub fn purge_migrated_catalogs(&mut self, limit: usize) -> Result<super::Purge, Error> {
        for pair in self.pairs()? {
            if self
                .target_checkpoint(pair.id())?
                .is_some_and(|position| position.generation > 0)
                && self.purge_target_catalog_rows(pair.id(), limit)? == super::Purge::Pending
            {
                return Ok(super::Purge::Pending);
            }
        }
        self.database.compact_migrated_catalog()?;
        Ok(super::Purge::Done)
    }

    pub fn hold_automation(&mut self, id: PairId) -> Result<(), Error> {
        Ok(self.database.hold_automation(id.as_bytes())?)
    }

    pub fn target_checkpoint(&self, id: PairId) -> Result<Option<Checkpoint>, Error> {
        Ok(self.database.target_catalog(id.as_bytes())?.map(checkpoint))
    }

    pub fn begin_target_catalog(
        &mut self,
        id: PairId,
        revision: u64,
        epoch: [u8; 16],
    ) -> Result<Checkpoint, Error> {
        Ok(checkpoint(self.database.begin_target_catalog(
            id.as_bytes(),
            database_revision(revision)?,
            epoch,
        )?))
    }

    pub fn acknowledge_target_catalog(
        &mut self,
        id: PairId,
        revision: u64,
        expected: &Checkpoint,
        next: &Checkpoint,
        spent: &[SyncPath],
    ) -> Result<(), Error> {
        let paths: Vec<_> = spent.iter().map(SyncPath::as_bytes).collect();
        Ok(self.database.acknowledge_target_catalog_resolving(
            id.as_bytes(),
            database_revision(revision)?,
            &row(expected),
            &row(next),
            &paths,
        )?)
    }

    pub fn purge_target_catalog_rows(
        &mut self,
        id: PairId,
        limit: usize,
    ) -> Result<super::Purge, Error> {
        Ok(self
            .database
            .purge_target_catalog_rows(id.as_bytes(), limit)?)
    }
}

fn checkpoint(row: TargetCatalogRow) -> Checkpoint {
    Checkpoint {
        epoch: row.epoch,
        generation: row.generation,
        digest: row.digest,
    }
}
fn row(checkpoint: &Checkpoint) -> TargetCatalogRow {
    TargetCatalogRow {
        epoch: checkpoint.epoch,
        generation: checkpoint.generation,
        digest: checkpoint.digest,
    }
}

impl RunRecords {
    /// Stream the pinned legacy catalog directly into target blocks. The local
    /// transaction remains untouched until the caller verifies publication.
    pub fn import_target(&self, store: &dyn BlobStore, catalog: &mut Catalog) -> io::Result<()> {
        catalog.base = TableRoot::from_visitor(store, |emit| {
            self.view
                .base_rows(|row| {
                    let path = SyncPath::try_from(row.path).map_err(other)?;
                    let entry = decode_entry(row.left_version, row.right_version).map_err(other)?;
                    emit(base_entry(&path, entry))
                })
                .map_err(other)
        })?;
        for (index, side) in [Side::Left, Side::Right].into_iter().enumerate() {
            catalog.snapshots[index] = TableRoot::from_visitor(store, |emit| {
                self.view
                    .snapshot(side_name(side), |row| {
                        let path = SyncPath::try_from(row.path).map_err(other)?;
                        // Match the legacy cache reader: bad hints require a rescan.
                        let Ok(version) = decode(row.version) else {
                            return Ok(());
                        };
                        emit(snapshot_entry(&SnapshotEntry {
                            path,
                            version,
                            fingerprint: row.fingerprint.to_vec(),
                        }))
                    })
                    .map_err(other)
            })?;
        }
        catalog.run = self.view.journal_run().map_err(other)?;
        catalog.journal = TableRoot::from_visitor(store, |emit| {
            self.view
                .journal_rows(|row| {
                    emit(journal_entry(&super::journal::entry(row).map_err(other)?)?)
                })
                .map_err(other)
        })?;
        Ok(())
    }
}
