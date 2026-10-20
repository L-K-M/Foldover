//! `foldover-db` — driver layer. SQLite state storage: schema,
//! migrations and backups. Calls the OS directly; depends on no other
//! Foldover crate. See PLAN.md §4.4.

#[cfg(unix)]
mod database;

#[cfg(unix)]
pub use database::{
    AutomationSettingRow, BaseChange, BaseRow, Database, DeviceRow, Error, HistoryRow, IdentityRow,
    JournalChange, JournalRow, MountRow, PairRow, PairValues, PauseRow, ResolutionRow, ShareRow,
    SnapshotRow, StateLocation,
};
