use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use std::collections::BTreeMap;
use std::sync::Arc;

use foldover_endpoint::{
    Endpoint, EndpointPair, Filter, InvalidFilter, LocalEndpoint, ObservedEntry, PairRootError,
    PeerLink, Reach, RootRelation, ScanError,
};
use foldover_host::{Environment, EnvironmentHold, LowPowerSyncs, NetworkUse};
use foldover_state::{
    Device, HistoryEntry, Journal, JournalUpdate, MountSources, PairStore, Pause, Share,
    SnapshotEntry, StateLocation, WatchCursor,
};
use foldover_types::{
    DeviceId, Durability, EndpointId, FolderRecords, FolderRole, LocalRoot, MAX_PAIR_NAME_BYTES,
    PairConfig, PairDraft, PairId, PairRoot, PeerRoot, Resolution, SftpRoot, ShareId, Side,
    SyncPath,
};

use crate::{Commit, RunInputs};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    State(foldover_state::Error),
    #[error(transparent)]
    Roots(#[from] PairRootError),
    #[error(transparent)]
    Endpoint(#[from] ScanError),
    #[error("Pair does not exist")]
    PairNotFound,
    #[error("Pair configuration changed; refresh before updating it")]
    StaleRevision,
    #[error(
        "A chosen folder is, holds or sits inside a folder of “{name}”; pairs share folders only as Mirror or Backup sources"
    )]
    CatalogOverlap { pair: PairId, name: String },
    #[error(
        "Can't check the chosen folders against “{name}”, whose folder {folder} can't be read: {source}"
    )]
    CatalogUnavailable {
        pair: PairId,
        name: String,
        folder: String,
        source: PairRootError,
    },
    #[error(
        "Selected root is inside the agent's state directory; a folder may hold it, but never be it or below it"
    )]
    StateOverlap,
    #[error("SFTP folders cannot guarantee full durability; choose standard durability")]
    DurabilityUnsupported,
    #[error(transparent)]
    Filter(#[from] InvalidFilter),
    #[error("That Mac is not paired with this one")]
    DeviceNotPaired,
    #[error("Name the shared folder, as the other Mac will show it")]
    InvalidShareName,
    #[error("The filters leave out the canary file; change them, or name another canary")]
    CanaryExcluded,
    #[error("That folder is inside, or holds, a folder already shared with that Mac")]
    ShareOverlap,
    #[error("The folder's path on the server is too long for Foldover")]
    ServerPathTooLong,
    #[error(
        "Only a Backup's target can go without Foldover's records, and not a folder on another Mac"
    )]
    RecordsRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Storage,
    NotFound,
    Changed,
    Unavailable,
    Overlap,
    Invalid,
}

impl Error {
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::State(_) => ErrorKind::Storage,
            Self::PairNotFound | Self::DeviceNotPaired => ErrorKind::NotFound,
            Self::StaleRevision => ErrorKind::Changed,
            Self::Roots(PairRootError::Overlap(_))
            | Self::CatalogOverlap { .. }
            | Self::StateOverlap
            | Self::ShareOverlap => ErrorKind::Overlap,
            Self::Filter(_)
            | Self::DurabilityUnsupported
            | Self::InvalidShareName
            | Self::CanaryExcluded
            | Self::ServerPathTooLong
            | Self::RecordsRequired => ErrorKind::Invalid,
            Self::Roots(_) | Self::Endpoint(_) | Self::CatalogUnavailable { .. } => {
                ErrorKind::Unavailable
            }
        }
    }
}

impl From<foldover_state::Error> for Error {
    fn from(error: foldover_state::Error) -> Self {
        match error {
            foldover_state::Error::PairNotFound => Self::PairNotFound,
            foldover_state::Error::StaleRevision => Self::StaleRevision,
            error => Self::State(error),
        }
    }
}

/// The agent's single configuration writer. Filesystem checks and a mutation
/// share one serialized command, so concurrent clients cannot reserve the same roots.
pub struct Engine {
    pairs: PairStore,
    state_root: Endpoint,
    /// Where the agent's records live, resolved to its canonical path once
    /// at open; a pair root may hold it, but never read, carry or damage
    /// it. If the directory is moved or renamed while the agent runs,
    /// protection follows the path recorded here, not the directory itself.
    state_path: PathBuf,
    /// How folders on paired Macs are reached, once Mac-to-Mac is on.
    reach: Reach,
}

impl Engine {
    pub fn open(location: &StateLocation) -> Result<Self, Error> {
        let mut pairs = PairStore::open(location)?;
        // No job survives an agent restart. Recover edits saved during
        // the previous process's last job before admitting new work.
        pairs.activate_updates()?;
        let root = pairs.root()?;
        // Resolve once: protection checks below compare this path against
        // canonical paths (watch events, scans, peer requests).
        let state_path = PathBuf::from(OsStr::from_bytes(root.as_bytes()));
        let state_path = state_path.canonicalize().map_err(|source| ScanError::Io {
            path: None,
            source: std::io::Error::new(
                source.kind(),
                format!(
                    "cannot resolve the agent's state directory {}: {source}",
                    state_path.display()
                ),
            ),
        })?;
        let state_root = Endpoint::Local(LocalEndpoint::open(&state_path)?);
        Ok(Self {
            pairs,
            state_root,
            reach: Reach::default().with_state(state_path.clone()),
            state_path,
        })
    }

    pub fn pairs_after(
        &self,
        after: Option<PairId>,
        limit: usize,
    ) -> Result<Vec<PairConfig>, Error> {
        Ok(self.pairs.pairs_after(after, limit)?)
    }

    /// The whole catalog, for agent-internal work such as scheduling.
    pub fn pairs(&self) -> Result<Vec<PairConfig>, Error> {
        Ok(self.pairs.pairs()?)
    }

    pub fn pair(&self, id: PairId) -> Result<PairConfig, Error> {
        self.pairs.pair(id)?.ok_or(Error::PairNotFound)
    }

    /// The settings still held by a current job. Archive reads use the
    /// same roots until that job finishes and activates saved edits.
    pub fn active_pair(&self, id: PairId) -> Result<PairConfig, Error> {
        self.pairs.active_pair(id)?.ok_or(Error::PairNotFound)
    }

    pub fn create_pair(&mut self, draft: &PairDraft) -> Result<PairConfig, Error> {
        validate_options(draft)?;
        self.validate_roots(draft, None)?;
        Ok(self.pairs.create(draft)?)
    }

    pub fn update_pair(
        &mut self,
        id: PairId,
        revision: u64,
        draft: &PairDraft,
    ) -> Result<PairConfig, Error> {
        if self.pair(id)?.revision() != revision {
            return Err(Error::StaleRevision);
        }
        validate_options(draft)?;
        self.validate_roots(draft, Some(id))?;
        Ok(self.pairs.update(id, revision, draft)?)
    }

