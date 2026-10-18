//! `foldover-engine` — engine layer. Scan, reconcile, plan, gates,
//! execute, recover, archive, conflicts and the scheduler; implements
//! the peer listener's request handler. Depends on foldover-endpoint,
//! foldover-state and foldover-host. See PLAN.md §4.4.

mod archive;
mod configuration;
mod gates;
mod plan;
mod readiness;
mod reconcile;
mod run;
mod watch;

#[cfg(test)]
mod plan_properties;

pub use archive::{ArchiveError, ArchiveErrorKind, ArchivedItem, ArchivedRun, PairArchive};
pub use configuration::{Engine, Error, ErrorKind};
pub use foldover_state::RecordedIdentities;
pub use foldover_state::StateLocation;
pub use foldover_state::{HISTORY_LIMIT, HistoryEntry, HistoryOutcome, RunTrigger};
pub use gates::{DeletionCounts, DeletionReview, InvalidDeletionCounts, RunContext};
pub use plan::{
    CandidateDecision, CandidatePlan, PlanError, PlanRow, TreeRole, plan_candidates, plan_resolved,
};
pub use readiness::missing_side;
pub use reconcile::reconcile_entry;
pub use run::{
    Action, Approval, Commit, Completion, Failure, PairRun, Preview, PreviewItem, RunError,
    RunInputs, RunOutcome, RunReport,
};
pub use watch::PairWatcher;
