//! One sync run of a local pair. Filesystem work happens outside the engine
//! lock; only base commits go through it, bound to the pair's revision.
//!
//! ```text
//! prepare: keep the Mac awake ─▶ open roots ─▶ observe both sides
//!          ─▶ plan against the base
//! execute: first-run, emptiness and deletion gates ─▶ root identities
//!          ─▶ archive leftovers
//!          ─▶ forward pass (parents first): copies, metadata, base records
//!          ─▶ reverse pass (children first): archives
//!          ─▶ base commits in batches, each checked against the revision
//!             after both roots' identities are read again
//! ```
//!
//! The power assertion lives as long as the run value: from before the
//! scan until the last write.
//!
//! A crash loses at most an uncommitted batch. The next run then finds both
//! sides equal and records them, or a leftover temporary and archives it.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime};

use foldover_endpoint::{
    ApplyError, Endpoint, EndpointPair, Expected, Filter, HeldLease, InvalidFilter, LeaseError,
    LeaseHolder, LeaseTiming, Observation, ObserveStep, ObservedEntry, PairRootError, Reach,
    RunScope, ScanError, access_denied,
};
use foldover_host::RunAwake;
use foldover_state::{
    BaseUpdate, JournalAction, JournalEntry, MountSources, RecordedIdentities, SnapshotEntry,
};
use foldover_types::{
    BaselineEntry, ChangeKind, ConflictKind, Durability, EndpointId, EntryContent, EntryDecision,
    EntryKind, EntryVersion, NameMatching, PairConfig, PairId, PairOptions, Propagation,
    Resolution, ReviewToken, ScanControl, Side, Skip, SyncPath, UnicodeMatching,
};
use unicode_normalization::UnicodeNormalization;

use crate::{
    CandidateDecision, CandidatePlan, Choices, DeletionCounts, DeletionReview, Moves, PlanError,
    PlanRow, RunContext, plan_resolved,
};

/// Base updates per commit: bounds the work a crash can make the next run redo.
const COMMIT_BATCH: usize = 256;
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;
/// Free space a run leaves on each volume, so the system and other writers
/// are not starved by a sync.
const SPACE_RESERVE: u64 = 256 * 1024 * 1024;
/// Owner decision (PLAN.md §10): archived versions are kept for 30 days.
const ARCHIVE_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Roots(#[from] PairRootError),
    #[error("Reading the {side:?} folder failed: {source}")]
    Observe {
        side: Side,
        #[source]
        source: ScanError,
    },
    #[error("{} ({source})", access_denied(*side))]
    AccessDenied {
        side: Side,
        #[source]
        source: ScanError,
    },
    #[error(
        "The {side:?} folder is not the one this pair synced before: its identity file is missing \
         or different. Reconnect the original disk, or change the pair to start over."
    )]
    IdentityChanged { side: Side },
    #[error(
        "The {side:?} folder could not be read during the sync, so it stopped: {source}. \
         Reconnect its disk or share and sync again."
    )]
    RootLost {
        side: Side,
        #[source]
        source: ScanError,
    },
    #[error(
        "Writing the {side:?} folder's identity failed: {source}. A folder that cannot hold \
         Foldover's .foldover folder can only be a Backup target that keeps none."
    )]
    IdentityWrite {
        side: Side,
        #[source]
        source: ApplyError,
    },
    #[error(
        "The {side:?} folder is on an SFTP server, which cannot guarantee full durability; \
         choose standard durability for this pair"
    )]
    UnsupportedDurability { side: Side },
    #[error(
        "The canary file “{}” is missing from the {side:?} folder, so nothing was written. \
         Put it back, or change the pair's canary.",
        String::from_utf8_lossy(path.as_bytes())
    )]
    CanaryMissing { side: Side, path: SyncPath },
    #[error(
        "The canary file “{}” cannot be read in the {side:?} folder, so nothing was written. \
         Use a regular file and download it first if it is in iCloud.",
        String::from_utf8_lossy(path.as_bytes())
    )]
    CanaryUnavailable { side: Side, path: SyncPath },
    #[error(
        "The canary file “{}” changed in the {side:?} folder since the last sync, so nothing was \
         written. Check that the right disk is connected, or change the pair's canary.",
        String::from_utf8_lossy(path.as_bytes())
    )]
    CanaryChanged { side: Side, path: SyncPath },
    #[error(
        "The pair's filters or a .foldoverignore file leave out the canary file “{}”, so nothing \
         was written. Change the filters, or name another canary.",
        String::from_utf8_lossy(path.as_bytes())
    )]
    CanaryExcluded { path: SyncPath },
    #[error(
        "The {side:?} folder's disk has {available} bytes free; this sync needs {needed} plus a \
         256 MiB reserve. Free space and sync again."
    )]
    InsufficientSpace {
        side: Side,
        needed: u64,
        available: u64,
    },
    #[error(transparent)]
    Filter(#[from] InvalidFilter),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error("Saving sync progress failed: {0}")]
    Commit(#[source] crate::Error),
    #[error("Keeping the Mac awake for the sync failed: {0}")]
    KeepAwake(#[source] io::Error),
    #[error("Run cancelled before planning finished")]
    Cancelled,
    #[error("This pair has no sync to undo: its newest sync changed nothing or was undone")]
    NothingToUndo,
    #[error(
        "The {side:?} folder keeps no archive, so this pair's syncs cannot be undone; files \
         replaced there are gone"
    )]
    UndoUnavailable { side: Side },
    #[error("{0}")]
    Lease(#[from] LeaseError),
}

/// This agent among others leasing the same folders; one per process.
fn agent_instance() -> &'static str {
    static INSTANCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    INSTANCE.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

/// Durable progress a run hands to the state store, in order.
#[derive(Clone, Copy, Debug)]
pub enum Commit<'a> {
    /// First run only, before any other write.
    Identities { left: EndpointId, right: EndpointId },
    /// Base updates, with the journal entries of `run`'s writes behind
    /// them (ADR 0029), so the run can be undone.
    Base {
        run: &'a str,
        updates: &'a [BaseUpdate],
        journal: &'a [JournalEntry],
    },
    /// An undo's base restorations for paths it is about to revert; the
    /// pair's automatic syncs are held from then on, so none redoes it.
    Undoing { updates: &'a [BaseUpdate] },
    /// Entries of `run` an undo reverted, so undoing again after a cancel
    /// or crash skips them.
    Undid { run: &'a str, seqs: &'a [u64] },
    /// An undo of `run` finished: the last base restorations, and `run`'s
    /// journal is spent.
    Undone {
        run: &'a str,
        updates: &'a [BaseUpdate],
    },
}

/// Where a run reports how far it got, for the agent to show while it
/// works. Calls come from the run's own thread; implementations keep
/// counters other threads read.
pub trait RunProgress {
    fn observed(&self, side: Side, step: ObserveStep);
    /// The plan is ready: the changes to make and the bytes they copy.
    fn planned(&self, changes: u64, bytes: u64);
    /// One planned change was handled, copying `bytes`.
    fn applied(&self, bytes: u64);
    /// The run waits for another Mac's sync of a folder, shown by its
    /// name, e.g. "Studio"; `None` once it goes on.
    fn waiting(&self, holder: Option<&str>);
}

/// For callers that show no progress.
pub struct NoProgress;

impl RunProgress for NoProgress {
    fn observed(&self, _: Side, _: ObserveStep) {}
    fn planned(&self, _: u64, _: u64) {}
    fn applied(&self, _: u64) {}
    fn waiting(&self, _: Option<&str>) {}
}

/// A run's leases on both folders, renewed as it checks for cancellation.
struct HeldLeases<'a> {
    leases: Vec<HeldLease<'a>>,
    /// The first renewal that failed, e.g. after another Mac took over;
    /// it stops the run.
    failure: RefCell<Option<LeaseError>>,
}