    /// Save an edit without replacing the configuration an active job
    /// uses for its snapshots, progress and journal commits.
    pub fn defer_pair_update(
        &mut self,
        id: PairId,
        revision: u64,
        draft: &PairDraft,
    ) -> Result<PairConfig, Error> {
        if self.pair(id)?.revision() != revision {
            return Err(Error::StaleRevision);
        }
        validate_options(draft)?;
        self.validate_roots(draft, Some(id))?;
        Ok(self.pairs.defer_update(id, revision, draft)?)
    }

    /// The service calls this under its job-claim boundary, with no
    /// previous job still running for this pair.
    pub fn activate_pair_update(&mut self, id: PairId) -> Result<(), Error> {
        Ok(self.pairs.activate_update(id)?)
    }

    /// Reach folders on paired Macs through `link`.
    pub fn reach_peers(&mut self, link: &Arc<PeerLink>) {
        self.reach = Reach::through(link).with_state(self.state_path.clone());
    }

    /// How folders are reached, for work outside the engine lock.
    pub fn reach(&self) -> Reach {
        self.reach.clone()
    }

    /// Where the agent's records live; endpoints opened through `reach`
    /// protect it, and peer-served folders refuse it.
    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    /// What a run reads from state, for work outside the engine lock.
    pub fn run_inputs(&self, id: PairId) -> Result<RunInputs, Error> {
        // Malformed cached fingerprints or versions only cost a reread.
        let snapshot = |side| -> Result<BTreeMap<_, _>, Error> {
            Ok(self
                .pairs
                .snapshot(id, side)?
                .into_iter()
                .filter_map(|entry| {
                    let observed = ObservedEntry::restore(entry.version, &entry.fingerprint)?;
                    Some((entry.path, observed))
                })
                .collect())
        };
        Ok(RunInputs::new(
            self.active_pair(id)?,
            self.pairs.base(id)?,
            self.pairs.identities(id)?,
            self.pairs.resolutions(id)?,
        )
        .with_snapshots(snapshot(Side::Left)?, snapshot(Side::Right)?)
        .with_reach(self.reach.clone()))
    }

    /// Store a fresh observation of one side for the next run to reuse.
    pub fn store_snapshot(
        &mut self,
        id: PairId,
        revision: u64,
        side: Side,
        entries: &[SnapshotEntry],
    ) -> Result<(), Error> {
        Ok(self.pairs.replace_snapshot(id, revision, side, entries)?)
    }

    /// Store a person's conflict choice for the pair revision they previewed.
    pub fn record_resolution(
        &mut self,
        id: PairId,
        revision: u64,
        path: &SyncPath,
        resolution: Resolution,
    ) -> Result<(), Error> {
        if self.pair(id)?.revision() != revision {
            return Err(Error::StaleRevision);
        }
        Ok(self
            .pairs
            .record_resolution(id, revision, path, resolution)?)
    }

    pub fn release_automation(&mut self, id: PairId) -> Result<(), Error> {
        Ok(self.pairs.release_automation(id)?)
    }

    pub fn automation_held(&self, id: PairId) -> Result<bool, Error> {
        Ok(self.pairs.automation_held(id)?)
    }

    /// The pair's watch cursors (ADR 0002), left then right.
    pub fn watch_cursors(&self, id: PairId) -> Result<[Option<WatchCursor>; 2], Error> {
        Ok([
            self.pairs.watch_cursor(id, Side::Left)?,
            self.pairs.watch_cursor(id, Side::Right)?,
        ])
    }

    pub fn save_watch_cursors(
        &mut self,
        id: PairId,
        cursors: &[(Side, WatchCursor)],
    ) -> Result<(), Error> {
        for (side, cursor) in cursors {
            self.pairs.save_watch_cursor(id, *side, cursor)?;
        }
        Ok(())
    }

    /// Paired Macs (ADR 0031), by name.
    pub fn devices(&self) -> Result<Vec<Device>, Error> {
        Ok(self.pairs.devices()?)
    }

    /// Record a device both people confirmed, as paired now.
    pub fn add_device(&mut self, id: DeviceId, name: String) -> Result<(), Error> {
        Ok(self.pairs.put_device(&Device {
            id,
            name,
            paired: std::time::SystemTime::now(),
        })?)
    }

    /// Forget a device, and end what this Mac shared with it; whether it
    /// was paired.
    pub fn remove_device(&mut self, id: DeviceId) -> Result<bool, Error> {
        Ok(self.pairs.remove_device(id)?)
    }

    /// Folders shared with paired Macs, by name.
    pub fn shares(&self) -> Result<Vec<Share>, Error> {
        Ok(self.pairs.shares()?)
    }

    /// What a paired Mac may sync with.
    pub fn shares_for(&self, device: DeviceId) -> Result<Vec<Share>, Error> {
        Ok(self.pairs.shares_for(device)?)
    }

    /// Share an existing folder with a paired Mac, under the name that Mac
    /// shows. The folder must stay clear of this agent's state.
    pub fn add_share(
        &mut self,
        device: DeviceId,
        root: LocalRoot,
        name: &str,
    ) -> Result<Share, Error> {
        if !self
            .pairs
            .devices()?
            .iter()
            .any(|paired| paired.id == device)
        {
            return Err(Error::DeviceNotPaired);
        }
        let name = name.trim();
        if name.is_empty() || name.contains('\0') || name.len() > MAX_PAIR_NAME_BYTES {
            return Err(Error::InvalidShareName);
        }
        let folder = Endpoint::open(&PairRoot::Local(root.clone()), &self.reach)?;
        // A folder that holds the agent's state directory is fine: it is
        // protected wherever the folder is read or written. The state
        // directory itself, or somewhere inside it, never is.
        if matches!(
            folder.relation_to(&self.state_root)?,
            RootRelation::Same | RootRelation::Within
        ) {
            return Err(Error::StateOverlap);
        }
        // That Mac could otherwise pair two views of the same files.
        for shared in self.pairs.shares_for(device)? {
            let overlaps = match Endpoint::open(&PairRoot::Local(shared.root.clone()), &self.reach)
            {
                Ok(other) => folder.relation_to(&other)? != RootRelation::Disjoint,
                // A folder that is away, as on an unplugged disk, is
                // judged by its path.
                Err(_) => paths_nest(root.as_bytes(), shared.root.as_bytes()),
            };
            if overlaps {
                return Err(Error::ShareOverlap);
            }
        }
        Ok(self
            .pairs
            .add_share(device, &root, name, std::time::SystemTime::now())?)
    }

    /// The folder this Mac shares with `device` as `share`, if it does.
    pub fn shared_folder(
        &self,
        device: DeviceId,
        share: ShareId,
    ) -> Result<Option<LocalRoot>, Error> {
        Ok(self
            .pairs
            .shares_for(device)?
            .into_iter()
            .find(|shared| shared.id == share)
            .map(|shared| shared.root))
    }

    /// Stop sharing; whether the share existed.
    pub fn remove_share(&mut self, id: ShareId) -> Result<bool, Error> {
        Ok(self.pairs.remove_share(id)?)
    }

