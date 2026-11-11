//! Job-owned target catalog sessions. Endpoint I/O happens outside the engine
//! mutex; only acknowledgement of a published checkpoint takes that mutex.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::io;
use std::sync::Arc;

use foldover_endpoint::{CatalogFiles, Endpoint, LeaseHolder};
use foldover_state::target_store::{BlobStore, CHECKPOINT_LIMIT, Catalog, Checkpoint};
use foldover_state::{Journal, JournalUpdate, SnapshotEntry};
use foldover_types::{FolderRecords, PairConfig, PairRoot, ScanControl, Side, SyncMode, SyncPath};

use crate::{Commit, Error, PendingRunInputs, RunInputs};

pub(crate) fn target_side(pair: &PairConfig) -> Option<Side> {
    if pair.draft().options().records() == FolderRecords::SourceOnly {
        return None;
    }
    match pair.draft().mode() {
        SyncMode::Sync => None,
        SyncMode::Mirror { source } | SyncMode::Backup { source } => Some(source.other()),
    }
}

struct Store(Arc<CatalogFiles>);
impl BlobStore for Store {
    fn read(&self, name: &str, limit: usize) -> io::Result<Option<Vec<u8>>> {
        self.0.read(name, limit)
    }
    fn write_new(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.0.write_new(name, bytes)
    }
}

struct CheckedStore<'a> {
    store: &'a Store,
    control: RefCell<&'a mut dyn FnMut() -> ScanControl>,
}
impl BlobStore for CheckedStore<'_> {
    fn read(&self, name: &str, limit: usize) -> io::Result<Option<Vec<u8>>> {
        self.check()?;
        self.store.read(name, limit)
    }
    fn write_new(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.check()?;
        self.store.write_new(name, bytes)
    }
}
impl CheckedStore<'_> {
    fn check(&self) -> io::Result<()> {
        if (self.control.borrow_mut())() == ScanControl::Cancel {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Reading or migrating the backup catalog was cancelled",
            ));
        }
        Ok(())
    }
}

pub struct TargetRun {
    store: Store,
    catalog: Catalog,
    acknowledged: Checkpoint,
    previous: Option<Catalog>,
    uncertain: bool,
    verified: Cell<bool>,
}

