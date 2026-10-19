//! `foldover-state` — domain services layer. The typed store: pairs,
//! base, snapshots, journal, runs, conflicts and devices. Depends on
//! foldover-db. See PLAN.md §4.4.

mod base;
mod pairs;

pub use base::BaseUpdate;
pub use foldover_db::StateLocation;
pub use pairs::{
    Error, HISTORY_LIMIT, HistoryEntry, HistoryOutcome, Journal, JournalAction, JournalEntry,
    JournalUpdate, MountSources, PairStore, Pause, RecordedIdentities, RunTrigger, SnapshotEntry,
};
