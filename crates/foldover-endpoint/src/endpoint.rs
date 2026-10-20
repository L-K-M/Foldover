//! One side of a pair: a local root or a folder on an SFTP server. The
//! engine works through this type only. Copies between two local roots
//! keep their native path (clones, resource forks, ACLs); every other
//! combination streams data and carries mode and modification time.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, SystemTime};

use foldover_types::{
    Durability, EndpointId, EntryVersion, MetadataPolicy, PairOptions, PairRoot, ScanControl,
    SyncPath,
};

use crate::sftp::SftpEndpoint;
use crate::{
    ApplyError, ArchivedItem, ArchivedRun, Expected, Filter, LocalEndpoint, Observation,
    ObservedEntry, RootRelation, RunScope, ScanError,
};

/// How far an observation has got, e.g. for "Comparing… 12,345 items".
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObserveStep {
    /// An entry was found.
    Listed,
    /// An entry's version is known, read or reused.
    Checked,
}

pub enum Endpoint {
    Local(LocalEndpoint),
    Sftp(SftpEndpoint),
}

impl Endpoint {
    /// Open a root and read its first entry, so refused access shows here.
    pub fn open(root: &PairRoot) -> Result<Self, ScanError> {
        match root {
            PairRoot::Local(root) => {
                let endpoint = LocalEndpoint::open(Path::new(OsStr::from_bytes(root.as_bytes())))?;
                endpoint.probe_access()?;
                Ok(Self::Local(endpoint))
            }
            PairRoot::Sftp(root) => {
                let endpoint = SftpEndpoint::open(root)?;
                endpoint.probe_access()?;
                Ok(Self::Sftp(endpoint))
            }
        }
    }

    pub fn identity(&self) -> Result<Option<EndpointId>, ScanError> {
        match self {
            Self::Local(endpoint) => endpoint.identity(),
            Self::Sftp(endpoint) => endpoint.identity(),
        }
    }

    pub fn ensure_identity(&self, durability: Durability) -> Result<EndpointId, ApplyError> {
        match self {
            Self::Local(endpoint) => endpoint.ensure_identity(durability),
            Self::Sftp(endpoint) => endpoint.ensure_identity(durability),
        }
    }

    pub fn ignore_rules(&self) -> Result<Vec<String>, ScanError> {
        match self {
            Self::Local(endpoint) => endpoint.ignore_rules(),
            Self::Sftp(endpoint) => endpoint.ignore_rules(),
        }
    }

    /// Observe the root, reporting each entry as it is found and again
    /// once its version is known.
    pub fn observe_from(
        &self,
        policy: MetadataPolicy,
        filter: &Filter,
        previous: &BTreeMap<SyncPath, ObservedEntry>,
        control: &mut impl FnMut() -> ScanControl,
        progress: &mut impl FnMut(ObserveStep),
    ) -> Result<Observation, ScanError> {
        match self {
            Self::Local(endpoint) => {
                endpoint.observe_reporting(policy, filter, previous, control, progress)
            }
            Self::Sftp(endpoint) => {
                endpoint.observe_reporting(policy, filter, previous, control, progress)
            }
        }
    }

    pub fn available_space(&self) -> Result<u64, ScanError> {
        match self {
            Self::Local(endpoint) => endpoint.available_space(),
            Self::Sftp(endpoint) => endpoint.available_space(),
        }
    }

    /// Where a local root's network volume is mounted from; SFTP roots
    /// have nothing to mount.
    pub fn network_source(&self) -> Result<Option<String>, ScanError> {
        match self {
            Self::Local(endpoint) => endpoint.network_source(),
            Self::Sftp(_) => Ok(None),
        }
    }

    pub fn recover_leftover(&self, leftover: &SyncPath, run: &RunScope) -> Result<(), ApplyError> {
        match self {
            Self::Local(endpoint) => endpoint.recover_leftover(leftover, run),
            Self::Sftp(endpoint) => endpoint.recover_leftover(leftover, run),
        }
    }

    pub fn remove(
        &self,
        path: &SyncPath,
        current: &ObservedEntry,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<(), ApplyError> {
        match self {
            Self::Local(endpoint) => endpoint.remove(path, current, run, control),
            Self::Sftp(endpoint) => endpoint.remove(path, current, run, control),
        }
    }

    /// Make `path` hold `source`'s entry; anything replaced is archived.
    pub fn copy_from(
        &self,
        source: &Endpoint,
        path: &SyncPath,
        from: &ObservedEntry,
        current: Expected<'_>,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<EntryVersion, ApplyError> {
        match (self, source) {
            (Self::Local(to), Self::Local(from_endpoint)) => {
                to.copy_from(from_endpoint, path, from, current, run, control)
            }
            _ => crate::sftp::copy(self, source, path, from, current, run, control),
        }
    }

    /// Give `path` `source`'s synced metadata, leaving content alone.
    pub fn update_metadata_from(
        &self,
        source: &Endpoint,
        path: &SyncPath,
        from: &ObservedEntry,
        current: &ObservedEntry,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<EntryVersion, ApplyError> {
        match (self, source) {
            (Self::Local(to), Self::Local(from_endpoint)) => {
                to.update_metadata_from(from_endpoint, path, from, current, run, control)
            }
            _ => crate::sftp::update_metadata(self, source, path, from, current, run, control),
        }
    }

    pub fn prune_archive(&self, retention: Duration, now: SystemTime) -> Result<u64, ApplyError> {
        match self {
            Self::Local(endpoint) => endpoint.prune_archive(retention, now),
            Self::Sftp(endpoint) => endpoint.prune_archive(retention, now),
        }
    }

    pub fn archived_runs(&self) -> Result<Vec<ArchivedRun>, ScanError> {
        match self {
            Self::Local(endpoint) => endpoint.archived_runs(),
            Self::Sftp(endpoint) => endpoint.archived_runs(),
        }
    }

    pub fn archived_items(
        &self,
        run: &str,
        after: Option<&SyncPath>,
        limit: usize,
    ) -> Result<(Vec<ArchivedItem>, bool), ScanError> {
        match self {
            Self::Local(endpoint) => endpoint.archived_items(run, after, limit),
            Self::Sftp(endpoint) => endpoint.archived_items(run, after, limit),
        }
    }

    pub fn put_back(
        &self,
        run: &str,
        path: &SyncPath,
        durability: Durability,
    ) -> Result<SyncPath, ApplyError> {
        match self {
            Self::Local(endpoint) => endpoint.put_back(run, path, durability),
            Self::Sftp(endpoint) => endpoint.put_back(run, path),
        }
    }

    pub fn keep_copy(
        &self,
        path: &SyncPath,
        expected: EntryVersion,
        from: &str,
        options: &PairOptions,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<SyncPath, ApplyError> {
        match self {
            Self::Local(endpoint) => endpoint.keep_copy(path, expected, from, options, control),
            Self::Sftp(endpoint) => endpoint.keep_copy(path, expected, from, options, control),
        }
    }

    /// How two roots nest. Roots on different kinds of endpoint, or on
    /// different servers, never overlap.
    pub fn relation_to(&self, other: &Endpoint) -> Result<RootRelation, ScanError> {
        match (self, other) {
            (Self::Local(endpoint), Self::Local(other)) => endpoint.relation_to(other),
            (Self::Sftp(endpoint), Self::Sftp(other)) => Ok(endpoint.relation_to(other)),
            _ => Ok(RootRelation::Disjoint),
        }
    }
}