    /// The pair's newest writing run, while it can still be undone.
    pub fn journal(&self, id: PairId) -> Result<Option<Journal>, Error> {
        Ok(self.pairs.journal(id)?)
    }

    /// Whether the pair has completed a first sync, which records identities.
    pub fn first_synced(&self, id: PairId) -> Result<bool, Error> {
        let identities = self.pairs.identities(id)?;
        Ok(identities.side(Side::Left).is_some() && identities.side(Side::Right).is_some())
    }

    /// Store a run's progress, only while the pair keeps `revision`.
    pub fn commit_run(
        &mut self,
        id: PairId,
        revision: u64,
        commit: Commit<'_>,
    ) -> Result<(), Error> {
        match commit {
            Commit::Identities { left, right } => {
                Ok(self.pairs.record_identities(id, revision, left, right)?)
            }
            Commit::Base {
                run,
                updates,
                journal,
            } => Ok(self.pairs.update_base(
                id,
                revision,
                updates,
                JournalUpdate::Record {
                    run,
                    entries: journal,
                },
            )?),
            Commit::Undoing { updates } => {
                Ok(self
                    .pairs
                    .update_base(id, revision, updates, JournalUpdate::Hold)?)
            }
            Commit::Undid { run, seqs } => {
                Ok(self
                    .pairs
                    .update_base(id, revision, &[], JournalUpdate::Undid { run, seqs })?)
            }
            Commit::Undone { run, updates } => {
                Ok(self
                    .pairs
                    .update_base(id, revision, updates, JournalUpdate::Spend { run })?)
            }
        }
    }

    /// Store a finished run; the pair keeps its newest runs.
    pub fn record_history(&mut self, id: PairId, entry: &HistoryEntry) -> Result<(), Error> {
        Ok(self.pairs.record_history(id, entry)?)
    }

    /// The pair's newest runs first, at most `limit`.
    pub fn history(&self, id: PairId, limit: usize) -> Result<Vec<HistoryEntry>, Error> {
        Ok(self.pairs.history(id, limit)?)
    }

    /// Where each side's network share was last mounted from.
    pub fn mounts(&self, id: PairId) -> Result<MountSources, Error> {
        Ok(self.pairs.mounts(id)?)
    }

    /// Record mount sources a run observed, only while the pair keeps
    /// `revision`.
    pub fn record_mounts(
        &mut self,
        id: PairId,
        revision: u64,
        sources: &MountSources,
    ) -> Result<(), Error> {
        Ok(self.pairs.record_mounts(id, revision, sources)?)
    }

    /// Whether automatic syncs are paused, for every pair.
    pub fn pause(&self) -> Result<Pause, Error> {
        Ok(self.pairs.pause()?)
    }

    /// Record that the agent starts at `now`, and return the starts that
    /// found the run before them stopped uncleanly, oldest first (crash-loop
    /// safe mode, PLAN.md §4.2).
    pub fn note_start(
        &self,
        now: std::time::SystemTime,
    ) -> Result<Vec<std::time::SystemTime>, Error> {
        Ok(self.pairs.note_start(now)?)
    }

    /// The marker `note_start` leaves, to clear at a clean stop without
    /// holding the engine.
    pub fn run_marker(&self) -> foldover_state::RunMarker {
        self.pairs.run_marker()
    }

    pub fn forget_unclean_stops(&self) -> Result<(), Error> {
        Ok(self.pairs.forget_unclean_stops()?)
    }

    pub fn set_pause(&mut self, pause: Pause) -> Result<(), Error> {
        Ok(self.pairs.set_pause(pause)?)
    }

    /// The person's choice for automatic syncs in Low Power Mode.
    pub fn low_power_syncs(&self) -> Result<LowPowerSyncs, Error> {
        Ok(match self.pairs.low_power_syncs()? {
            foldover_state::LowPowerSyncs::Pause => LowPowerSyncs::Pause,
            foldover_state::LowPowerSyncs::Run => LowPowerSyncs::Run,
        })
    }

    pub fn set_low_power_syncs(&mut self, syncs: LowPowerSyncs) -> Result<(), Error> {
        Ok(self.pairs.set_low_power_syncs(match syncs {
            LowPowerSyncs::Pause => foldover_state::LowPowerSyncs::Pause,
            LowPowerSyncs::Run => foldover_state::LowPowerSyncs::Run,
        })?)
    }

    /// Whether the pair's syncs go over the network: an SFTP or paired
    /// Mac root, or a local root Foldover last saw on a mounted share.
    /// An unreadable mount record counts as network: automation fails
    /// closed on costly networks.
    pub fn uses_network(&self, pair: &PairConfig) -> NetworkUse {
        let remote = |root: &PairRoot| !matches!(root, PairRoot::Local(_));
        if remote(pair.draft().left()) || remote(pair.draft().right()) {
            return NetworkUse::Network;
        }
        match self.pairs.mounts(pair.id()) {
            Ok(mounts) if mounts == MountSources::default() => NetworkUse::Local,
            _ => NetworkUse::Network,
        }
    }

    /// Why the pair's automatic syncs wait in `environment`, if they do
    /// (PLAN.md §3.5). Manual syncs never wait.
    pub fn automatic_hold(
        &self,
        pair: &PairConfig,
        environment: Environment,
    ) -> Option<EnvironmentHold> {
        // An unreadable choice is the default: pause in Low Power Mode.
        let low_power = self.low_power_syncs().unwrap_or_default();
        environment.hold(self.uses_network(pair), low_power)
    }

    pub fn remove_pair(&mut self, id: PairId, revision: u64) -> Result<PairConfig, Error> {
        // The caller reserves this pair against active or starting jobs.
        Ok(self.pairs.remove(id, revision)?)
    }

    fn validate_roots(&self, draft: &PairDraft, replacing: Option<PairId>) -> Result<(), Error> {
        let selected = EndpointPair::open(draft.left(), draft.right(), &self.reach)?;
        for side in [Side::Left, Side::Right] {
            // A root may hold the agent's state directory — it is
            // protected wherever the folder is read or written — but it
            // may not be it, or live inside it.
            if matches!(
                selected.endpoint(side).relation_to(&self.state_root)?,
                RootRelation::Same | RootRelation::Within
            ) {
                return Err(Error::StateOverlap);
            }
        }
        let selected = [
            Some(selected.endpoint(Side::Left)),
            Some(selected.endpoint(Side::Right)),
        ];
        // Refusing, the check returns an unreadable folder as an error,
        // so every finding is an overlap.
        match self
            .catalog_overlaps(draft, selected, replacing, Offline::Refuse)?
            .into_iter()
            .next()
        {
            Some(overlap) => Err(Error::CatalogOverlap {
                pair: overlap.pair,
                name: overlap.name,
            }),
            None => Ok(()),
        }
    }

