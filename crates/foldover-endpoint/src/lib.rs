//! `foldover-endpoint` — domain services layer. The `Endpoint` trait,
//! capabilities, credentials, `.foldover/` management and the `local`,
//! `share`, `sftp` and `peer` backends. Depends on foldover-fs,
//! foldover-watch, foldover-net and foldover-os. See PLAN.md §4.4.

#[cfg(unix)]
mod local;

#[cfg(unix)]
mod local_pair;

#[cfg(unix)]
pub use local::{
    ApplyError, Expected, LocalEndpoint, LocalEntry, Observation, ObservedEntry, RunScope,
    ScanControl, ScanError, ScanStats, archive_name,
};

#[cfg(unix)]
pub use foldover_fs::{
    AccessControl, AttributeRole, DeclaredSupport, EntryMetadata, ExtendedAttribute,
    ExtendedMetadata, FileIdentity, FilesystemFeature, FilesystemFeatures, MetadataReadError,
    NodeKind, RootRelation,
};

#[cfg(unix)]
pub use local_pair::{LocalPair, PairRootError};
