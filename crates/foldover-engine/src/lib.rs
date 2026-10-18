//! `foldover-engine` — engine layer. Scan, reconcile, plan, gates,
//! execute, recover, archive, conflicts and the scheduler; implements
//! the peer listener's request handler. Depends on foldover-endpoint,
//! foldover-state and foldover-host. See PLAN.md §4.4.

mod configuration;
mod gates;
mod plan;
mod reconcile;
mod run;

#[cfg(test)]
mod plan_properties;

pub use configuration::{Engine, Error, ErrorKind};
pub use foldover_state::StateLocation;
pub use gates::{DeletionCounts, DeletionReview, InvalidDeletionCounts, RunContext};
pub use plan::{CandidateDecision, CandidatePlan, PlanError, PlanRow, TreeRole, plan_candidates};
pub use reconcile::reconcile_entry;
pub use run::{
    Action, Approval, Completion, Failure, PairRun, Preview, PreviewItem, RunError, RunOutcome,
    RunReport,
};