    /// For each side of `draft`, the first configured pair one of whose
    /// folders is, holds or sits inside that side's folder, where the two
    /// pairs may not share it. `selected` holds the draft's opened
    /// folders; a side that did not open is not compared.
    ///
    /// Only Mirror and Backup sources may be shared: runs only read them,
    /// and write one only when a person copies a change back into it,
    /// under its lease (PLAN.md §3.6, ADR 0047). A folder another pair
    /// writes would take two writers' plans, archives and leftovers.
    ///
    /// A configured folder that is missing, say on a disk not mounted, is
    /// compared by where it would be and by its identity file (ADR 0048).
    /// One that is there but cannot be read is refused or reported, as
    /// `offline` says.
    pub(crate) fn catalog_overlaps(
        &self,
        draft: &PairDraft,
        selected: [Option<&Endpoint>; 2],
        replacing: Option<PairId>,
        offline: Offline,
    ) -> Result<Vec<CatalogOverlap>, Error> {
        let mut found: [Option<CatalogOverlap>; 2] = [None, None];
        // Page the catalog to bound memory even when many pairs are configured.
        let mut after = None;
        const CATALOG_BATCH: usize = 32;
        loop {
            let page = self.pairs.pairs_after(after, CATALOG_BATCH)?;
            if page.is_empty() {
                break;
            }
            for pair in page {
                after = Some(pair.id());
                if Some(pair.id()) == replacing {
                    continue;
                }
                // Saved roots reserve their next run, while the active
                // roots remain reserved for a job finishing the old one.
                let active = self
                    .pairs
                    .active_pair(pair.id())?
                    .filter(|active| active.draft() != pair.draft());
                let roots = [Side::Left, Side::Right]
                    .into_iter()
                    .map(|side| (pair.draft(), side))
                    .chain(active.iter().flat_map(|active| {
                        [Side::Left, Side::Right].map(|side| (active.draft(), side))
                    }));
                let name = pair.draft().name();
                for (owner, owner_side) in roots {
                    let sides: Vec<Side> = [Side::Left, Side::Right]
                        .into_iter()
                        .filter(|&side| {
                            found[side as usize].is_none()
                                && selected[side as usize].is_some()
                                && !shareable(draft.role(side), owner.role(owner_side))
                        })
                        .collect();
                    if sides.is_empty() {
                        continue;
                    }
                    let finding = |side, kind| CatalogOverlap {
                        side,
                        pair: pair.id(),
                        name: name.to_owned(),
                        kind,
                    };
                    let unreadable = |path: &LocalRoot, source| Error::CatalogUnavailable {
                        pair: pair.id(),
                        name: name.to_owned(),
                        folder: String::from_utf8_lossy(path.as_bytes()).into_owned(),
                        source: PairRootError::Endpoint(source),
                    };
                    let existing = match root(owner, owner_side) {
                        local @ PairRoot::Local(path) => match Endpoint::open(local, &self.reach) {
                            Ok(endpoint) => Existing::Local(Box::new(endpoint)),
                            Err(ScanError::RootMissing) => Existing::Missing {
                                path,
                                identity: self.pairs.identities(pair.id())?.side(owner_side),
                            },
                            Err(source) => match offline {
                                Offline::Refuse => return Err(unreadable(path, source)),
                                Offline::Report => {
                                    for &side in &sides {
                                        found[side as usize] =
                                            Some(finding(side, OverlapKind::Unverified));
                                    }
                                    continue;
                                }
                            },
                        },
                        PairRoot::Sftp(root) => Existing::Sftp(root),
                        PairRoot::Peer(root) => Existing::Peer(root),
                    };
                    let overlapping = match overlapping(draft, &selected, &sides, &existing) {
                        Ok(overlapping) => overlapping,
                        Err(source) => match (offline, root(owner, owner_side)) {
                            // Only local folders are compared on disk.
                            (Offline::Refuse, PairRoot::Local(path)) => {
                                return Err(unreadable(path, source));
                            }
                            (Offline::Refuse, PairRoot::Sftp(_) | PairRoot::Peer(_)) => {
                                return Err(source.into());
                            }
                            (Offline::Report, _) => {
                                for &side in &sides {
                                    found[side as usize] =
                                        Some(finding(side, OverlapKind::Unverified));
                                }
                                continue;
                            }
                        },
                    };
                    for side in overlapping {
                        found[side as usize] = Some(finding(side, OverlapKind::Overlaps));
                    }
                }
            }
            if found.iter().all(Option::is_some) {
                break;
            }
        }
        Ok(found.into_iter().flatten().collect())
    }
}

/// A configured pair that keeps one side of a draft from being saved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CatalogOverlap {
    pub(crate) side: Side,
    pub(crate) pair: PairId,
    pub(crate) name: String,
    pub(crate) kind: OverlapKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OverlapKind {
    /// One of the pair's folders is, holds or sits inside this side's.
    Overlaps,
    /// One of the pair's folders is there but cannot be read, so this
    /// side cannot be checked against it.
    Unverified,
}

/// What a catalog check does with a configured pair's folder that is
/// there but cannot be read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Offline {
    /// Refuse: saving needs every existing folder verified (ADR 0011).
    Refuse,
    /// Report it as `OverlapKind::Unverified`: a readiness check tells
    /// the person what saving would refuse.
    Report,
}

