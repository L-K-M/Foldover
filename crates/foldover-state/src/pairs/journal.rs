//! The newest writing run's operations per pair, so that run can be undone
//! (ADR 0029). Entries are stored with the base updates their writes
//! caused, in one transaction.

use foldover_db::{JournalChange, JournalRow};
use foldover_types::{BaselineEntry, EntryVersion, PairId, Side, SyncPath};

use super::{Error, PairStore, database_revision};
use crate::BaseUpdate;
use crate::base::{decode, decode_entry, encode};

const LEFT: &str = "left";
const RIGHT: &str = "right";
const CREATE: &str = "create";
const REPLACE: &str = "replace";
const METADATA: &str = "metadata";
const ARCHIVE: &str = "archive";

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
}

/// One write a run made, in write order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalEntry {
    pub seq: u64,
    pub path: SyncPath,
    pub side: Side,
    pub action: JournalAction,
    /// The base before the run.
    pub before: Option<BaselineEntry>,
}

/// A run's writes, named by its archive folder, e.g.
/// `2026-10-18T09-30-00.123Z`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Journal {
    pub run: String,
    pub entries: Vec<JournalEntry>,
}

/// How a base update touches the journal.
#[derive(Clone, Copy, Debug)]
pub enum JournalUpdate<'a> {
    None,
    /// Writes of `run`; its first batch replaces the previous run's.
    Record {
        run: &'a str,
        entries: &'a [JournalEntry],
    },
    /// An undo is changing files: hold the pair's automatic syncs, across
    /// restarts, until a person syncs it.
    Hold,
    /// An undo reverted these entries of `run`; a retry skips them.
    Undid {
        run: &'a str,
        seqs: &'a [u64],
    },
    /// `run` was undone; its entries are spent.
    Spend {
        run: &'a str,
    },
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
        let revision = database_revision(revision)?;
        let rows: Vec<JournalRow>;
        let undone: Vec<i64>;
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
            JournalUpdate::Spend { run } => JournalChange::Spend { run },
            JournalUpdate::Record { run, entries } => {
                rows = entries.iter().map(row).collect::<Result<_, _>>()?;
                JournalChange::Record { run, rows: &rows }
            }
        };
        self.change_base(id, revision, updates, change)
    }

    /// A person synced: automatic syncs may run again.
    pub fn release_automation(&mut self, id: PairId) -> Result<(), Error> {
        Ok(self.database.release_automation(id.as_bytes())?)
    }

    pub fn automation_held(&self, id: PairId) -> Result<bool, Error> {
        Ok(self.database.automation_held(id.as_bytes())?)
    }

    /// The pair's journaled run, if its newest writing run is not undone.
    pub fn journal(&self, id: PairId) -> Result<Option<Journal>, Error> {
        let Some((run, rows)) = self.database.journal(id.as_bytes())? else {
            return Ok(None);
        };
        let entries = rows.into_iter().map(entry).collect::<Result<_, _>>()?;
        Ok(Some(Journal { run, entries }))
    }
}

fn row(entry: &JournalEntry) -> Result<JournalRow, Error> {
    let (action, version) = match entry.action {
        JournalAction::Create(version) => (CREATE, version),
        JournalAction::Replace(version) => (REPLACE, version),
        JournalAction::Metadata(version) => (METADATA, version),
        JournalAction::Archive(version) => (ARCHIVE, version),
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
        _ => return Err(invalid()),
    };
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
    })
}
