//! `foldover-endpoint` — domain services layer. The `Endpoint` trait,
//! capabilities, credentials, `.foldover/` management and the `local`,
//! `share`, `sftp` and `peer` backends. Depends on foldover-fs,
//! foldover-watch, foldover-net and foldover-os. See PLAN.md §4.4.

#[cfg(unix)]
mod local;

#[cfg(unix)]
mod endpoint;

#[cfg(unix)]
mod pair;

#[cfg(unix)]
mod sftp;

#[cfg(unix)]
mod watch;

mod share;
pub use share::mount_share;

#[cfg(unix)]
pub use watch::RootWatcher;

#[cfg(unix)]
pub use local::{
    ApplyError, ArchivedItem, ArchivedRun, Expected, Filter, InvalidFilter, LocalEndpoint,
    LocalEntry, Observation, ObservedEntry, RunScope, ScanControl, ScanError, ScanStats,
    archive_name,
};

#[cfg(unix)]
pub use foldover_fs::{
    AccessControl, AttributeRole, DeclaredSupport, EntryMetadata, ExtendedAttribute,
    ExtendedMetadata, FileIdentity, FilesystemFeature, FilesystemFeatures, ForeignMetadata,
    MetadataReadError, NodeKind, RootRelation,
};

#[cfg(unix)]
pub use endpoint::Endpoint;

#[cfg(unix)]
pub use pair::{EndpointPair, PairRootError, access_denied};

#[cfg(unix)]
pub use sftp::SftpEndpoint;
