//! `foldover-types` — vocabulary. Domain values, enums and errors shared
//! by every crate; data only, so it can't become a catch-all. May be
//! used anywhere and depends on no other Foldover crate. See PLAN.md §4.4.

mod pair;
mod reconcile;
mod sync_path;

pub use pair::{InvalidPair, LocalRoot, PairConfig, PairDraft, PairId};

pub use reconcile::{
    BaselineEntry, BaselineMismatch, ChangeKind, ConflictKind, Digest, EntryContent, EntryDecision,
    EntryKind, EntryVersion, Side, SyncMode,
};
pub use sync_path::{InvalidSyncPath, SyncPath};
