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
pub use endpoint::{Endpoint, ListedEntry, ListedKind, ObserveStep, Reach};

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

mod lease;
pub use lease::{HeldLease, LeaseError, LeaseHolder, LeaseTiming, LockFile};

mod remote;
pub use remote::{FolderCache, PeerEndpoint, serve_folder, watch_folder};

#[cfg(unix)]
pub use pair::{EndpointPair, PairRootError, access_denied};

#[cfg(unix)]
pub use sftp::SftpEndpoint;