impl HeldLeases<'_> {
    /// `control`'s answer, after renewing the leases that are due.
    fn check(&self, control: &mut impl FnMut() -> ScanControl) -> ScanControl {
        if self.failure.borrow().is_none()
            && let Some(error) = self
                .leases
                .iter()
                .find_map(|lease| lease.renew_if_due().err())
        {
            *self.failure.borrow_mut() = Some(error);
        }
        if self.failure.borrow().is_some() {
            return ScanControl::Cancel;
        }
        control()
    }

    fn failure(&self) -> Option<LeaseError> {
        self.failure.take()
    }
}

/// What the user saw and accepted in a preview.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Approval {
    left_deletions: u64,
    right_deletions: u64,
}

impl Approval {
    pub const fn new(left_deletions: u64, right_deletions: u64) -> Self {
        Self {
            left_deletions,
            right_deletions,
        }
    }

    /// Any accepted deletion count at all; such approvals must bind to a
    /// reviewed plan.
    pub fn approves_anything(self) -> bool {
        self.left_deletions > 0 || self.right_deletions > 0
    }

    fn deletions(self, side: Side) -> u64 {
        match side {
            Side::Left => self.left_deletions,
            Side::Right => self.right_deletions,
        }
    }
}

/// A change a run would make, as shown in a preview.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Copy {
        to: Side,
        change: ChangeKind,
    },
    Archive {
        on: Side,
    },
    Conflict(ConflictKind),
    /// An ancestor's conflict must be resolved first.
    Blocked,
    /// Backup keeps a target-only entry or edit, or a person skipped this
    /// change for one run.
    KeepTarget,
    /// Rename on `on` what the other folder moved here, from
    /// `PreviewItem::moved_from`.
    Move {
        on: Side,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviewItem {
    path: SyncPath,
    action: Action,
    left: Option<EntryVersion>,
    right: Option<EntryVersion>,
    bytes: u64,
    moved_from: Option<SyncPath>,
}

impl PreviewItem {
    pub fn path(&self) -> &SyncPath {
        &self.path
    }

    pub fn action(&self) -> Action {
        self.action
    }

    /// What each side held when previewed; a resolution is bound to these.
    pub fn versions(&self) -> (Option<EntryVersion>, Option<EntryVersion>) {
        (self.left, self.right)
    }

    /// File data the item copies; 0 for everything else.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// For a move, where the entry comes from.
    pub fn moved_from(&self) -> Option<&SyncPath> {
        self.moved_from.as_ref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Preview {
    pair: PairId,
    revision: u64,
    items: Vec<PreviewItem>,
    left_deletions: DeletionCounts,
    right_deletions: DeletionCounts,
    failures: Vec<Failure>,
}

impl Preview {
    /// Entries left unchanged because their scan failed.
    pub fn failures(&self) -> &[Failure] {
        &self.failures
    }

    /// The pair this preview was planned for.
    pub fn pair(&self) -> PairId {
        self.pair
    }

    /// The pair revision this preview was planned for.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Every path with a visible change, conflict or block, in path order.
    pub fn items(&self) -> &[PreviewItem] {
        &self.items
    }

    pub fn deletions(&self, side: Side) -> DeletionCounts {
        match side {
            Side::Left => self.left_deletions,
            Side::Right => self.right_deletions,
        }
    }

    /// A deterministic fingerprint a client presents back to prove it
    /// reviewed this exact plan (ADR 0013): the pair, its revision, the
    /// deletion counts and every item's path, action, both versions and
    /// bytes — including items beyond a response page. The encoding is
    /// length-prefixed, so distinct plans cannot collide structurally.
    pub fn token(&self) -> ReviewToken {
        fn version(hasher: &mut blake3::Hasher, version: Option<EntryVersion>) {
            match version {
                None => {
                    hasher.update(&[0]);
                }
                Some(version) => {
                    hasher.update(&[1]);
                    match version.content() {
                        EntryContent::File(digest) => {
                            hasher.update(&[0]).update(digest.as_bytes());
                        }
                        EntryContent::Directory => {
                            hasher.update(&[1]);
                        }
                        EntryContent::Symlink(digest) => {
                            hasher.update(&[2]).update(digest.as_bytes());
                        }
                    }
                    hasher.update(version.metadata().as_bytes());
                }
            };
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"foldover:sync-preview:v1\0");
        hasher.update(self.pair.as_bytes());
        hasher.update(&self.revision.to_le_bytes());
        for counts in [self.left_deletions, self.right_deletions] {
            hasher.update(&counts.paths().to_le_bytes());
            hasher.update(&counts.deletions().to_le_bytes());
        }
        hasher.update(&(self.items.len() as u64).to_le_bytes());
        for item in &self.items {
            hasher.update(&(item.path.as_bytes().len() as u64).to_le_bytes());
            hasher.update(item.path.as_bytes());
            match item.action {
                Action::Copy { to, change } => {
                    hasher.update(&[0]);
                    hasher.update(&[to as u8]);
                    hasher.update(&[match change {
                        ChangeKind::Create => 0,
                        ChangeKind::ReplaceContent => 1,
                        ChangeKind::UpdateMetadata => 2,
                    }]);
                }
                Action::Archive { on } => {
                    hasher.update(&[1]);
                    hasher.update(&[on as u8]);
                }
                Action::Conflict(kind) => {
                    hasher.update(&[2]);
                    hasher.update(&[match kind {
                        ConflictKind::BothChanged => 0,
                        ConflictKind::DeletedVersusEdited => 1,
                        ConflictKind::TypeMismatch => 2,
                        ConflictKind::MetadataOnly => 3,
                        ConflictKind::NameClash => 4,
                    }]);
                }
                Action::Blocked => {
                    hasher.update(&[3]);
                }
                Action::KeepTarget => {
                    hasher.update(&[4]);
                }
                Action::Move { on } => {
                    hasher.update(&[5]);
                    hasher.update(&[on as u8]);
                }
            }
            if let Some(from) = &item.moved_from {
                hasher.update(&(from.as_bytes().len() as u64).to_le_bytes());
                hasher.update(from.as_bytes());
            }
            version(&mut hasher, item.left);
            version(&mut hasher, item.right);
            hasher.update(&item.bytes.to_le_bytes());
        }
        for failure in &self.failures {
            let path = failure.path.as_ref().map_or(&[][..], SyncPath::as_bytes);
            hasher.update(&(path.len() as u64).to_le_bytes());
            hasher.update(path);
            hasher.update(&(failure.message.len() as u64).to_le_bytes());
            hasher.update(failure.message.as_bytes());
        }
        ReviewToken::from_bytes(*hasher.finalize().as_bytes())
            .expect("a BLAKE3 digest is not all zero")
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Completion {
    #[default]
    Finished,
    Cancelled,
}

/// A change that failed; the run went on with independent paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Failure {
    path: Option<SyncPath>,
    message: String,
}

impl Failure {
    /// `None` identifies the endpoint root rather than a relative entry.
    pub fn path(&self) -> Option<&SyncPath> {
        self.path.as_ref()
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RunReport {
    completion: Completion,
    archive: String,
    pruned: u64,
    copied: u64,
    /// Files and links renamed for moves the other folder made.
    moved: u64,
    updated: u64,
    archived: u64,
    recovered: u64,
    conflicts: Vec<(SyncPath, ConflictKind)>,
    failures: Vec<Failure>,
}

impl RunReport {
    pub fn completion(&self) -> Completion {
        self.completion
    }
    /// This run's folder under `.foldover/archive` on each side.
    pub fn archive(&self) -> &str {
        &self.archive
    }
    /// Archived runs past retention, deleted after this run.
    pub fn pruned(&self) -> u64 {
        self.pruned
    }
    pub fn copied(&self) -> u64 {
        self.copied
    }
    pub fn moved(&self) -> u64 {
        self.moved
    }
    pub fn updated(&self) -> u64 {
        self.updated
    }
    pub fn archived(&self) -> u64 {
        self.archived
    }
    /// Leftover temporaries from interrupted runs, moved to the archive.
    pub fn recovered(&self) -> u64 {
        self.recovered
    }
    pub fn conflicts(&self) -> &[(SyncPath, ConflictKind)] {
        &self.conflicts
    }
    pub fn failures(&self) -> &[Failure] {
        &self.failures
    }
}

#[derive(Debug)]
pub enum RunOutcome {
    Ran(RunReport),
    /// Too many deletions for this context; nothing was written.
    NeedsApproval(Preview),
}

/// Everything a run reads from the state store, taken under the engine lock.
pub struct RunInputs {
    pair: PairConfig,
    base: BTreeMap<SyncPath, BaselineEntry>,
    identities: RecordedIdentities,
    resolutions: BTreeMap<SyncPath, Resolution>,
    skips: BTreeMap<SyncPath, Skip>,
    left_snapshot: BTreeMap<SyncPath, ObservedEntry>,
    right_snapshot: BTreeMap<SyncPath, ObservedEntry>,
    reach: Reach,
    lease_timing: LeaseTiming,
}

impl RunInputs {
    pub fn new(
        pair: PairConfig,
        base: BTreeMap<SyncPath, BaselineEntry>,
        identities: RecordedIdentities,
        resolutions: BTreeMap<SyncPath, Resolution>,
    ) -> Self {
        Self {
            pair,
            base,
            identities,
            resolutions,
            skips: BTreeMap::new(),
            left_snapshot: BTreeMap::new(),
            right_snapshot: BTreeMap::new(),
            reach: Reach::default(),
            lease_timing: LeaseTiming::STANDARD,
        }
    }

    /// How long runs wait for, and keep, folder leases; tests shorten it.
    pub fn with_lease_timing(self, lease_timing: LeaseTiming) -> Self {
        Self {
            lease_timing,
            ..self
        }
    }

    /// How folders on paired Macs are reached.
    pub fn with_reach(self, reach: Reach) -> Self {
        Self { reach, ..self }
    }

    pub fn reach(&self) -> &Reach {
        &self.reach
    }

    /// Changes a person chose to leave alone in this run only.
    pub fn with_skips(self, skips: BTreeMap<SyncPath, Skip>) -> Self {
        Self { skips, ..self }
    }

    /// Previous observations, whose versions are reused for entries with an
    /// unchanged stat. They must come from the same roots and policy.
    pub fn with_snapshots(
        self,
        left: BTreeMap<SyncPath, ObservedEntry>,
        right: BTreeMap<SyncPath, ObservedEntry>,
    ) -> Self {
        Self {
            left_snapshot: left,
            right_snapshot: right,
            ..self
        }
    }
}

/// Both sides observed and planned, ready to preview or execute.
pub struct PairRun {
    pair: PairConfig,
    identities: RecordedIdentities,
    endpoints: EndpointPair,
    left: Observation,
    right: Observation,
    failures: Vec<Failure>,
    uncertain: Vec<SyncPath>,
    /// Observations the filters excluded, kept for the next run's cache.
    left_cache: BTreeMap<SyncPath, ObservedEntry>,
    right_cache: BTreeMap<SyncPath, ObservedEntry>,
    plan: CandidatePlan,
    /// The base the plan was made against, for the journal's undo records.
    base: BTreeMap<SyncPath, BaselineEntry>,
    /// Whether the base remembered any entry the filters still include.
    remembers: Remembers,
    lease_timing: LeaseTiming,
    /// The pair's options as this run applies them: see `run_options`.
    options: PairOptions,
    _awake: RunAwake,
}

/// Whether a run's base held entries, which makes an empty side news.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Remembers {
    Nothing,
    Entries,
}

impl PairRun {
    /// Recorded identities must match the roots' identity files; until the
    /// first run records them, any folder is accepted.
    pub fn prepare(
        inputs: RunInputs,
        control: &mut impl FnMut() -> ScanControl,
        progress: &dyn RunProgress,
    ) -> Result<Self, RunError> {
        let draft = inputs.pair.draft();
        let endpoints = EndpointPair::open(draft.left(), draft.right(), &inputs.reach)?;
        Self::prepare_on(inputs, endpoints, control, progress)
    }

    /// `prepare` on endpoints opened elsewhere.
    pub(crate) fn prepare_on(
        inputs: RunInputs,
        endpoints: EndpointPair,
        control: &mut impl FnMut() -> ScanControl,
        progress: &dyn RunProgress,
    ) -> Result<Self, RunError> {
        let RunInputs {
            pair,
            base,
            identities,
            resolutions,
            skips,
            left_snapshot,
            right_snapshot,
            reach: _,
            lease_timing,
        } = inputs;
        let draft = pair.draft();
        // PLAN.md §3.5: idle sleep would stop a long scan or copy midway.
        let awake = foldover_host::keep_awake(draft.name()).map_err(RunError::KeepAwake)?;
        for side in [Side::Left, Side::Right] {
            // A folder without records has an identity only this agent knows.
            if draft.unrecorded() == Some(side) {
                continue;
            }
            let Some(recorded) = identities.side(side) else {
                continue;
            };
            let observed = endpoints
                .endpoint(side)
                .identity()
                .map_err(|source| observe_error(side, source))?;
            if observed != Some(recorded) {
                return Err(RunError::IdentityChanged { side });
            }
        }
        let options = run_options(draft.options(), &endpoints)?;
        let policy = options.metadata();
        // One filter for both sides, so an exclusion never looks like a deletion.
        let mut rules: Vec<String> = draft.options().filters().rules().to_vec();
        for side in [Side::Left, Side::Right] {
            rules.extend(
                endpoints
                    .endpoint(side)
                    .ignore_rules()
                    .map_err(|source| observe_error(side, source))?,
            );
        }
        let filters = draft.options().filters();
        let cutoff = filters.age_limit().and_then(|days| {
            SystemTime::now().checked_sub(Duration::from_secs(u64::from(days) * SECONDS_PER_DAY))
        });
        let filter = Filter::new(rules.iter().map(String::as_str))?
            .with_size_limit(filters.size_limit())
            .with_cutoff(cutoff);
        // A canary the rules leave out would look missing in every run.
        let canary = draft.options().canary();
        if let Some(canary) = canary.filter(|canary| !filter.keeps(canary)) {
            return Err(RunError::CanaryExcluded {
                path: canary.clone(),
            });
        }
        let observe = |side: Side, control: &mut dyn FnMut() -> ScanControl| {
            let previous = match side {
                Side::Left => &left_snapshot,
                Side::Right => &right_snapshot,
            };
            endpoints
                .endpoint(side)
                .observe_from(policy, &filter, previous, &mut || control(), &mut |step| {
                    progress.observed(side, step)
                })
                .map_err(|source| match source {
                    ScanError::Cancelled => RunError::Cancelled,
                    source => observe_error(side, source),
                })
        };
        let mut left = observe(Side::Left, control)?;
        let mut right = observe(Side::Right, control)?;
        let left_names = endpoints.endpoint(Side::Left).name_matching();
        let right_names = endpoints.endpoint(Side::Right).name_matching();
        align_spellings(&mut left, &base, left_names);
        align_spellings(&mut right, &base, right_names);
        let mut failures = Vec::new();
        for (side, observation) in [(Side::Left, &left), (Side::Right, &right)] {
            for issue in observation.issues() {
                failures.push(Failure {
                    path: Some(issue.path().clone()),
                    message: format!(
                        "Folder {}: {}. Left unchanged; will retry next sync.",
                        match side {
                            Side::Left => "A",
                            Side::Right => "B",
                        },
                        issue.message()
                    ),
                });
            }
            for directory in observation.unstable_directories() {
                failures.push(Failure {
                    path: directory.clone(),
                    message: format!(
                        "Folder {}: Contents changed while scanning. Readable files will sync; \
                         entries whose absence could not be verified stay unchanged until the next sync.",
                        match side {
                            Side::Left => "A",
                            Side::Right => "B",
                        },
                    ),
                });
            }
        }
        failures
            .sort_by(|left, right| (&left.path, &left.message).cmp(&(&right.path, &right.message)));
        let unavailable: Vec<_> = unavailable(&left, &right, &base, left_names.either(right_names))
            .cloned()
            .collect();
        let uncertain = uncertain_absences(&left, &right, &base, left_names.either(right_names));
        // Size and age differ between folders. A file too large in either
        // folder, or old or absent in both, is skipped in both, and hidden
        // from the base too: its base row stays, so a later edit syncs as
        // an edit rather than looking new on both sides.
        let skipped: BTreeSet<SyncPath> = {
            let left_old: BTreeSet<&SyncPath> = left.old().iter().collect();
            let right_old: BTreeSet<&SyncPath> = right.old().iter().collect();
            let old_or_absent = |old: &BTreeSet<&SyncPath>, seen: &Observation, path| {
                old.contains(path) || !seen.entries().contains_key(*path)
            };
            // The canary proves the folders however old or large it is.
            left_old
                .iter()
                .chain(&right_old)
                .filter(|path| {
                    old_or_absent(&left_old, &left, path) && old_or_absent(&right_old, &right, path)
                })
                .copied()
                .chain(left.oversized().iter().chain(right.oversized()))
                .filter(|path| Some(*path) != canary)
                .chain(&unavailable)
                .cloned()
                .collect()
        };
        let skipped: Vec<SyncPath> = skipped.into_iter().collect();
        // Excluded entries leave the plan, but unchanged ones should not be
        // reread every run: their observations stay cached for the next one.
        let cache = |observation: &Observation| {
            skipped
                .iter()
                .filter_map(|path| observation.entries().get_key_value(path))
                .map(|(path, entry)| (path.clone(), entry.clone()))
                .collect()
        };
        let left_cache = cache(&left);
        let right_cache = cache(&right);
        left.exclude(&skipped);
        right.exclude(&skipped);
        let mut base = base;
        for path in &skipped {
            base.remove(path);
        }
        let choices = Choices {
            resolutions: &resolutions,
            skips: &skips,
            left_names,
            right_names,
            left_moves: moves(endpoints.endpoint(Side::Left)),
            right_moves: moves(endpoints.endpoint(Side::Right)),
        };
        let mut plan = plan_resolved(
            draft.mode(),
            &base,
            &left.versions(),
            &right.versions(),
            &choices,
            control,
        )
        .map_err(|error| match error {
            PlanError::Cancelled => RunError::Cancelled,
            error => RunError::Plan(error),
        })?;
        let protected: Vec<_> = unavailable.iter().chain(&uncertain).cloned().collect();
        plan.protect_unavailable(&protected, left_names.either(right_names))?;
        plan.protect_uncertain_absences(&uncertain, left_names.either(right_names))?;
        Ok(Self {
            pair,
            identities,
            endpoints,
            left,
            right,
            failures,
            uncertain,
            left_cache,
            right_cache,
            plan,
            remembers: if base.is_empty() {
                Remembers::Nothing
            } else {
                Remembers::Entries
            },
            base,
            lease_timing,
            options,
            _awake: awake,
        })
    }

    pub fn revision(&self) -> u64 {
        self.pair.revision()
    }

    /// Where each side's network share is mounted from, to mount it again
    /// later. Informational: a side that cannot tell reports none.
    pub fn mount_sources(&self) -> MountSources {
        let source = |side| {
            self.endpoints
                .endpoint(side)
                .network_source()
                .ok()
                .flatten()
        };
        MountSources {
            left: source(Side::Left),
            right: source(Side::Right),
        }
    }

    /// This run's observation of one side, for the next run to reuse,
    /// filtered entries included: skipping them in the plan must not make
    /// the next run reread unchanged data.
    pub fn snapshot(&self, side: Side) -> Vec<SnapshotEntry> {
        let cache = match side {
            Side::Left => &self.left_cache,
            Side::Right => &self.right_cache,
        };
        let entries: BTreeMap<_, _> = cache
            .iter()
            .chain(self.observation(side).entries())
            .collect();
        entries
            .into_iter()
            .map(|(path, entry)| SnapshotEntry {
                path: path.clone(),
                fingerprint: entry.fingerprint_bytes(),
                version: entry.version(),
            })
            .collect()
    }

    pub fn preview(&self) -> Preview {
        let items = self
            .plan
            .rows()
            .iter()
            .filter_map(|row| {
                let action = match row.decision() {
                    CandidateDecision::BlockedByAncestor(_) => Action::Blocked,
                    CandidateDecision::Entry(decision) => match *decision {
                        EntryDecision::Unchanged
                        | EntryDecision::RecordBase
                        | EntryDecision::ForgetBase => return None,
                        EntryDecision::KeepTarget => Action::KeepTarget,
                        EntryDecision::Propagate { from, change } => Action::Copy {
                            to: from.other(),
                            change,
                        },
                        EntryDecision::Archive { on } => Action::Archive { on },
                        EntryDecision::Conflict(kind) => Action::Conflict(kind),
                        EntryDecision::Move { on } => Action::Move { on },
                        // Shown with the path it moves to.
                        EntryDecision::MovedAway { .. } => return None,
                    },
                };
                let version = |side: Side| {
                    self.observation(side)
                        .entries()
                        .get(row.path())
                        .map(ObservedEntry::version)
                };
                Some(PreviewItem {
                    path: row.path().clone(),
                    action,
                    left: version(Side::Left),
                    right: version(Side::Right),
                    bytes: self.copied_bytes(row).map_or(0, |(_, bytes)| bytes),
                    moved_from: self.plan.moved_from(row.path()).cloned(),
                })
            })
            .collect();
        Preview {
            pair: self.pair.id(),
            revision: self.pair.revision(),
            items,
            left_deletions: self.plan.deletions(Side::Left),
            right_deletions: self.plan.deletions(Side::Right),
            failures: self.failures.clone(),
        }
    }

    /// Execute the plan. `commit` stores progress for the planned pair
    /// revision and fails if the pair changed; the run then stops.
    pub fn execute(
        self,
        approval: Approval,
        context: RunContext,
        started: SystemTime,
        commit: &mut impl FnMut(u64, Commit<'_>) -> Result<(), crate::Error>,
        control: &mut impl FnMut() -> ScanControl,
        progress: &dyn RunProgress,
    ) -> Result<RunOutcome, RunError> {
        let options = &self.options;
        // Full durability is a promise a generic SFTP server cannot
        // keep; refuse before anything — identities included — mutates
        // either folder.
        for side in [Side::Left, Side::Right] {
            if options.durability() == Durability::Full
                && matches!(self.endpoints.endpoint(side), Endpoint::Sftp(_))
            {
                return Err(RunError::UnsupportedDurability { side });
            }
        }

        // PLAN.md §3.6: the canary proves the right folders are mounted;
        // a missing or changed one means a different folder, so the run
        // refuses before any gate, identity or lease touches the folders.
        if let Some(canary) = options.canary() {
            for side in [Side::Left, Side::Right] {
                let observed = self
                    .observation(side)
                    .entries()
                    .get(canary)
                    .map(ObservedEntry::version)
                    .filter(|version| version.kind() == EntryKind::File);
                let Some(observed) = observed else {
                    // Special or cloud-only entries cannot prove content.
                    let unavailable = self
                        .observation(side)
                        .unavailable()
                        .iter()
                        .any(|root| root == canary || root.is_ancestor_of(canary));
                    let path = canary.clone();
                    return Err(match unavailable {
                        true => RunError::CanaryUnavailable { side, path },
                        false => RunError::CanaryMissing { side, path },
                    });
                };
                let recorded = self.base.get(canary).map(|entry| match side {
                    Side::Left => entry.left(),
                    Side::Right => entry.right(),
                });
                if recorded.is_some_and(|version| observed.content() != version.content()) {
                    return Err(RunError::CanaryChanged {
                        side,
                        path: canary.clone(),
                    });
                }
            }
        }

        // PLAN.md §3.2, §3.6: the first run of every pair is previewed,
        // which also covers a non-empty target's confirmation. Only the
        // exact plan a person saw (`RunContext::Reviewed`) records the
        // identities and writes; anything else returns the plan for review.
        let first_run = self.identities.side(Side::Left).is_none()
            || self.identities.side(Side::Right).is_none();
        if first_run && context != RunContext::Reviewed {
            return Ok(RunOutcome::NeedsApproval(self.preview()));
        }
        // PLAN.md §3.6: a folder that is suddenly empty — wiped, or a stale
        // copy put in its place — must not empty the other side unseen,
        // even below the mass-deletion fuse.
        if self.emptied_side_archives() && context != RunContext::Reviewed {
            return Ok(RunOutcome::NeedsApproval(self.preview()));
        }
        for side in [Side::Left, Side::Right] {
            let approved = self.plan.deletions(side).deletions() <= approval.deletions(side);
            let allowed = match self.plan.deletion_review(side, context) {
                DeletionReview::WithinLimits => true,
                DeletionReview::ConfirmationRequired => approved,
                DeletionReview::StopUnattended => false,
            };
            if !allowed {
                return Ok(RunOutcome::NeedsApproval(self.preview()));
            }
        }

        // Replaced versions stay in the archive on the same volume, so a
        // copy needs its full size whether it creates or replaces.
        for side in [Side::Left, Side::Right] {
            let needed = self.bytes_to_copy(side);
            if needed == 0 {
                continue;
            }
            let available = self
                .endpoints
                .endpoint(side)
                .available_space()
                .map_err(|source| observe_error(side, source))?;
            require_space(side, needed, available)?;
        }

        let revision = self.pair.revision();
        // PLAN.md §3.6: a Backup target without records gets an identity
        // only this agent knows, and nothing of it is read or written there.
        let unrecorded = self.pair.draft().unrecorded();
        let identities = match (
            self.identities.side(Side::Left),
            self.identities.side(Side::Right),
        ) {
            // The scan may have taken hours; both roots must still be the
            // recorded ones before the first write.
            (Some(left), Some(right)) => {
                let identities = [left, right];
                self.check_roots(identities)?;
                identities
            }
            _ => {
                // A side whose identity is recorded must still hold it,
                // as when only the other root changed; only an unrecorded
                // side gets one written or adopted.
                let ensure = |side: Side| {
                    let endpoint = self.endpoints.endpoint(side);
                    let recorded = self.identities.side(side);
                    if unrecorded == Some(side) {
                        return Ok(recorded.unwrap_or_else(|| {
                            EndpointId::from_bytes(*uuid::Uuid::new_v4().as_bytes())
                        }));
                    }
                    let Some(recorded) = recorded else {
                        return endpoint
                            .ensure_identity(options.durability())
                            .map_err(|source| RunError::IdentityWrite { side, source });
                    };
                    let observed = endpoint
                        .identity()
                        .map_err(|source| RunError::RootLost { side, source })?;
                    if observed != Some(recorded) {
                        return Err(RunError::IdentityChanged { side });
                    }
                    Ok(recorded)
                };
                let (left, right) = (ensure(Side::Left)?, ensure(Side::Right)?);
                commit(revision, Commit::Identities { left, right }).map_err(RunError::Commit)?;
                [left, right]
            }
        };

        let scope = RunScope::new(started, options.clone());
        let name = scope.name().to_owned();
        // A folder without records keeps no archive: what the run replaces
        // there is deleted.
        let scope_on = |side| match unrecorded == Some(side) {
            true => scope.clone().without_archive(),
            false => scope.clone(),
        };
        let scopes = [scope_on(Side::Left), scope_on(Side::Right)];
        // PLAN.md §4: a run writes a folder only while it holds the
        // folder's lease, so two Macs never write one at once.
        let leases = self.take_leases(identities, &name, control, progress)?;
        // A stop the run's thread hears reaches the copy workers too.
        let stop = AtomicBool::new(false);
        let mut control = || {
            let answer = leases.check(control);
            if answer == ScanControl::Cancel {
                stop.store(true, Ordering::Relaxed);
            }
            answer
        };
        let control = &mut control;
        let mut executor = Executor {
            run: &self,
            report: RunReport {
                archive: name.clone(),
                failures: self.failures.clone(),
                ..RunReport::default()
            },
            scopes: &scopes,
            pending: Vec::new(),
            journal: Vec::new(),
            written: 0,
            failed: Vec::new(),
        };
        let mut commit = |executor: &mut Executor<'_>| {
            let updates = Commit::Base {
                run: &name,
                updates: &executor.pending,
                journal: &executor.journal,
            };
            commit(revision, updates).map_err(RunError::Commit)?;
            executor.pending.clear();
            executor.journal.clear();
            Ok::<_, RunError>(())
        };

        for side in [Side::Left, Side::Right] {
            let observation = self.observation(side);
            for leftover in observation.leftovers() {
                match self
                    .endpoints
                    .endpoint(side)
                    .recover_leftover(leftover, executor.scope(side))
                {
                    Ok(()) => executor.report.recovered += 1,
                    Err(error) => executor.fail(leftover, &error),
                }
            }
        }

        let rows = self.plan.rows();
        let changes = rows.iter().filter(|row| writes(row.decision())).count();
        progress.planned(
            changes as u64,
            self.bytes_to_copy(Side::Left)
                .saturating_add(self.bytes_to_copy(Side::Right)),
        );
        let forward = rows.iter().filter(|row| !is_archive(row.decision()));
        let reverse = rows.iter().rev().filter(|row| is_archive(row.decision()));
        let writer = executor.writer();
        let stopped = thread::scope(|threads| {
            let mut copies = Copies::start(threads, writer, self.workers(), &stop);
            let mut stopped = false;
            for (index, row) in forward.chain(reverse).enumerate() {
                if control() == ScanControl::Cancel {
                    stopped = true;
                    break;
                }
                // PLAN.md §4.7: writes go through handles held on the roots,
                // so an unmounted disk fails rather than taking writes at its
                // old path. Reading both identities every batch stops the run
                // at the first sign of that instead of failing entry by entry.
                if index > 0
                    && index % COMMIT_BATCH == 0
                    && let Err(error) = self.check_roots(identities)
                {
                    copies.finish(&mut executor, control, progress);
                    commit(&mut executor)?;
                    return Err(error);
                }
                // Archives go children first, once every copy has landed.
                if is_archive(row.decision()) {
                    copies.finish(&mut executor, control, progress);
                }
                if copies.takes(&executor, row) {
                    copies.hand(row, &mut executor, control, progress);
                } else {
                    executor.apply(row.path(), row.decision(), control);
                    if writes(row.decision()) {
                        progress.applied(executor.copied_bytes(row));
                    }
                }
                copies.collect(&mut executor, progress);
                if executor.pending.len().max(executor.journal.len()) >= COMMIT_BATCH
                    && let Err(error) = commit(&mut executor)
                {
                    // Landed copies must be journaled before this run
                    // can fail, as the root check above does. The
                    // retry is best-effort: the run fails either way,
                    // so the first failure is what to report.
                    copies.finish(&mut executor, control, progress);
                    let _ = commit(&mut executor);
                    return Err(error);
                }
            }
            copies.finish(&mut executor, control, progress);
            // A copy a stop kept from starting leaves the plan undone too.
            Ok::<_, RunError>(stopped || copies.skipped())
        })?;
        if stopped {
            executor.report.completion = Completion::Cancelled;
        }
        commit(&mut executor)?;
        if let Some(error) = leases.failure() {
            return Err(error.into());
        }

        // Keep retention bounded even when a path fails every run.
        if executor.report.completion == Completion::Finished {
            for side in [Side::Left, Side::Right] {
                if unrecorded == Some(side) {
                    continue;
                }
                let endpoint = self.endpoints.endpoint(side);
                match endpoint.prune_archive(ARCHIVE_RETENTION, started) {
                    Ok(pruned) => executor.report.pruned += pruned,
                    Err(error) => executor.fail_message(
                        &SyncPath::try_from(b".foldover/archive".as_slice())
                            .expect("the archive path is valid"),
                        &error.to_string(),
                    ),
                }
            }
        }
        Ok(RunOutcome::Ran(executor.report))
    }

    /// Both roots' leases for the run named `run`, taken in identity order
    /// so two Macs syncing the same two folders cannot each hold one.
    fn take_leases(
        &self,
        identities: [EndpointId; 2],
        run: &str,
        control: &mut impl FnMut() -> ScanControl,
        progress: &dyn RunProgress,
    ) -> Result<HeldLeases<'_>, RunError> {
        let mut sides = [(identities[0], Side::Left), (identities[1], Side::Right)];
        sides.sort_by_key(|(identity, _)| *identity.as_bytes());
        let holder = LeaseHolder {
            id: format!("{}/{run}", agent_instance()),
            label: foldover_host::computer_name(),
        };
        let waited = Cell::new(false);
        let mut waiting = |label: &str| {
            waited.set(true);
            progress.waiting(Some(label));
        };
        let mut leases = Vec::new();
        for (_, side) in sides {
            // A folder without records holds no lease: another Mac could
            // write it meanwhile (PLAN.md §3.6).
            if self.pair.draft().unrecorded() == Some(side) {
                continue;
            }
            let endpoint = self.endpoints.endpoint(side);
            leases.push(HeldLease::take(
                endpoint,
                holder.clone(),
                self.lease_timing,
                control,
                &mut waiting,
            )?);
        }
        if waited.get() {
            progress.waiting(None);
        }
        Ok(HeldLeases {
            leases,
            failure: RefCell::new(None),
        })
    }

    /// Both roots still hold `identities`, in left, right order. A folder
    /// without records holds none: reading its free space through the
    /// held root still finds it gone.
    fn check_roots(&self, identities: [EndpointId; 2]) -> Result<(), RunError> {
        for (side, identity) in [Side::Left, Side::Right].into_iter().zip(identities) {
            if self.pair.draft().unrecorded() == Some(side) {
                self.endpoints
                    .endpoint(side)
                    .available_space()
                    .map_err(|source| RunError::RootLost { side, source })?;
                continue;
            }
            let observed = self
                .endpoints
                .endpoint(side)
                .identity()
                .map_err(|source| RunError::RootLost { side, source })?;
            if observed != Some(identity) {
                return Err(RunError::IdentityChanged { side });
            }
        }
        Ok(())
    }

    /// One side holds nothing although the base remembers entries, and the
    /// plan archives on the other side because of it.
    fn emptied_side_archives(&self) -> bool {
        if self.remembers == Remembers::Nothing {
            return false;
        }
        [Side::Left, Side::Right].into_iter().any(|side| {
            self.observation(side).entries().is_empty() && self.plan.rows().iter().any(|row| {
                matches!(
                    row.decision(),
                    CandidateDecision::Entry(EntryDecision::Archive { on }) if *on == side.other()
                )
            })
        })
    }

    /// Data bytes the plan copies to `side`.
    fn bytes_to_copy(&self, side: Side) -> u64 {
        self.plan
            .rows()
            .iter()
            .filter_map(|row| self.copied_bytes(row))
            .filter(|(to, _)| *to == side)
            .map(|(_, bytes)| bytes)
            .fold(0, u64::saturating_add)
    }

    /// Copies the run makes at once: as many as the slower folder takes.
    /// A copy between two servers holds a session on each, so two in
    /// opposite directions could each wait for the other's server once a
    /// server refuses more sessions: such pairs copy one at a time.
    fn workers(&self) -> NonZeroUsize {
        let endpoint = |side| self.endpoints.endpoint(side);
        if matches!(
            (endpoint(Side::Left), endpoint(Side::Right)),
            (Endpoint::Sftp(_), Endpoint::Sftp(_))
        ) {
            return NonZeroUsize::MIN;
        }
        endpoint(Side::Left)
            .workers()
            .min(endpoint(Side::Right).workers())
    }

    /// The side a row copies file data to, and how many bytes.
    fn copied_bytes(&self, row: &PlanRow) -> Option<(Side, u64)> {
        let CandidateDecision::Entry(EntryDecision::Propagate {
            from,
            change: ChangeKind::Create | ChangeKind::ReplaceContent,
        }) = row.decision()
        else {
            return None;
        };
        let source = self.observation(*from).entries().get(row.path())?;
        (source.version().kind() == foldover_types::EntryKind::File)
            .then(|| (from.other(), source.size()))
    }

    fn observation(&self, side: Side) -> &Observation {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }
}

/// A failed read of one side, with guidance when access was refused.
fn observe_error(side: Side, source: ScanError) -> RunError {
    if source.is_access_denied() {
        return RunError::AccessDenied { side, source };
    }
    RunError::Observe { side, source }
}

/// A folder that ignores Unicode normalization may store a name in
/// another normalization than it was written in: HFS+ decomposes `é`
/// (PLAN.md §4.10). An entry the base knows in one spelling, observed
/// only in another, is mapped back to the base's, so the respelling is no
/// change. The folder finds the file by either spelling, so writing it by
/// the base's still lands on it. Case is left alone: a case-only rename
/// is a person's, and syncs as one (ADR 0030).
fn align_spellings(
    observation: &mut Observation,
    base: &BTreeMap<SyncPath, BaselineEntry>,
    names: NameMatching,
) {
    if names.unicode() != UnicodeMatching::Normalized {
        return;
    }
    let composed = |path: &SyncPath| -> Option<String> {
        let text = std::str::from_utf8(path.as_bytes()).ok()?;
        Some(text.nfc().collect())
    };
    // Base spellings this folder no longer shows, by composed form; two
    // that compose alike are ambiguous and left alone.
    let mut missing: BTreeMap<String, Option<&SyncPath>> = BTreeMap::new();
    for path in base.keys() {
        if observation.entries().contains_key(path) {
            continue;
        }
        if let Some(key) = composed(path) {
            missing
                .entry(key)
                .and_modify(|known| *known = None)
                .or_insert(Some(path));
        }
    }
    if missing.is_empty() {
        return;
    }
    let respelled: Vec<(SyncPath, SyncPath)> = observation
        .entries()
        .keys()
        .chain(observation.unavailable())
        .filter(|path| !base.contains_key(*path))
        .filter_map(|path| {
            let known = missing.get(&composed(path)?)?.as_ref()?;
            Some((path.clone(), (*known).clone()))
        })
        .collect();
    // Such a folder holds one spelling of a name; should two map to one
    // base spelling anyway, neither is mapped rather than one lost.
    // Count once: a restored tree can respell tens of thousands of names.
    let mut destinations: BTreeMap<&SyncPath, usize> = BTreeMap::new();
    for (_, to) in &respelled {
        *destinations.entry(to).or_default() += 1;
    }

    for (from, to) in &respelled {
        if destinations[to] == 1 {
            observation.respell(from, to.clone());
        }
    }
}

fn require_space(side: Side, needed: u64, available: u64) -> Result<(), RunError> {
    // Subtracting avoids overflow: needed + reserve could exceed u64::MAX.
    if needed > available.saturating_sub(SPACE_RESERVE) {
        return Err(RunError::InsufficientSpace {
            side,
            needed,
            available,
        });
    }
    Ok(())
}

/// Whether a planned row changes either folder.
fn writes(decision: &CandidateDecision) -> bool {
    matches!(
        decision,
        CandidateDecision::Entry(
            EntryDecision::Propagate { .. }
                | EntryDecision::Archive { .. }
                | EntryDecision::Move { .. }
        )
    )
}

fn is_archive(decision: &CandidateDecision) -> bool {
    matches!(
        decision,
        CandidateDecision::Entry(EntryDecision::Archive { .. })
    )
}

struct Executor<'a> {
    run: &'a PairRun,
    /// Each side's view of the run, left then right.
    scopes: &'a [RunScope; 2],
    report: RunReport,
    pending: Vec<BaseUpdate>,
    /// The writes behind `pending`, committed with it.
    journal: Vec<JournalEntry>,
    /// Writes journaled so far: the next entry's sequence number.
    written: u64,
    /// Paths whose change failed; their descendants are skipped.
    failed: Vec<SyncPath>,
}

/// What a write needs from the run, shared by the run's thread and the
/// copy workers.
#[derive(Clone, Copy)]
struct Writer<'a> {
    run: &'a PairRun,
    scopes: &'a [RunScope; 2],
}

impl Writer<'_> {
    fn scope(&self, side: Side) -> &RunScope {
        &self.scopes[side as usize]
    }

    /// Make the copy `row` plans: the version stored.
    fn copy(
        &self,
        row: &PlanRow,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<EntryVersion, ApplyError> {
        let CandidateDecision::Entry(EntryDecision::Propagate { from, change }) = row.decision()
        else {
            unreachable!("only planned copies are handed out")
        };
        let path = row.path();
        let entry = |side: Side| self.run.observation(side).entries().get(path);
        let source = entry(*from).expect("handed-out copies have a source");
        let target = from.other();
        self.propagate(path, source, target, entry(target), *change, control)
    }

    fn propagate(
        &self,
        path: &SyncPath,
        source: &ObservedEntry,
        target: Side,
        current: Option<&ObservedEntry>,
        change: ChangeKind,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<EntryVersion, ApplyError> {
        let endpoints = &self.run.endpoints;
        let from: &Endpoint = endpoints.endpoint(target.other());
        let to = endpoints.endpoint(target);
        match (change, current) {
            (ChangeKind::UpdateMetadata, Some(current)) => {
                to.update_metadata_from(from, path, source, current, self.scope(target), control)
            }
            (_, current) => {
                let current = current.map_or(Expected::Absent, Expected::Present);
                to.copy_from(from, path, source, current, self.scope(target), control)
            }
        }
    }
}

impl<'a> Executor<'a> {
    fn scope(&self, side: Side) -> &RunScope {
        &self.scopes[side as usize]
    }

    fn writer(&self) -> Writer<'a> {
        Writer {
            run: self.run,
            scopes: self.scopes,
        }
    }

    /// Whether `row` is a copy of a file or link a worker may make: its
    /// folder exists by now, and nothing below it waits on it.
    fn copies(&self, row: &PlanRow) -> bool {
        let CandidateDecision::Entry(EntryDecision::Propagate { from, .. }) = row.decision() else {
            return false;
        };
        let source = self.run.observation(*from).entries().get(row.path());
        source.is_some_and(|source| source.version().kind() != EntryKind::Directory)
            && !self.below_failure(row.path())
    }

    fn below_failure(&self, path: &SyncPath) -> bool {
        self.failed.iter().any(|failed| failed.is_ancestor_of(path))
    }

    /// Record the copy a worker made for `row`.
    fn landed(&mut self, row: &PlanRow, result: Result<EntryVersion, ApplyError>) {
        let CandidateDecision::Entry(EntryDecision::Propagate { from, change }) = row.decision()
        else {
            unreachable!("only planned copies are handed out")
        };
        let source = self.run.observation(*from).entries().get(row.path());
        let source = source.expect("handed-out copies have a source").version();
        match result {
            Ok(stored) => self.record(row.path(), *from, source, stored, *change),
            Err(error) => self.fail(row.path(), &error),
        }
    }

    /// Data bytes `row` copies.
    fn copied_bytes(&self, row: &PlanRow) -> u64 {
        self.run.copied_bytes(row).map_or(0, |(_, bytes)| bytes)
    }

    fn apply(
        &mut self,
        path: &SyncPath,
        decision: &CandidateDecision,
        control: &mut impl FnMut() -> ScanControl,
    ) {
        let decision = match decision {
            CandidateDecision::BlockedByAncestor(_) => return,
            CandidateDecision::Entry(decision) => *decision,
        };
        if self.below_failure(path) {
            return;
        }
        let entry = |side: Side| self.run.observation(side).entries().get(path);
        match decision {
            EntryDecision::Unchanged | EntryDecision::KeepTarget => {}
            EntryDecision::Conflict(kind) => self.report.conflicts.push((path.clone(), kind)),
            EntryDecision::ForgetBase => self.pending.push(BaseUpdate::Forget(path.clone())),
            EntryDecision::RecordBase => {
                let versions = entry(Side::Left).zip(entry(Side::Right));
                match versions
                    .map(|(left, right)| BaselineEntry::new(left.version(), right.version()))
                {
                    Some(Ok(base)) => self.pending.push(BaseUpdate::Record(path.clone(), base)),
                    _ => self.fail_message(path, "Planned base record has no matching sides"),
                }
            }
            EntryDecision::Propagate { from, change } => {
                let Some(source) = entry(from) else {
                    return self.fail_message(path, "Planned source entry is missing");
                };
                let target = from.other();
                let stored =
                    self.writer()
                        .propagate(path, source, target, entry(target), change, control);
                match stored {
                    Ok(stored) => self.record(path, from, source.version(), stored, change),
                    Err(error) => self.fail(path, &error),
                }
            }
            EntryDecision::Archive { on } => {
                let Some(current) = entry(on) else {
                    return self.fail_message(path, "Planned archive entry is missing");
                };
                match self
                    .run
                    .endpoints
                    .endpoint(on)
                    .remove(path, current, self.scope(on), control)
                {
                    Ok(()) => {
                        self.report.archived += 1;
                        self.journal(path, on, JournalAction::Archive(current.version()));
                        self.pending.push(BaseUpdate::Forget(path.clone()));
                    }
                    Err(error) => self.fail(path, &error),
                }
            }
            // The row the entry moves to carries the move.
            EntryDecision::MovedAway { .. } => {}
            EntryDecision::Move { on } => self.move_here(path, on, control),
        }
    }

    /// Rename on `on` the file or link the other folder moved to `to`:
    /// same content, so nothing is copied or archived (PLAN.md §4.7). It
    /// keeps this folder's metadata until brought to the source's.
    fn move_here(&mut self, to: &SyncPath, on: Side, control: &mut impl FnMut() -> ScanControl) {
        let run = self.run;
        let Some(from) = run.plan.moved_from(to) else {
            return self.fail_message(to, "Planned move has no source");
        };
        if self.failed.iter().any(|failed| failed.is_ancestor_of(from)) {
            return;
        }
        let (Some(current), Some(source)) = (
            run.observation(on).entries().get(from),
            run.observation(on.other()).entries().get(to),
        ) else {
            return self.fail_message(to, "Planned move entry is missing");
        };
        let target = run.endpoints.endpoint(on);
        if let Err(error) = target.move_entry(from, current, to, self.scope(on), control) {
            return self.fail(to, &error);
        }
        // The old path is gone either way; its base goes with it.
        self.pending.push(BaseUpdate::Forget(from.clone()));
        let stored = if current.version() == source.version() {
            Ok(current.version())
        } else {
            let from_side = run.endpoints.endpoint(on.other());
            target.update_metadata_from(from_side, to, source, current, self.scope(on), control)
        };
        let journaled = *stored.as_ref().unwrap_or(&current.version());
        self.journal.push(JournalEntry {
            seq: self.written,
            path: to.clone(),
            side: on,
            action: JournalAction::Move(journaled),
            before: run.base.get(from).copied(),
            moved_from: Some(from.clone()),
        });
        self.written += 1;
        let stored = match stored {
            Ok(stored) => stored,
            Err(error) => return self.fail(to, &error),
        };
        let (left, right) = match on {
            Side::Left => (stored, source.version()),
            Side::Right => (source.version(), stored),
        };
        match BaselineEntry::new(left, right) {
            Ok(base) => {
                self.report.moved += 1;
                self.pending.push(BaseUpdate::Record(to.clone(), base));
            }
            Err(_) => self.fail_message(to, "Moved content differs from the source"),
        }
    }

    fn record(
        &mut self,
        path: &SyncPath,
        from: Side,
        source: EntryVersion,
        stored: EntryVersion,
        change: ChangeKind,
    ) {
        // The write happened whatever the base says next.
        let action = match change {
            ChangeKind::Create => JournalAction::Create(stored),
            ChangeKind::ReplaceContent => JournalAction::Replace(stored),
            ChangeKind::UpdateMetadata => JournalAction::Metadata(stored),
        };
        self.journal(path, from.other(), action);
        let (left, right) = match from {
            Side::Left => (source, stored),
            Side::Right => (stored, source),
        };
        match BaselineEntry::new(left, right) {
            Ok(base) => {
                match change {
                    ChangeKind::UpdateMetadata => self.report.updated += 1,
                    ChangeKind::Create | ChangeKind::ReplaceContent => self.report.copied += 1,
                }
                self.pending.push(BaseUpdate::Record(path.clone(), base));
            }
            Err(_) => self.fail_message(path, "Stored content differs from the source"),
        }
    }

    fn journal(&mut self, path: &SyncPath, side: Side, action: JournalAction) {
        self.journal.push(JournalEntry {
            seq: self.written,
            path: path.clone(),
            side,
            action,
            before: self.run.base.get(path).copied(),
            moved_from: None,
        });
        self.written += 1;
    }

    fn fail(&mut self, path: &SyncPath, error: &ApplyError) {
        self.fail_message(path, &error.to_string());
    }

    fn fail_message(&mut self, path: &SyncPath, message: &str) {
        self.report.failures.push(Failure {
            path: Some(path.clone()),
            message: message.to_owned(),
        });
        self.failed.push(path.clone());
    }
}

mod copies;
use copies::Copies;

mod undo;
pub use undo::UndoReport;

/// Whether `endpoint` takes renames for moves; another Mac's folder takes
/// copies, so an older agent there never meets a request it lacks.
fn moves(endpoint: &Endpoint) -> Moves {
    match endpoint.moves() {
        true => Moves::Rename,
        false => Moves::Copy,
    }
}

/// The pair's options as a run on `endpoints` applies them. Gatekeeper's
/// download mark is never written to a network volume (PLAN.md §4.10),
/// so it leaves both folders' versions when either is one; otherwise one
/// side would always differ. A folder that cannot tell fails the run
/// rather than risk writing the mark to a share.
fn run_options(options: &PairOptions, endpoints: &EndpointPair) -> Result<PairOptions, RunError> {
    let mut quarantine = Propagation::Sync;
    for side in [Side::Left, Side::Right] {
        let networked = endpoints
            .endpoint(side)
            .on_network_volume()
            .map_err(|source| observe_error(side, source))?;
        if networked {
            quarantine = Propagation::Ignore;
        }
    }
    Ok(options
        .clone()
        .with_metadata(options.metadata().with_quarantine(quarantine)))
}

/// Unread entries, with everything below them on either
/// side or in the base: skipped in both folders, unchanged to the base.
fn unavailable<'a>(
    left: &'a Observation,
    right: &'a Observation,
    base: &'a BTreeMap<SyncPath, BaselineEntry>,
    names: NameMatching,
) -> impl Iterator<Item = &'a SyncPath> {
    // Match as the planner does, so another spelling cannot become a deletion.
    let roots: BTreeSet<Vec<u8>> = left
        .unavailable()
        .iter()
        .chain(right.unavailable())
        .map(|root| crate::plan::name_key(root.as_bytes(), names))
        .collect();
    let below = move |path: &&SyncPath| {
        if roots.is_empty() {
            return false;
        }
        let key = crate::plan::name_key(path.as_bytes(), names);
        roots.contains(&key)
            || key
                .iter()
                .enumerate()
                .any(|(index, &byte)| byte == b'/' && roots.contains(&key[..index]))
    };
    left.unavailable()
        .iter()
        .chain(right.unavailable())
        .chain(left.entries().keys())
        .chain(right.entries().keys())
        .chain(base.keys())
        .filter(below)
}

