//! `foldover-engine` — engine layer. Scan, reconcile, plan, gates,
//! execute, recover, archive, conflicts and the scheduler; implements
//! the peer listener's request handler. Depends on foldover-endpoint,
//! foldover-state and foldover-host. See PLAN.md §4.4.

mod reconcile;

pub use reconcile::reconcile_entry;
