//! `foldover-engine` — engine layer. Scan, reconcile, plan, gates,
//! execute, recover, archive, conflicts and the scheduler; implements
//! the peer listener's request handler. Depends on foldover-endpoint,
//! foldover-state and foldover-host. See PLAN.md §4.4.

mod archive;
mod browse;
mod catch_up;
mod configuration;
mod conflict;
mod gates;
mod peers;
mod plan;
mod poll;
mod readiness;
mod reconcile;
mod run;
mod watch;

#[cfg(test)]
mod plan_properties;

pub use archive::{
    ArchiveError, ArchiveErrorKind, ArchivedItem, ArchivedRun, PairArchive, PutBackReport,
};
pub use browse::{ServerFolder, browse_server};
pub use catch_up::{changed_while_away, quiet_cursors};
pub use configuration::{Engine, Error, ErrorKind};
pub use conflict::{KeepBothError, keep_both};
pub use foldover_endpoint::ObserveStep;
pub use foldover_endpoint::Reach;
pub use foldover_endpoint::event_now;
pub use foldover_host::{
    Environment, EnvironmentHold, LowPowerSyncs, NetworkCost, NetworkUse, PowerMode, local_time,
};
pub use foldover_state::RecordedIdentities;
pub use foldover_state::StateLocation;
pub use foldover_state::WatchCursor;
pub use foldover_state::{
    Device, HISTORY_LIMIT, HistoryEntry, HistoryOutcome, MountSources, Pause, RunTrigger, Share,
};
pub use gates::{DeletionCounts, DeletionReview, InvalidDeletionCounts, RunContext};
pub use peers::{
    Answer, Decision, FileServer, FolderCache, LinkError, NearbyMac, NotRecorded, PeerHost,
    PeerIdentity, PeerLink, PendingPairing, Presence, computer_name, nearby_file_servers,
    peer_identity, serve_folder,
};
pub use plan::{
    CandidateDecision, CandidatePlan, Choices, PlanError, PlanRow, TreeRole, plan_candidates,
    plan_resolved,
};
pub use poll::{ChangePoll, PollMode, Polled};
pub use readiness::{ReadinessWarning, missing_side, reconnect};
pub use reconcile::reconcile_entry;
pub use run::{
    Action, Approval, Commit, Completion, Failure, NoProgress, PairRun, Preview, PreviewItem,
    RunError, RunInputs, RunOutcome, RunProgress, RunReport, UndoReport,
};
pub use watch::PairWatcher;
