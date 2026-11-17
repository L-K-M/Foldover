//! The newest writing run's operations per pair, so that run can be undone
//! (ADR 0029). Entries are stored with the base updates their writes
//! caused, in one transaction, beside the identities of the folders the
//! run wrote.

use foldover_db::{JournalChange, JournalRow};
use foldover_types::{BaselineEntry, EndpointId, EntryVersion, PairId, Side, SyncPath};

use super::{Error, PairStore, SnapshotEntry, database_revision, identity};
use crate::BaseUpdate;
use crate::base::{decode, decode_entry, encode};

const LEFT: &str = "left";
const RIGHT: &str = "right";
const CREATE: &str = "create";
const REPLACE: &str = "replace";
const METADATA: &str = "metadata";
const ARCHIVE: &str = "archive";
const MOVE: &str = "move";

/// What a run did to one path on one side, with what it left there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalAction {
    /// Wrote an entry where there was none.
    Create(EntryVersion),
    /// Replaced an entry; the old one went to the run's archive.
    Replace(EntryVersion),
    /// Changed only metadata; the old metadata was not kept.
    Metadata(EntryVersion),
    /// Moved this file or link into the run's archive, or removed this
    /// empty folder.
    Archive(EntryVersion),
    /// Moved this file or link here from `JournalEntry::moved_from`, with
    /// the version it has here now.
    Move(EntryVersion),
}

/// One write a run made, in write order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalEntry {
    pub seq: u64,
    pub path: SyncPath,
    pub side: Side,
    pub action: JournalAction,
    /// The base before the run; for a move, the base of `moved_from`.
    pub before: Option<BaselineEntry>,
    /// For a move, where the entry was.
    pub moved_from: Option<SyncPath>,
}

/// A run's writes, named by its archive folder, e.g.
/// `2026-10-18T09-30-00.123Z_0123456789abcdef0123456789abcdef_0123456789abcdef0123456789abcdef`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunJournal {
    pub run: String,
    pub entries: Vec<JournalEntry>,
    /// The identities of the folders the run wrote, left then right; a
    /// run an older agent journaled has none.
    pub identities: Option<[EndpointId; 2]>,
}

/// What Undo reverts: the newest writing run, or an interrupted run and
/// the runs that continued it (ADR 0055), oldest first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Journal {
    pub runs: Vec<RunJournal>,
}

/// How a base update touches the journal.
#[derive(Clone, Copy, Debug)]
pub enum JournalUpdate<'a> {
    None,
    /// Writes of `run` in the folders with these identities, left then
    /// right; its first batch replaces the previous run's, unless `run`
    /// continues an interrupted one.
    Record {
        run: &'a str,
        entries: &'a [JournalEntry],
        identities: [EndpointId; 2],
    },
    /// An undo is changing files: hold the pair's automatic syncs, across
    /// restarts, until a person syncs it.
    Hold,
    /// An undo reverted these entries of `run`; a retry skips them.
    Undid {
        run: &'a str,
        seqs: &'a [u64],
    },
    /// The journal's runs were undone; their entries are spent.
    Spend,
}

impl PairStore {
    /// Apply base updates and the journal change atomically, only while the
    /// pair keeps `revision`, so a run planned for other roots or another
    /// mode cannot commit.
    pub fn update_base(
        &mut self,
        id: PairId,
        revision: u64,
        updates: &[BaseUpdate],
        journal: JournalUpdate<'_>,
    ) -> Result<(), Error> {
        self.update_base_observed(id, revision, updates, journal, &[], &[])
    }