impl TargetRun {
    /// Open or migrate one pair. No local data is discarded by this operation.
    pub fn open(
        pending: &PendingRunInputs,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<Option<Self>, Error> {
        let Some(checkpoint) = pending.checkpoint() else {
            return Ok(None);
        };
        let pair = pending.configuration();
        let side = target_side(pair).ok_or_else(|| {
            io::Error::other("Target catalog routing disagrees with pair settings")
        })?;
        let root: &PairRoot = match side {
            Side::Left => pair.draft().left(),
            Side::Right => pair.draft().right(),
        };
        let endpoint = Arc::new(Endpoint::open(root, pending.reach())?);
        let files = Arc::new(CatalogFiles::open(
            endpoint,
            pair.id(),
            LeaseHolder {
                id: format!("catalog/{}", uuid::Uuid::new_v4()),
                label: "Foldover backup catalog".into(),
            },
            pair.draft().options().durability(),
            control,
        )?);
        if pending
            .identities()
            .side(side)
            .is_some_and(|id| id != files.identity())
        {
            return Err(io::Error::other(
                "The backup target has been replaced; its catalog was not migrated",
            )
            .into());
        }
        let store = Store(files);
        let mut candidates: Vec<_> = store
            .0
            .list()?
            .iter()
            .filter_map(|name| Checkpoint::parse_filename(name))
            .filter(|candidate| {
                candidate.epoch == checkpoint.epoch && candidate.generation >= checkpoint.generation
            })
            .collect();
        candidates.sort_by_key(|candidate| candidate.generation);
        if candidates
            .windows(2)
            .any(|window| window[0].generation == window[1].generation)
        {
            return Err(
                io::Error::other("The backup target contains conflicting checkpoints").into(),
            );
        }
        let mut previous = None;
        let catalog = if checkpoint.generation > 0 {
            let mut current = load(&store, checkpoint.clone())?;
            if let Some(latest) = candidates
                .last()
                .filter(|candidate| **candidate != *checkpoint)
            {
                if latest.generation != checkpoint.generation + 1 {
                    return Err(io::Error::other(
                        "The target catalog advanced beyond this Mac's acknowledged state",
                    )
                    .into());
                }
                let recovered = load(&store, latest.clone())?;
                if recovered.previous != checkpoint.digest {
                    return Err(io::Error::other(
                        "The target checkpoint history does not match this Mac",
                    )
                    .into());
                }
                validate_tables(&store, &recovered)?;
                previous = Some(current);
                current = recovered;
            }
            current
        } else if let Some(latest) = candidates.last() {
            if candidates.len() != 1 || latest.generation != 1 {
                return Err(io::Error::other(
                    "Unexpected checkpoint during target catalog migration",
                )
                .into());
            }
            let recovered = load(&store, latest.clone())?;
            if recovered.previous != [0; 32] {
                return Err(io::Error::other("Invalid migration checkpoint history").into());
            }
            validate_tables(&store, &recovered)?;
            recovered
        } else {
            let mut catalog = Catalog::empty(
                pair.id(),
                store.0.identity(),
                checkpoint.clone(),
                description(pair),
            );
            let checked = CheckedStore {
                store: &store,
                control: RefCell::new(control),
            };
            pending.records().import_target(&checked, &mut catalog)?;
            let (catalog, bytes) = catalog.next()?;
            publish(&store, &catalog, &bytes)?;
            // Verify every referenced block before the first local acknowledgement.
            validate_tables(&checked, &catalog)?;
            catalog
        };
        if catalog.pair != pair.id() || catalog.target != store.0.identity() {
            return Err(io::Error::other(
                "This checkpoint belongs to a different backup target or pair",
            )
            .into());
        }
        if previous.is_none() && catalog.checkpoint.generation > 1 {
            let position = Checkpoint {
                epoch: catalog.checkpoint.epoch,
                generation: catalog.checkpoint.generation - 1,
                digest: catalog.previous,
            };
            if let Some(bytes) = store.read(&position.filename(), CHECKPOINT_LIMIT)? {
                previous = Some(Catalog::decode(position, &bytes)?);
            }
        }
        Ok(Some(Self {
            store,
            catalog,
            acknowledged: checkpoint.clone(),
            previous,
            uncertain: false,
            verified: Cell::new(false),
        }))
    }

    /// Keep the lease alive from the run's cancellation/progress callback.
    pub fn files(&self) -> Arc<CatalogFiles> {
        Arc::clone(&self.store.0)
    }

    pub fn decode(
        &self,
        pending: PendingRunInputs,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<RunInputs, Error> {
        let inputs = pending.decode_target(
            &self.catalog,
            &CheckedStore {
                store: &self.store,
                control: RefCell::new(control),
            },
        )?;
        self.verified.set(true);
        Ok(inputs)
    }
    pub fn journal(&self) -> Result<Option<Journal>, Error> {
        Ok(self.catalog.read_journal(&self.store)?)
    }

    pub fn acknowledge(
        &mut self,
        ack: &mut impl FnMut(&Checkpoint, &Checkpoint, &[SyncPath]) -> Result<(), Error>,
    ) -> Result<(), Error> {
        if self.catalog.checkpoint != self.acknowledged {
            self.store.0.fence()?;
            ack(
                &self.acknowledged,
                &self.catalog.checkpoint,
                &self.catalog.spent,
            )?;
            self.acknowledged = self.catalog.checkpoint.clone();
        }
        Ok(())
    }

    pub fn snapshot(
        &mut self,
        side: Side,
        entries: impl IntoIterator<Item = SnapshotEntry>,
        ack: &mut impl FnMut(&Checkpoint, &Checkpoint, &[SyncPath]) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let next = self.catalog.replace_snapshot(&self.store, side, entries)?;
        self.save(next, ack)
    }

    /// Identities and automation holds remain small local records. The caller
    /// persists a hold before dispatching Undoing here.
    pub fn commit(
        &mut self,
        commit: Commit<'_>,
        ack: &mut impl FnMut(&Checkpoint, &Checkpoint, &[SyncPath]) -> Result<(), Error>,
    ) -> Result<(), Error> {
        if let Commit::Undone { run, updates } = &commit
            && updates.len() > crate::run::COMMIT_BATCH
        {
            // The undo executor gathers refilled directories until the end.
            // Persist them in bounded checkpoints before spending the journal;
            // an interruption leaves that journal available for another undo.
            for batch in updates.chunks(crate::run::COMMIT_BATCH) {
                self.commit(Commit::Undoing { updates: batch }, ack)?;
            }
            return self.commit(Commit::Undone { run, updates: &[] }, ack);
        }
        let (updates, journal) = match commit {
            Commit::Identities { .. } => {
                return Err(
                    io::Error::other("Endpoint identities must be recorded locally").into(),
                );
            }
            Commit::Base {
                run,
                updates,
                journal,
            } => (
                updates,
                JournalUpdate::Record {
                    run,
                    entries: journal,
                },
            ),
            Commit::Undoing { updates } => (updates, JournalUpdate::Hold),
            Commit::Undid { run, seqs } => (&[][..], JournalUpdate::Undid { run, seqs }),
            Commit::Undone { run, updates } => (updates, JournalUpdate::Spend { run }),
        };
        let next = self.catalog.change(&self.store, updates, journal)?;
        self.save(next, ack)
    }

    fn save(
        &mut self,
        next: Catalog,
        ack: &mut impl FnMut(&Checkpoint, &Checkpoint, &[SyncPath]) -> Result<(), Error>,
    ) -> Result<(), Error> {
        // A failed acknowledgement must stop the job. Never publish a second
        // unacknowledged successor, even if the executor retries a final batch.
        if self.uncertain || self.catalog.checkpoint != self.acknowledged {
            return Err(io::Error::other(
                "The previous target checkpoint has not been acknowledged locally",
            )
            .into());
        }
        let (next, bytes) = next.next()?;
        self.uncertain = true;
        publish(&self.store, &next, &bytes)?;
        self.previous = Some(std::mem::replace(&mut self.catalog, next));
        self.acknowledge(ack)?;
        self.uncertain = false;
        Ok(())
    }

    /// Retain the current and previous complete checkpoints. A failed cleanup
    /// affects target space only; it never changes the authoritative catalog.
    pub fn collect(&self) -> Result<(), Error> {
        if !self.verified.get() || self.uncertain || self.catalog.checkpoint != self.acknowledged {
            return Ok(());
        }
        self.store.0.fence()?;
        let mut keep: BTreeSet<String> = self.catalog.files().into_iter().collect();
        keep.insert(self.catalog.checkpoint.filename());
        if let Some(previous) = &self.previous {
            keep.extend(previous.files());
            keep.insert(previous.checkpoint.filename());
        }
        for name in self.store.0.list()? {
            if keep.contains(&name) {
                continue;
            }
            let block = name.strip_suffix(".block").is_some_and(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            });
            let pending = name
                .strip_prefix("pending-")
                .and_then(|name| name.strip_suffix(".tmp"))
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok());
            if block || pending || Checkpoint::parse_filename(&name).is_some() {
                self.store.0.remove(&name)?;
            }
        }
        Ok(())
    }
}

