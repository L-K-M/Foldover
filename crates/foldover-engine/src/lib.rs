//! `foldover-engine` — engine layer. Scan, reconcile, plan, gates,
//! execute, recover, archive, conflicts and the scheduler; implements
//! the peer listener's request handler. Depends on foldover-endpoint,
//! foldover-state and foldover-host. See PLAN.md §4.4.

mod archive;
mod configuration;
mod conflict;
mod gates;
mod peers;
mod plan;
mod readiness;
mod reconcile;
mod run;
mod watch;

#[cfg(test)]
mod plan_properties;

pub use archive::{
    ArchiveError, ArchiveErrorKind, ArchivedItem, ArchivedRun, PairArchive, PutBackReport,
};
pub use configuration::{Engine, Error, ErrorKind};
pub use conflict::{KeepBothError, keep_both};
pub use foldover_endpoint::ObserveStep;
pub use foldover_state::RecordedIdentities;
pub use foldover_state::StateLocation;
pub use foldover_state::{
    Device, HISTORY_LIMIT, HistoryEntry, HistoryOutcome, MountSources, Pause, RunTrigger,
};
pub use gates::{DeletionCounts, DeletionReview, InvalidDeletionCounts, RunContext};
pub use peers::{
    Decision, LinkError, NearbyMac, PeerIdentity, PeerLink, PendingPairing, Presence,
    computer_name, peer_identity,
};
pub use plan::{
    CandidateDecision, CandidatePlan, Choices, PlanError, PlanRow, TreeRole, plan_candidates,
    plan_resolved,
};
pub use readiness::{missing_side, reconnect};
pub use reconcile::reconcile_entry;
pub use run::{
    Action, Approval, Commit, Completion, Failure, NoProgress, PairRun, Preview, PreviewItem,
    RunError, RunInputs, RunOutcome, RunProgress, RunReport, UndoReport,
};
pub use watch::PairWatcher;
