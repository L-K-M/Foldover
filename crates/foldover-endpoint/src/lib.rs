//! `foldover-endpoint` — domain services layer. The `Endpoint` trait,
//! capabilities, credentials, `.foldover/` management and the `local`,
//! `share`, `sftp` and `peer` backends. Depends on foldover-fs,
//! foldover-watch, foldover-net and foldover-os. See PLAN.md §4.4.

#[cfg(unix)]
mod local;

#[cfg(unix)]
mod endpoint;

mod peer_identity;

pub use foldover_net::peer::Identity as PeerIdentity;
pub use peer_identity::{PeerIdentityError, load_peer_identity};

#[cfg(unix)]
mod pair;

#[cfg(unix)]
mod observation_reads;

#[cfg(unix)]
mod sftp;

#[cfg(unix)]
mod watch;

mod share;
pub use share::mount_share;

#[cfg(unix)]
pub use watch::{RootWatcher, changed_since_event, event_history, event_now};

#[cfg(unix)]
pub use local::{
    ApplyError, ArchivedItem, ArchivedRun, Expected, Filter, InvalidFilter, LocalEndpoint,
    LocalEntry, Observation, ObservedEntry, PreviousObservation, Pruning, RunScope, ScanControl,
    ScanError, ScanIssue, ScanStats, SnapshotBuilder, archive_name, run_name, run_owner,
};

#[cfg(unix)]
pub use foldover_fs::{
    AccessControl, AttributeRole, DeclaredSupport, EntryMetadata, ExtendedAttribute,
    ExtendedMetadata, FileIdentity, FilesystemFeature, FilesystemFeatures, ForeignMetadata,
    MetadataReadError, NodeKind, RootRelation,
};

#[cfg(unix)]
pub use endpoint::{Bottleneck, Endpoint, ListedEntry, ListedKind, ObserveStep, Reach};

#[cfg(feature = "simulation")]
mod memory;
#[cfg(feature = "simulation")]
pub use memory::{MemoryEndpoint, NameStorage, Quirks, Replace};

pub use foldover_os::{CloudProvider, cloud_provider, current_cloud_provider};

mod link;
pub use foldover_net::discovery::{FileServer, file_servers};
pub use link::{
    Answer, Decision, LinkError, NearbyMac, NotRecorded, PeerHost, PeerLink, PendingPairing,
    Presence, WatchHandle,
};

mod pace;

#[cfg(unix)]
mod observation_versions;

#[cfg(unix)]
pub use observation_versions::ObservedVersion;

#[cfg(unix)]
mod observation_hints;

#[cfg(unix)]
pub use observation_hints::ObservationHints;

mod catalog;
pub use catalog::{CATALOG_FILE_LIMIT, CatalogFiles};

mod lease;
pub use lease::{HeldLease, LeaseError, LeaseHolder, LeaseTiming, LockFile};

mod remote;
pub use remote::{FolderCache, PeerEndpoint, serve_folder, watch_folder};

#[cfg(unix)]
pub use pair::{EndpointPair, PairRootError, access_denied};

#[cfg(unix)]
pub use sftp::SftpEndpoint;

/// The pair tests' runs belong to.
#[cfg(test)]
pub(crate) fn test_pair() -> foldover_types::PairId {
    foldover_types::PairId::from_bytes([0x5a; foldover_types::PairId::BYTE_LEN])
        .expect("a test pair is not zero")
}

#[cfg(unix)]
mod observation_cache;
#[cfg(unix)]
mod observation_entries;
#[cfg(unix)]
pub use observation_entries::{ObservationEntries, ObservationIter};
