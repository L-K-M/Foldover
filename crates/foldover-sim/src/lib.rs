//! `foldover-sim` — test support (dev role). In-memory folders with file
//! system quirks and injected faults, for other crates' simulations
//! (ADR 0038); a dev-dependency only. The folders plug into
//! `foldover-endpoint`'s `Endpoint` through its `simulation` feature,
//! which depending on this crate turns on. See PLAN.md §4.4.

pub use foldover_endpoint::{MemoryEndpoint, NameStorage, Quirks, Replace};