/// Missing names below a listing that changed cannot authorize deletion or
/// baseline removal. Safe creates still recheck absence at the write boundary.
fn uncertain_absences(
    left: &Observation,
    right: &Observation,
    base: &BTreeMap<SyncPath, BaselineEntry>,
    names: NameMatching,
) -> Vec<SyncPath> {
    let mut paths = BTreeMap::new();
    for (seen, other) in [(left, right), (right, left)] {
        let unstable = seen.unstable_directories();
        if unstable.is_empty() {
            continue;
        }
        let whole_root = unstable.iter().any(Option::is_none);
        let directories: BTreeSet<_> = unstable
            .iter()
            .flatten()
            .map(|path| crate::plan::name_key(path.as_bytes(), names))
            .collect();
        // Consider only missing counterparts and base rows. A first backup
        // with an empty target needs no second index of its source tree.
        let mut missing: BTreeMap<_, _> = other
            .entries()
            .keys()
            .chain(base.keys())
            .filter(|path| !seen.entries().contains_key(*path))
            .map(|path| (crate::plan::name_key(path.as_bytes(), names), path))
            .filter(|(key, _)| {
                whole_root
                    || directories.contains(key)
                    || key
                        .iter()
                        .enumerate()
                        .any(|(index, &byte)| byte == b'/' && directories.contains(&key[..index]))
            })
            .collect();
        if missing.is_empty() {
            continue;
        }
        // Both folders may spell an observed entry differently. A case or
        // Unicode alias still proves presence and must remain usable.
        for path in seen.entries().keys() {
            missing.remove(&crate::plan::name_key(path.as_bytes(), names));
        }
        paths.extend(missing);
    }
    paths.into_values().cloned().collect()
}

#[cfg(test)]
mod tests;
