//! `foldover-types` — vocabulary. Domain values, enums and errors shared
//! by every crate; data only, so it can't become a catch-all. May be
//! used anywhere and depends on no other Foldover crate. See PLAN.md §4.4.

mod endpoint_id;
mod metadata_policy;
mod options;
mod pair;
mod reconcile;
mod root;
mod sync_path;

pub use pair::{
    InvalidPair, LocalRoot, MAX_LOCAL_ROOT_BYTES, MAX_PAIR_NAME_BYTES, PairConfig, PairDraft,
    PairId,
};

pub use endpoint_id::EndpointId;
pub use metadata_policy::{MetadataPolicy, Propagation};
pub use options::{Durability, Filters, PairOptions, Schedule, ScheduleInterval};
pub use root::{PairRoot, SftpRoot};

pub use reconcile::{
    BaselineEntry, BaselineMismatch, ChangeKind, ConflictKind, Digest, EntryContent, EntryDecision,
    EntryKind, EntryVersion, Resolution, Side, SyncMode,
};
pub use sync_path::{InvalidSyncPath, SyncPath};

/// Cooperative cancellation between bounded scan/read steps.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanControl {
    Continue,
    Cancel,
}