/// A configured pair's folder: opened when it is local and there, else
/// where it would be.
enum Existing<'a> {
    Local(Box<Endpoint>),
    /// Nothing is at the path, e.g. its disk is not mounted. `identity`
    /// is the one the pair recorded there, if it synced.
    Missing {
        path: &'a LocalRoot,
        identity: Option<EndpointId>,
    },
    Sftp(&'a SftpRoot),
    Peer(&'a PeerRoot),
}

/// Which of `sides` of `draft` are, hold or sit inside `existing`.
/// Local roots are compared on disk; SFTP and paired Macs' roots by
/// locator, so changing the catalog never waits on a server or another
/// Mac.
fn overlapping(
    draft: &PairDraft,
    selected: &[Option<&Endpoint>; 2],
    sides: &[Side],
    existing: &Existing<'_>,
) -> Result<Vec<Side>, ScanError> {
    let mut overlapping = Vec::new();
    for &side in sides {
        let overlaps = match existing {
            Existing::Local(existing) => match selected[side as usize] {
                Some(endpoint) => endpoint.relation_to(existing)? != RootRelation::Disjoint,
                None => false,
            },
            // The folder would be inside this side's, or this side is that
            // folder, carrying its identity, at another path: the disk
            // mounted elsewhere, or the folder moved.
            Existing::Missing { path, identity } => match selected[side as usize] {
                // An identity file that cannot be read cannot clear it.
                Some(endpoint) => {
                    endpoint.relation_to_missing(path)? != RootRelation::Disjoint
                        || match identity {
                            Some(identity) => endpoint.identity()? == Some(*identity),
                            None => false,
                        }
                }
                None => false,
            },
            Existing::Sftp(existing) => {
                matches!(root(draft, side), PairRoot::Sftp(selected) if selected.overlaps(existing))
            }
            Existing::Peer(existing) => {
                matches!(root(draft, side), PairRoot::Peer(selected) if selected == *existing)
            }
        };
        if overlaps {
            overlapping.push(side);
        }
    }
    Ok(overlapping)
}

/// Whether two pairs may both use one folder, each in its role: only as
/// Mirror or Backup sources.
fn shareable(selected: FolderRole, existing: FolderRole) -> bool {
    selected == FolderRole::Source && existing == FolderRole::Source
}

fn root(draft: &PairDraft, side: Side) -> &PairRoot {
    match side {
        Side::Left => draft.left(),
        Side::Right => draft.right(),
    }
}

/// Options the chosen folders cannot keep are refused before any root
/// is opened, so an unreachable server cannot hide the rejection.
fn validate_options(draft: &PairDraft) -> Result<(), Error> {
    // SFTP has no flush for namespace changes, so full durability can
    // never be promised to or from it.
    let remote = |root: &PairRoot| matches!(root, PairRoot::Sftp(_));
    if draft.options().durability() == Durability::Full
        && (remote(draft.left()) || remote(draft.right()))
    {
        return Err(Error::DurabilityUnsupported);
    }
    // PLAN.md §3.6: only a Backup's target may go without records, and
    // another Mac's folder always keeps them.
    if draft.options().records() == FolderRecords::SourceOnly {
        let target = match draft.unrecorded() {
            Some(Side::Left) => draft.left(),
            Some(Side::Right) => draft.right(),
            None => return Err(Error::RecordsRequired),
        };
        if matches!(target, PairRoot::Peer(_)) {
            return Err(Error::RecordsRequired);
        }
    }
    validate_filters(draft)
}

/// Rules are checked when saved, so a run never meets an invalid one,
/// nor one that leaves out the canary. A `.foldoverignore` file can still
/// leave it out; runs report that.
fn validate_filters(draft: &PairDraft) -> Result<(), Error> {
    let filter = Filter::new(draft.options().filters().rules().iter().map(String::as_str))?;
    if draft
        .options()
        .canary()
        .is_some_and(|canary| !filter.keeps(canary))
    {
        return Err(Error::CanaryExcluded);
    }
    Ok(())
}

/// Whether one absolute path is the other or lies below it.
fn paths_nest(left: &[u8], right: &[u8]) -> bool {
    let below = |path: &[u8], parent: &[u8]| {
        path.strip_prefix(parent)
            .is_some_and(|rest| rest.starts_with(b"/") || parent == b"/")
    };
    left == right || below(left, right) || below(right, left)
}

#[cfg(test)]
mod tests {
    use super::*;
    use foldover_host::{NetworkCost, PowerMode};
    use foldover_types::{LocalRoot, SyncMode};
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use crate::ReadinessWarning;

    fn draft(left: &Path, right: &Path) -> PairDraft {
        PairDraft::new("Pair".into(), root(left), root(right), SyncMode::Sync).unwrap()
    }

    fn root(path: &Path) -> LocalRoot {
        LocalRoot::from_bytes(path.as_os_str().as_bytes().to_vec()).unwrap()
    }

    #[test]
    fn catalog_rejects_nested_aliased_and_state_roots_without_mutation() {
        let temporary = tempfile::tempdir().unwrap();
        let mut engine =
            Engine::open(&StateLocation::Directory(temporary.path().join("state"))).unwrap();
        let left = temporary.path().join("left");
        let right = temporary.path().join("right");
        let third = temporary.path().join("third");
        fs::create_dir_all(left.join("child")).unwrap();
        fs::create_dir(&right).unwrap();
        fs::create_dir(&third).unwrap();
        let pair = engine.create_pair(&draft(&left, &right)).unwrap();
        let alias = temporary.path().join("alias");
        symlink(&left, &alias).unwrap();
        for overlap in [&left, &alias, &left.join("child")] {
            assert!(
                matches!(engine.create_pair(&draft(overlap, &third)), Err(Error::CatalogOverlap { pair: id, .. }) if id == pair.id())
            );
        }
        assert!(matches!(
            engine.create_pair(&draft(temporary.path(), &third)),
            Err(Error::Roots(_))
        ));
        let inside_state = temporary.path().join("state/inside");
        fs::create_dir(&inside_state).unwrap();
        for inside in [temporary.path().join("state"), inside_state] {
            assert!(matches!(
                engine.create_pair(&draft(&inside, &third)),
                Err(Error::StateOverlap)
            ));
        }
        assert_eq!(engine.pairs_after(None, 10).unwrap(), vec![pair]);
    }

    /// The agent's records sit under the chosen folder, as under the
    /// home directory: the folder pairs, and the state directory itself
    /// stays out of the sync (endpoint tests prove the protection).
    #[test]
    fn a_root_may_hold_the_state_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let mut engine =
            Engine::open(&StateLocation::Directory(temporary.path().join("state"))).unwrap();
        let right = other.path().join("right");
        fs::create_dir(&right).unwrap();

        let pair = engine
            .create_pair(&draft(temporary.path(), &right))
            .unwrap();
        assert_eq!(engine.pairs_after(None, 10).unwrap(), vec![pair]);

        // A folder so shared is served to a paired Mac the same way.
        let [mac] = [[7; 32]].map(DeviceId::from_bytes);
        engine.add_device(mac, "Mac".into()).unwrap();
        engine
            .add_share(mac, root(temporary.path()), "Home")
            .unwrap();
    }

    /// Review: two shares for one Mac could nest, so that Mac could pair
    /// two views of the same files with each other.
    #[test]
    fn folders_shared_with_one_mac_never_nest() {
        let temporary = tempfile::tempdir().unwrap();
        let mut engine =
            Engine::open(&StateLocation::Directory(temporary.path().join("state"))).unwrap();
        let [studio, mini] = [[1; 32], [2; 32]].map(DeviceId::from_bytes);
        engine.add_device(studio, "Studio".into()).unwrap();
        engine.add_device(mini, "Mini".into()).unwrap();
        let home = temporary.path().join("home");
        let photos = home.join("Photos");
        fs::create_dir_all(photos.join("2024")).unwrap();
        fs::create_dir(home.join("Music")).unwrap();
        engine.add_share(studio, root(&photos), "Photos").unwrap();

        for nested in [photos.join("2024"), home.clone(), photos.clone()] {
            assert!(matches!(
                engine.add_share(studio, root(&nested), "Nested"),
                Err(Error::ShareOverlap)
            ));
        }
        engine
            .add_share(studio, root(&home.join("Music")), "Music")
            .unwrap();
        // Another Mac's shares are its own.
        engine
            .add_share(mini, root(&photos.join("2024")), "2024")
            .unwrap();
    }

    #[test]
    fn updates_are_revision_checked_and_unreadable_catalog_fails_closed() {
        let temporary = tempfile::tempdir().unwrap();
        let location = StateLocation::Directory(temporary.path().join("state"));
        let mut engine = Engine::open(&location).unwrap();
        let paths: Vec<_> = ["a", "b", "c", "d"]
            .map(|name| temporary.path().join(name))
            .into();
        for path in &paths {
            fs::create_dir(path).unwrap();
        }
        let saved = engine.create_pair(&draft(&paths[0], &paths[1])).unwrap();
        let updated = engine
            .update_pair(saved.id(), saved.revision(), saved.draft())
            .unwrap();
        assert_eq!(updated.revision(), saved.revision() + 1);
        assert!(matches!(
            engine.update_pair(saved.id(), saved.revision(), saved.draft()),
            Err(Error::StaleRevision)
        ));
        // A folder that is there but cannot be read cannot be checked.
        fs::set_permissions(&paths[0], fs::Permissions::from_mode(0o000)).unwrap();
        let refused = engine.create_pair(&draft(&paths[2], &paths[3]));
        fs::set_permissions(&paths[0], fs::Permissions::from_mode(0o755)).unwrap();
        let error = refused.unwrap_err();
        assert!(
            matches!(&error, Error::CatalogUnavailable { pair, name, .. }
                if *pair == saved.id() && name == "Pair"),
            "{error:?}"
        );
        let message = error.to_string();
        assert!(message.contains("“Pair”"), "{message}");
        assert!(message.contains(&*paths[0].to_string_lossy()), "{message}");
        drop(engine);
        let engine = Engine::open(&location).unwrap();
        assert_eq!(engine.pair(saved.id()).unwrap(), updated);
    }

    /// ADR 0048: a folder on a disk that is not mounted no longer holds up
    /// every change. It is compared by where it would be, and by the
    /// identity file it carries, should it be mounted elsewhere.
    #[test]
    fn a_missing_folder_is_compared_by_where_it_would_be_and_its_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        let mut engine = Engine::open(&StateLocation::Directory(base.join("state"))).unwrap();
        let [volume, other] = ["volume", "other"].map(|name| base.join(name));
        let [missing, target, free, beside, moved] = [
            volume.join("disk/photos"),
            base.join("target"),
            other.join("free"),
            other.join("beside"),
            other.join("moved"),
        ];
        for path in [&missing, &target, &free, &beside, &moved] {
            fs::create_dir_all(path).unwrap();
        }
        let saved = engine.create_pair(&draft(&missing, &target)).unwrap();
        let reach = Reach::default();
        let identity = |path: &Path| {
            Endpoint::open(&PairRoot::Local(root(path)), &reach)
                .unwrap()
                .ensure_identity(Durability::Standard)
                .unwrap()
        };
        let (left, right) = (identity(&missing), identity(&target));
        engine
            .commit_run(
                saved.id(),
                saved.revision(),
                Commit::Identities { left, right },
            )
            .unwrap();
        // The disk goes away, and the folder moves along with it.
        fs::rename(&missing, moved.join("photos")).unwrap();
        fs::remove_dir(volume.join("disk")).unwrap();

        // Beside where it would be: saved, though it cannot be opened.
        let elsewhere = engine.create_pair(&draft(&free, &beside)).unwrap();
        engine
            .remove_pair(elsewhere.id(), elsewhere.revision())
            .unwrap();
        // Holding where it would be, or being it at another path: refused.
        let carried = moved.join("photos");
        for (refused, holds) in [(&volume, "holds"), (&carried, "is")] {
            let error = engine.create_pair(&draft(&free, refused)).unwrap_err();
            assert!(
                matches!(&error, Error::CatalogOverlap { pair, .. } if *pair == saved.id()),
                "{holds}: {error:?}"
            );
            assert_eq!(
                engine.check_pair(&draft(&free, refused), None).unwrap(),
                [ReadinessWarning::Overlap {
                    side: Side::Right,
                    pair: saved.id(),
                    name: "Pair".into(),
                }],
                "{holds}"
            );
        }

        // Unreadable, the identity file cannot clear the folder.
        let id_file = carried.join(".foldover/id");
        fs::set_permissions(&id_file, fs::Permissions::from_mode(0o000)).unwrap();
        let refused = engine.create_pair(&draft(&free, &carried));
        fs::set_permissions(&id_file, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            matches!(&refused, Err(Error::CatalogUnavailable { pair, .. }) if *pair == saved.id()),
            "{refused:?}"
        );
    }

    /// A missing folder whose nearest existing folder cannot be read
    /// cannot be placed: saving names the pair, and the check reports it.
    #[test]
    fn a_missing_folder_behind_an_unreadable_one_cannot_be_checked() {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        let mut engine = Engine::open(&StateLocation::Directory(base.join("state"))).unwrap();
        let [locked, target, free, other] =
            ["locked", "target", "free", "other"].map(|name| base.join(name));
        let missing = locked.join("disk/photos");
        for path in [&missing, &target, &free, &other] {
            fs::create_dir_all(path).unwrap();
        }
        let saved = engine.create_pair(&draft(&missing, &target)).unwrap();
        fs::remove_dir_all(locked.join("disk")).unwrap();
        // Searchable, so the missing folder is missing, but not listable.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o100)).unwrap();
        let refused = engine.create_pair(&draft(&free, &other));
        let checked = engine.check_pair(&draft(&free, &other), None);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let error = refused.unwrap_err();
        assert!(
            matches!(&error, Error::CatalogUnavailable { pair, .. } if *pair == saved.id()),
            "{error:?}"
        );
        assert!(error.to_string().contains("“Pair”"), "{error}");
        let unverified = |side| ReadinessWarning::Unverified {
            side,
            pair: saved.id(),
            name: "Pair".into(),
        };
        assert_eq!(
            checked.unwrap(),
            [unverified(Side::Left), unverified(Side::Right)]
        );
    }

    fn one_way(left: &Path, right: &Path, mode: SyncMode) -> PairDraft {
        PairDraft::new("Pair".into(), root(left), root(right), mode).unwrap()
    }

    /// One folder backed up to two targets, or a folder and one inside
    /// it backed up separately: only Mirror and Backup sources share.
    #[test]
    fn only_one_way_sources_share_or_nest() {
        let temporary = tempfile::tempdir().unwrap();
        let mut engine =
            Engine::open(&StateLocation::Directory(temporary.path().join("state"))).unwrap();
        let paths = ["source/inner", "a", "b", "c", "d"].map(|name| temporary.path().join(name));
        for path in &paths {
            fs::create_dir_all(path).unwrap();
        }
        let source = temporary.path().join("source");
        let inner = &paths[0];
        let backup = SyncMode::Backup { source: Side::Left };
        let mirror = SyncMode::Mirror {
            source: Side::Right,
        };
        let first = engine
            .create_pair(&one_way(&source, &paths[1], backup))
            .unwrap();
        // The same source, and a folder inside it, as another source.
        engine
            .create_pair(&one_way(&source, &paths[2], backup))
            .unwrap();
        engine
            .create_pair(&one_way(&paths[3], inner, mirror))
            .unwrap();

        // Anything that would write the shared source, or another pair's
        // target, still overlaps; the error names the pair.
        for refused in [
            one_way(&source, &paths[4], SyncMode::Sync),
            one_way(&paths[4], &source, backup),
            one_way(
                &paths[4],
                &paths[1],
                SyncMode::Mirror { source: Side::Left },
            ),
            one_way(&paths[1], &paths[4], backup),
        ] {
            let error = engine.create_pair(&refused).unwrap_err();
            assert!(
                matches!(&error, Error::CatalogOverlap { name, .. } if name == "Pair"),
                "{error:?}"
            );
            assert!(error.to_string().contains("“Pair”"), "{error}");
        }
        // Turning a sharing source into a two-way folder is refused too.
        assert!(matches!(
            engine.update_pair(
                first.id(),
                first.revision(),
                &one_way(&source, &paths[1], SyncMode::Sync)
            ),
            Err(Error::CatalogOverlap { .. })
        ));
    }

    #[test]
    fn pending_edits_reserve_both_sets_of_roots_and_recover_on_restart() {
        let temporary = tempfile::tempdir().unwrap();
        let location = StateLocation::Directory(temporary.path().join("state"));
        let mut engine = Engine::open(&location).unwrap();
        let paths = ["a", "b", "c", "d", "e"].map(|name| temporary.path().join(name));
        for path in &paths {
            fs::create_dir(path).unwrap();
        }
        let original = engine.create_pair(&draft(&paths[0], &paths[1])).unwrap();
        let saved = engine
            .defer_pair_update(original.id(), 1, &draft(&paths[2], &paths[3]))
            .unwrap();
        assert_eq!(engine.pair(original.id()).unwrap(), saved);
        assert_eq!(
            engine.run_inputs(original.id()).unwrap().configuration(),
            &original
        );
        for reserved in [&paths[0], &paths[2]] {
            assert!(
                matches!(engine.create_pair(&draft(reserved, &paths[4])), Err(Error::CatalogOverlap { pair: id, .. }) if id == original.id())
            );
        }
        drop(engine);
        let mut engine = Engine::open(&location).unwrap();
        assert_eq!(
            engine.run_inputs(original.id()).unwrap().configuration(),
            &saved
        );
        // Restart recovered the saved roots, so the old roots are free.
        engine.create_pair(&draft(&paths[0], &paths[4])).unwrap();
    }

    /// A job finishing a two-way configuration still writes both folders,
    /// so its source cannot be shared before the edit to a Backup lands.
    #[test]
    fn a_pending_one_way_edit_shares_nothing_until_it_is_active() {
        let temporary = tempfile::tempdir().unwrap();
        let location = StateLocation::Directory(temporary.path().join("state"));
        let mut engine = Engine::open(&location).unwrap();
        let paths = ["a", "b", "c"].map(|name| temporary.path().join(name));
        for path in &paths {
            fs::create_dir(path).unwrap();
        }
        let backup = SyncMode::Backup { source: Side::Left };
        let original = engine.create_pair(&draft(&paths[0], &paths[1])).unwrap();
        engine
            .defer_pair_update(original.id(), 1, &one_way(&paths[0], &paths[1], backup))
            .unwrap();
        assert!(matches!(
            engine.create_pair(&one_way(&paths[0], &paths[2], backup)),
            Err(Error::CatalogOverlap { pair, .. }) if pair == original.id()
        ));
        drop(engine);
        let mut engine = Engine::open(&location).unwrap();
        engine
            .create_pair(&one_way(&paths[0], &paths[2], backup))
            .unwrap();
    }

    /// Full durability cannot be promised on SFTP: the check precedes
    /// root validation, so an unreachable server cannot hide it.
    #[test]
    fn sftp_pairs_reject_full_durability_before_any_root_check() {
        let temporary = tempfile::tempdir().unwrap();
        let location = StateLocation::Directory(temporary.path().join("state"));
        let mut engine = Engine::open(&location).unwrap();
        let right = temporary.path().join("right");
        fs::create_dir(&right).unwrap();
        let sftp = PairRoot::from_bytes(b"sftp://nas/srv".to_vec()).unwrap();
        let full = foldover_types::PairOptions::new(
            foldover_types::MetadataPolicy::default(),
            foldover_types::Durability::Full,
        );
        for (left, right_root) in [
            (sftp.clone(), PairRoot::from(root(&right))),
            (PairRoot::from(root(&right)), sftp.clone()),
        ] {
            let draft = PairDraft::new("Pair".into(), left, right_root, SyncMode::Sync)
                .unwrap()
                .with_options(full.clone());
            let error = engine.create_pair(&draft).unwrap_err();
            assert!(matches!(error, Error::DurabilityUnsupported), "{error:?}");
            assert_eq!(error.kind(), ErrorKind::Invalid);
        }
        assert!(engine.pairs().unwrap().is_empty());
        drop(engine);

        // A draft with Full over SFTP still decodes from stored bytes:
        // the rejection lives in the save path, not the data model.
        let draft = PairDraft::new("Pair".into(), sftp.clone(), root(&right), SyncMode::Sync)
            .unwrap()
            .with_options(full.clone());
        let stored =
            PairConfig::from_stored(PairId::from_bytes([7; PairId::BYTE_LEN]).unwrap(), 1, draft)
                .unwrap();
        assert_eq!(
            stored.draft().options().durability(),
            foldover_types::Durability::Full
        );

        // An existing full-durability pair stays editable as long as
        // the saved draft remains one the folders can keep.
        let left = temporary.path().join("left");
        fs::create_dir(&left).unwrap();
        let draft = PairDraft::new("Pair".into(), root(&left), root(&right), SyncMode::Sync)
            .unwrap()
            .with_options(full.clone());
        let mut engine = Engine::open(&location).unwrap();
        let pair = engine.create_pair(&draft).unwrap();
        let revision = engine
            .update_pair(pair.id(), pair.revision(), pair.draft())
            .unwrap()
            .revision();
        // The same pair gains an SFTP root: re-saving it is refused.
        let remote = PairDraft::new("Pair".into(), root(&left), sftp, SyncMode::Sync)
            .unwrap()
            .with_options(full);
        assert!(matches!(
            engine.update_pair(pair.id(), revision, &remote),
            Err(Error::DurabilityUnsupported)
        ));
    }

    /// PLAN.md §3.6: only a Backup's target may keep no records, and
    /// never another Mac's folder.
    #[test]
    fn records_stay_in_both_folders_unless_a_backup_target_goes_without() {
        let temporary = tempfile::tempdir().unwrap();
        let location = StateLocation::Directory(temporary.path().join("state"));
        let mut engine = Engine::open(&location).unwrap();
        let (left, right) = (
            temporary.path().join("left"),
            temporary.path().join("right"),
        );
        fs::create_dir(&left).unwrap();
        fs::create_dir(&right).unwrap();
        let source_only =
            foldover_types::PairOptions::default().with_records(FolderRecords::SourceOnly);
        let draft = |right: PairRoot, mode| {
            PairDraft::new("Pair".into(), root(&left), right, mode)
                .unwrap()
                .with_options(source_only.clone())
        };
        let mac = PairRoot::Peer(foldover_types::PeerRoot::new(
            foldover_types::DeviceId::from_bytes([7; foldover_types::DeviceId::BYTE_LEN]),
            foldover_types::ShareId::from_bytes([8; foldover_types::ShareId::BYTE_LEN]),
        ));
        for refused in [
            draft(root(&right).into(), SyncMode::Sync),
            draft(root(&right).into(), SyncMode::Mirror { source: Side::Left }),
            draft(mac, SyncMode::Backup { source: Side::Left }),
        ] {
            let error = engine.create_pair(&refused).unwrap_err();
            assert!(matches!(error, Error::RecordsRequired), "{error:?}");
            assert_eq!(error.kind(), ErrorKind::Invalid);
        }
        let backup = draft(root(&right).into(), SyncMode::Backup { source: Side::Left });
        let pair = engine.create_pair(&backup).unwrap();
        assert_eq!(pair.draft().unrecorded(), Some(Side::Right));
    }

    /// Review of #84: filters that leave out the canary are refused when
    /// saved, before any run reports it missing.
    #[test]
    fn filters_that_leave_out_the_canary_are_refused() {
        let temporary = tempfile::tempdir().unwrap();
        let location = StateLocation::Directory(temporary.path().join("state"));
        let mut engine = Engine::open(&location).unwrap();
        let (left, right) = (
            temporary.path().join("left"),
            temporary.path().join("right"),
        );
        fs::create_dir(&left).unwrap();
        fs::create_dir(&right).unwrap();
        let canary = foldover_types::SyncPath::try_from(b"docs/canary.txt".to_vec()).unwrap();
        let draft = |rules: &[&str]| {
            let filters =
                foldover_types::Filters::new(rules.iter().map(|rule| (*rule).to_owned()).collect())
                    .unwrap();
            let options = foldover_types::PairOptions::default()
                .with_filters(filters)
                .with_canary(Some(canary.clone()))
                .unwrap();
            PairDraft::new("Pair".into(), root(&left), root(&right), SyncMode::Sync)
                .unwrap()
                .with_options(options)
        };
        for rules in [&["docs/"][..], &["*.txt"][..]] {
            let error = engine.create_pair(&draft(rules)).unwrap_err();
            assert!(
                matches!(error, Error::CanaryExcluded),
                "{rules:?}: {error:?}"
            );
            assert_eq!(error.kind(), ErrorKind::Invalid);
        }
        assert!(engine.create_pair(&draft(&["*.tmp"])).is_ok());
    }

    /// PLAN.md §3.5: a pair uses the network when a root is remote or a
    /// local root last synced from a mounted share; only those pairs
    /// hold on a costly network. Low Power Mode holds every pair unless
    /// the person opted in.
    #[test]
    fn automatic_holds_follow_the_network_use_rule() {
        let temporary = tempfile::tempdir().unwrap();
        let mut engine =
            Engine::open(&StateLocation::Directory(temporary.path().join("state"))).unwrap();
        let left = temporary.path().join("left");
        let right = temporary.path().join("right");
        fs::create_dir(&left).unwrap();
        fs::create_dir(&right).unwrap();
        let local = engine.create_pair(&draft(&left, &right)).unwrap();
        // Remote pairs need no catalog row: the roots alone decide.
        let remote = |bytes: Vec<u8>| {
            PairConfig::from_stored(
                PairId::from_bytes([9; PairId::BYTE_LEN]).unwrap(),
                1,
                PairDraft::new(
                    "Remote".into(),
                    PairRoot::from_bytes(bytes).unwrap(),
                    PairRoot::from(root(&right)),
                    SyncMode::Sync,
                )
                .unwrap(),
            )
            .unwrap()
        };
        let costly = Environment {
            power: PowerMode::Normal,
            network: Some(NetworkCost::Expensive),
        };
        let low = Environment {
            power: PowerMode::LowPower,
            network: Some(NetworkCost::Unrestricted),
        };

        assert_eq!(engine.uses_network(&local), NetworkUse::Local);
        assert_eq!(engine.automatic_hold(&local, costly), None);
        for bytes in [
            b"sftp://nas/photos".to_vec(),
            format!("peer://{}/{}", "ab".repeat(32), "cd".repeat(16)).into_bytes(),
        ] {
            let pair = remote(bytes);
            assert_eq!(engine.uses_network(&pair), NetworkUse::Network);
            assert_eq!(
                engine.automatic_hold(&pair, costly),
                Some(EnvironmentHold::ExpensiveNetwork)
            );
        }

        // A share Foldover mounted counts as a network root.
        engine
            .record_mounts(
                local.id(),
                local.revision(),
                &MountSources {
                    left: Some("smb://nas/photos".into()),
                    right: None,
                },
            )
            .unwrap();
        assert_eq!(engine.uses_network(&local), NetworkUse::Network);
        assert_eq!(
            engine.automatic_hold(&local, costly),
            Some(EnvironmentHold::ExpensiveNetwork)
        );

        assert_eq!(
            engine.automatic_hold(&local, low),
            Some(EnvironmentHold::LowPowerMode)
        );
        engine.set_low_power_syncs(LowPowerSyncs::Run).unwrap();
        assert_eq!(engine.low_power_syncs().unwrap(), LowPowerSyncs::Run);
        assert_eq!(engine.automatic_hold(&local, low), None);
        // The network hold is unaffected by the low-power choice.
        assert_eq!(
            engine.automatic_hold(&local, costly),
            Some(EnvironmentHold::ExpensiveNetwork)
        );
    }

    #[test]
    fn offline_removal_checks_revision_and_releases_catalog_roots() {
        let temporary = tempfile::tempdir().unwrap();
        let mut engine =
            Engine::open(&StateLocation::Directory(temporary.path().join("state"))).unwrap();
        let paths = ["a", "b", "c", "d"].map(|name| temporary.path().join(name));
        for path in &paths {
            fs::create_dir(path).unwrap();
        }
        let original = engine.create_pair(&draft(&paths[0], &paths[1])).unwrap();
        let updated = engine
            .update_pair(original.id(), original.revision(), original.draft())
            .unwrap();
        fs::write(paths[0].join("kept"), b"keep this file").unwrap();
        let detached = temporary.path().join("detached");
        fs::rename(&paths[0], &detached).unwrap();
        assert!(matches!(
            engine.remove_pair(original.id(), original.revision()),
            Err(Error::StaleRevision)
        ));
        assert_eq!(engine.pair(updated.id()).unwrap(), updated);
        assert_eq!(
            engine
                .remove_pair(updated.id(), updated.revision())
                .unwrap(),
            updated
        );
        assert!(matches!(
            engine.pair(updated.id()),
            Err(Error::PairNotFound)
        ));
        assert_eq!(fs::read(detached.join("kept")).unwrap(), b"keep this file");
        assert!(paths[1].is_dir());
        assert!(engine.create_pair(&draft(&paths[2], &paths[3])).is_ok());
    }
}