impl Drop for TargetRun {
    fn drop(&mut self) {
        if let Err(error) = self.collect() {
            eprintln!("foldoverd: target catalog cleanup will retry: {error}");
        }
    }
}

fn load(store: &Store, checkpoint: Checkpoint) -> io::Result<Catalog> {
    let bytes = store.read(&checkpoint.filename(), CHECKPOINT_LIMIT)?.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound,
        "The backup target is missing an acknowledged checkpoint; restore its catalog before syncing"))?;
    Catalog::decode(checkpoint, &bytes)
}

fn publish(store: &Store, catalog: &Catalog, bytes: &[u8]) -> io::Result<()> {
    store.0.fence()?;
    let name = catalog.checkpoint.filename();
    if let Err(error) = store.write_new(&name, bytes) {
        // A connection can fail after the server's rename. Only exact readback
        // can turn that ambiguous result into a successful publication.
        if store.read(&name, CHECKPOINT_LIMIT)?.as_deref() != Some(bytes) {
            return Err(error);
        }
    }
    if store.read(&name, CHECKPOINT_LIMIT)?.as_deref() != Some(bytes) {
        return Err(io::Error::other(
            "The backup target did not retain its complete checkpoint",
        ));
    }
    store.0.fence()
}

fn validate_tables(store: &dyn BlobStore, catalog: &Catalog) -> io::Result<()> {
    for table in [
        &catalog.base,
        &catalog.snapshots[0],
        &catalog.snapshots[1],
        &catalog.journal,
    ] {
        table.visit(store, &mut |_, _| Ok(()))?;
    }
    Ok(())
}

fn description(pair: &PairConfig) -> String {
    // Informational recovery context, never parsed as authority or credentials.
    let draft = pair.draft();
    format!(
        "Foldover pair {} revision {}\nName: {}\nLeft: {:?}\nRight: {:?}\nMode: {:?}\nMetadata: {:?}\n",
        pair.id(),
        pair.revision(),
        draft.name(),
        draft.left(),
        draft.right(),
        draft.mode(),
        draft.options().metadata()
    )
}

#[cfg(test)]
mod tests;