    /// `update_base`, also storing each side's entries as the writes behind
    /// the updates left them, so the next run reuses their versions unread
    /// (ADR 0056), and dropping the stored entries of `forgotten` paths,
    /// so the next run reads them (ADR 0057).
    pub fn update_base_observed(
        &mut self,
        id: PairId,
        revision: u64,
        updates: &[BaseUpdate],
        journal: JournalUpdate<'_>,
        observed: &[(Side, SnapshotEntry)],
        forgotten: &[(Side, SyncPath)],
    ) -> Result<(), Error> {
        let revision = database_revision(revision)?;
        let rows: Vec<JournalRow>;
        let undone: Vec<i64>;
        let wrote: [EndpointId; 2];
        let change = match journal {
            JournalUpdate::None => JournalChange::None,
            JournalUpdate::Hold => JournalChange::Hold,
            JournalUpdate::Undid { run, seqs } => {
                undone = seqs
                    .iter()
                    .map(|seq| i64::try_from(*seq))
                    .collect::<Result<_, _>>()
                    .map_err(|_| Error::InvalidStored("journal sequence"))?;
                JournalChange::Undid { run, seqs: &undone }
            }
            JournalUpdate::Spend => JournalChange::Spend,
            JournalUpdate::Record {
                run,
                entries,
                identities,
            } => {
                rows = entries.iter().map(row).collect::<Result<_, _>>()?;
                wrote = identities;
                let [left, right] = &wrote;
                JournalChange::Record {
                    run,
                    rows: &rows,
                    identities: [left.as_bytes(), right.as_bytes()],
                }
            }
        };
        self.change_base(id, revision, updates, change, observed, forgotten)
    }

    /// A person synced: automatic syncs may run again.
    pub fn release_automation(&mut self, id: PairId) -> Result<(), Error> {
        Ok(self.database.release_automation(id.as_bytes())?)
    }

    pub fn automation_held(&self, id: PairId) -> Result<bool, Error> {
        Ok(self.database.automation_held(id.as_bytes())?)
    }

    /// The pair's journaled runs, if its newest writing run is not undone.
    pub fn journal(&self, id: PairId) -> Result<Option<Journal>, Error> {
        let runs = self
            .database
            .journal(id.as_bytes())?
            .into_iter()
            .map(|run| {
                let entries = run.rows.into_iter().map(entry).collect::<Result<_, _>>()?;
                let identities = run
                    .identities
                    .map(|[left, right]| Ok::<_, Error>([identity(left)?, identity(right)?]))
                    .transpose()?;
                Ok(RunJournal {
                    run: run.run,
                    entries,
                    identities,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        Ok((!runs.is_empty()).then_some(Journal { runs }))
    }
}

fn row(entry: &JournalEntry) -> Result<JournalRow, Error> {
    let (action, version) = match entry.action {
        JournalAction::Create(version) => (CREATE, version),
        JournalAction::Replace(version) => (REPLACE, version),
        JournalAction::Metadata(version) => (METADATA, version),
        JournalAction::Archive(version) => (ARCHIVE, version),
        JournalAction::Move(version) => (MOVE, version),
    };
    Ok(JournalRow {
        seq: i64::try_from(entry.seq).map_err(|_| Error::InvalidStored("journal sequence"))?,
        path: entry.path.as_bytes().to_vec(),
        side: match entry.side {
            Side::Left => LEFT,
            Side::Right => RIGHT,
        }
        .into(),
        action: action.into(),
        version: encode(version).to_vec(),
        base: entry
            .before
            .map(|base| (encode(base.left()).to_vec(), encode(base.right()).to_vec())),
        source: entry
            .moved_from
            .as_ref()
            .map(|path| path.as_bytes().to_vec()),
    })
}

fn entry(row: JournalRow) -> Result<JournalEntry, Error> {
    let invalid = || Error::InvalidStored("journal entry");
    let version = decode(&row.version)?;
    let action = match row.action.as_str() {
        CREATE => JournalAction::Create(version),
        REPLACE => JournalAction::Replace(version),
        METADATA => JournalAction::Metadata(version),
        ARCHIVE => JournalAction::Archive(version),
        MOVE => JournalAction::Move(version),
        _ => return Err(invalid()),
    };
    let moved_from = row
        .source
        .map(|path| SyncPath::try_from(path).map_err(|_| invalid()))
        .transpose()?;
    if matches!(action, JournalAction::Move(_)) != moved_from.is_some() {
        return Err(invalid());
    }
    Ok(JournalEntry {
        seq: u64::try_from(row.seq).map_err(|_| invalid())?,
        path: SyncPath::try_from(row.path).map_err(|_| invalid())?,
        side: match row.side.as_str() {
            LEFT => Side::Left,
            RIGHT => Side::Right,
            _ => return Err(invalid()),
        },
        action,
        before: row
            .base
            .map(|(left, right)| decode_entry(&left, &right))
            .transpose()?,
        moved_from,
    })
}
