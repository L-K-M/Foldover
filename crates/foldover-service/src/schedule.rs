//! Unattended runs. `Every` pairs are due one interval after their last
//! scheduled start, or after the agent started. `OnChange` pairs are due
//! once their folders have been quiet briefly after a change, and hourly,
//! because change notifications can be missed. Pairs that never completed
//! a first sync are skipped: their first run is reviewed by a person.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use foldover_engine::{PairWatcher, Reach};
use foldover_types::{PairConfig, PairId, Schedule, Side};

use crate::Service;
use crate::sync::Work;

/// How often the agent checks for due pairs.
const TICK: Duration = Duration::from_secs(5);
/// Changes must stop this long before a run, so a copy in progress settles.
const QUIET: Duration = Duration::from_secs(5);
/// On-change pairs are also verified this often.
const VERIFY: Duration = Duration::from_secs(60 * 60);
/// A share that went away is mounted again at most this often.
const RECONNECT: Duration = Duration::from_secs(5 * 60);
/// A due pair's folders are checked at most this often.
pub(crate) const PROBE: Duration = Duration::from_secs(30);
const SECONDS_PER_MINUTE: u64 = 60;

/// A due pair's folder check: at most one runs at a time and results
/// expire after `PROBE`, so a dead server is not hammered and a parked
/// check cannot hold the pairs behind it.
enum Probe {
    /// On the blocking pool; the pair waits for it.
    Running,
    /// The check blew up; retry on the cadence, not every tick.
    Failed { at: Instant },
    /// The check finished; the first tick to see it stamps the result.
    Done(Option<Side>),
    /// Both folders were present at `at`.
    Ready { at: Instant },
    /// A folder was missing at `at`.
    Missing { side: Side, at: Instant },
}

/// The folder presence check probes run, `missing_side` by default.
type FolderCheck = dyn Fn(&PairConfig, &Reach) -> Option<Side> + Send + Sync;

/// What a due pair may do this tick.
enum Gate {
    /// Folders were recently present: the pair may run.
    Start,
    /// A folder is missing; check again after `PROBE`.
    Wait(Side),
    /// No fresh result: check the folders off the tick.
    Probe,
    /// A check is in flight; do not block on it.
    Parked,
}

pub(crate) struct Scheduler {
    started: Instant,
    last: Mutex<HashMap<PairId, Instant>>,
    changed: Mutex<HashMap<PairId, Instant>>,
    watcher: Mutex<Option<PairWatcher>>,
    watched: Mutex<Vec<PairId>>,
    /// Pairs with a folder that could not be watched, retried every tick.
    unwatchable: Mutex<HashSet<PairId>>,
    reconnected: Mutex<HashMap<PairId, Instant>>,
    /// Per-pair folder checks, kept off the tick.
    probes: Mutex<HashMap<PairId, Probe>>,
    /// The folder check probes run; tests replace it.
    probe: Mutex<Arc<FolderCheck>>,
}

impl Scheduler {
    pub(crate) fn new(started: Instant) -> Self {
        Self {
            started,
            last: Mutex::default(),
            changed: Mutex::default(),
            watcher: Mutex::default(),
            watched: Mutex::default(),
            unwatchable: Mutex::default(),
            reconnected: Mutex::default(),
            probes: Mutex::default(),
            probe: Mutex::new(Arc::new(foldover_engine::missing_side)),
        }
    }

    #[cfg(test)]
    pub(crate) fn started(&self) -> Instant {
        self.started
    }

    /// Record a change; the pair runs once changes stop for `QUIET`.
    pub(crate) fn mark_changed(&self, id: PairId, at: Instant) {
        self.changed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, at);
    }

    fn due(&self, pair: &PairConfig, now: Instant) -> bool {
        let previous = self
            .last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&pair.id())
            .copied()
            .unwrap_or(self.started);
        let since = now.saturating_duration_since(previous);
        match pair.draft().options().schedule() {
            Schedule::Manual => false,
            Schedule::Every(interval) => {
                since >= Duration::from_secs(u64::from(interval.minutes()) * SECONDS_PER_MINUTE)
            }
            Schedule::OnChange => {
                let changed = self
                    .changed
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&pair.id())
                    .copied();
                changed.is_some_and(|at| now.saturating_duration_since(at) >= QUIET)
                    || since >= VERIFY
            }
        }
    }

    /// Whether to try mounting the pair's share again now; records the try.
    fn may_reconnect(&self, id: PairId, now: Instant) -> bool {
        let mut reconnected = self
            .reconnected
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if reconnected
            .get(&id)
            .is_some_and(|at| now.saturating_duration_since(*at) < RECONNECT)
        {
            return false;
        }
        reconnected.insert(id, now);
        true
    }

    fn started_run(&self, id: PairId, now: Instant) {
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, now);
        // Later events, including this run's own writes, start another run.
        self.changed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
    }

    /// What a due pair may do now. A parked check is left alone: waiting
    /// for it would let one dead server hold every pair behind it. A
    /// result is stamped with the tick that first sees it, so pool queue
    /// time and a slow check cannot expire it on arrival.
    fn gate(&self, id: PairId, now: Instant) -> Gate {
        let mut probes = self.probes.lock().unwrap_or_else(PoisonError::into_inner);
        match probes.get(&id) {
            Some(Probe::Running) => Gate::Parked,
            Some(Probe::Failed { at }) if now.saturating_duration_since(*at) < PROBE => {
                Gate::Parked
            }
            Some(Probe::Done(missing)) => {
                let missing = *missing;
                probes.insert(
                    id,
                    match missing {
                        Some(side) => Probe::Missing { side, at: now },
                        None => Probe::Ready { at: now },
                    },
                );
                match missing {
                    Some(side) => Gate::Wait(side),
                    None => Gate::Start,
                }
            }
            Some(Probe::Ready { at }) if now.saturating_duration_since(*at) < PROBE => Gate::Start,
            Some(Probe::Missing { side, at }) if now.saturating_duration_since(*at) < PROBE => {
                Gate::Wait(*side)
            }
            _ => Gate::Probe,
        }
    }

    fn probe_started(&self, id: PairId) {
        self.probes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, Probe::Running);
    }

    /// The check finished; carries no timestamp — the first tick to see
    /// the result stamps it fresh against that tick's clock.
    fn probe_done(&self, id: PairId, missing: Option<Side>) {
        self.probes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, Probe::Done(missing));
    }

    /// The check panicked on the blocking pool; `at` is the tick that
    /// asked, so a retry waits out the cadence instead of crashing
    /// every tick.
    fn probe_failed(&self, id: PairId, at: Instant) {
        self.probes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, Probe::Failed { at });
    }

    /// The folder check the probes run, `missing_side` unless replaced.
    fn probe_check(&self) -> Arc<FolderCheck> {
        self.probe
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    #[cfg(test)]
    pub(crate) fn set_probe(
        &self,
        probe: impl Fn(&PairConfig, &Reach) -> Option<Side> + Send + Sync + 'static,
    ) {
        *self.probe.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(probe);
    }

    /// Test hook: no folder check is still on the blocking pool.
    #[cfg(test)]
    pub(crate) fn probes_settled(&self) -> bool {
        self.probes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .all(|probe| !matches!(probe, Probe::Running))
    }

    /// Watch exactly the folders of these pairs, if a watcher runs.
    /// Unwatchable folders (unmounted disks) are retried each tick and get
    /// hourly runs meanwhile; one that becomes watchable counts as changed,
    /// since it may have changed while it was away.
    fn watch(&self, pairs: &[PairConfig], now: Instant) {
        let ids: Vec<_> = pairs.iter().map(PairConfig::id).collect();
        let mut watched = self.watched.lock().unwrap_or_else(PoisonError::into_inner);
        let mut unwatchable = self
            .unwatchable
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *watched == ids && unwatchable.is_empty() {
            return;
        }
        *watched = ids;
        let mut watcher = self.watcher.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(watcher) = watcher.as_mut() else {
            return;
        };

        let failed: HashSet<_> = watcher.watch(pairs).into_iter().map(|(id, _)| id).collect();
        for id in unwatchable.difference(&failed) {
            if watched.contains(id) {
                self.mark_changed(*id, now);
            }
        }
        *unwatchable = failed;
    }
}

impl Service {
    /// Watch changing pairs and start unattended runs until the agent exits.
    pub fn start_scheduler(&self) {
        let scheduler = Arc::clone(&self.scheduler);
        let watcher = PairWatcher::new(move |id| scheduler.mark_changed(id, Instant::now()));
        // Without notifications, on-change pairs still run hourly.
        *self
            .scheduler
            .watcher
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = watcher.ok();
        let service = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(TICK);
            loop {
                tick.tick().await;
                let due = service.clone();
                // Reading the catalog takes the engine lock; keep it off the runtime.
                let _ = tokio::task::spawn_blocking(move || due.run_due(Instant::now())).await;
            }
        });
    }

    /// Start every due pair's unattended run; returns the pairs started.
    pub(crate) fn run_due(&self, now: Instant) -> Vec<PairId> {
        let candidates = {
            let Ok(engine) = self.engine.lock() else {
                return Vec::new();
            };
            // Changes stay recorded during a pause and run after it.
            if engine
                .pause()
                .is_ok_and(|pause| pause.active(SystemTime::now()))
            {
                return Vec::new();
            }
            let Ok(pairs) = engine.pairs() else {
                return Vec::new();
            };
            pairs
                .into_iter()
                .filter(|pair| pair.draft().options().schedule() != Schedule::Manual)
                .filter(|pair| engine.first_synced(pair.id()).unwrap_or(false))
                // After an undo; an unreadable hold counts as held.
                .filter(|pair| !engine.automation_held(pair.id()).unwrap_or(true))
                .collect::<Vec<_>>()
        };
        let changing: Vec<_> = candidates
            .iter()
            .filter(|pair| pair.draft().options().schedule() == Schedule::OnChange)
            .cloned()
            .collect();
        self.scheduler.watch(&changing, now);

        let mut started = Vec::new();
        for pair in candidates {
            if !self.scheduler.due(&pair, now) {
                continue;
            }
            match self.scheduler.gate(pair.id(), now) {
                Gate::Parked => continue,
                Gate::Wait(side) => {
                    self.jobs.wait_for(pair.id(), side);
                    continue;
                }
                Gate::Start => {}
                Gate::Probe => {
                    self.scheduler.probe_started(pair.id());
                    let service = self.clone();
                    // Folder checks touch disks and networks; a parked or
                    // dead-end one must not hold the pairs behind it, so
                    // they run on the blocking pool, never on the tick or
                    // the runtime's executor threads.
                    if tokio::runtime::Handle::try_current().is_ok() {
                        tokio::task::spawn_blocking(move || service.probe(pair, now));
                    } else {
                        service.probe(pair, now);
                    }
                    continue;
                }
            }
            // A busy pair is not due again until its job ends.
            if self
                .jobs
                .start(&self.engine, pair.id(), Work::Scheduled)
                .is_ok()
            {
                self.scheduler.started_run(pair.id(), now);
                started.push(pair.id());
            }
        }
        started
    }

    /// Check one due pair's folders on the blocking pool and record the
    /// result for the next tick. An unmounted disk or share is waited
    /// for, not reported as a failure; the pair stays due and runs once
    /// the folder is back. A share is mounted again from where it last
    /// came from.
    fn probe(&self, pair: PairConfig, at: Instant) {
        let id = pair.id();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let check = self.scheduler.probe_check();
            let reach = self
                .engine
                .lock()
                .map(|engine| engine.reach())
                .unwrap_or_default();
            check(&pair, &reach).and_then(|side| {
                let source = self
                    .engine
                    .lock()
                    .ok()
                    .and_then(|engine| engine.mounts(id).ok())
                    .and_then(|mounts| mounts.side(side).map(str::to_owned));
                let back = source.is_some_and(|source| {
                    self.scheduler.may_reconnect(id, at)
                        && foldover_engine::reconnect(&pair, side, &source, &reach)
                });
                if back { None } else { Some(side) }
            })
        }));
        match outcome {
            Ok(missing) => {
                self.scheduler.probe_done(id, missing);
                if let Some(side) = missing {
                    self.jobs.wait_for(id, side);
                }
            }
            // Without this a panic would wedge the pair on `Running`
            // forever; the panic hook already logged it.
            Err(_) => self.scheduler.probe_failed(id, at),
        }
    }

    #[cfg(test)]
    pub(crate) fn mark_changed(&self, id: PairId, at: Instant) {
        self.scheduler.mark_changed(id, at);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use foldover_types::{LocalRoot, PairDraft, PairOptions, SyncMode};

    use super::*;

    #[test]
    fn a_folder_that_becomes_watchable_counts_as_changed() {
        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left");
        let right = directory.path().join("right");
        fs::create_dir(&left).unwrap();
        let root =
            |path: &Path| LocalRoot::from_bytes(path.as_os_str().as_bytes().to_vec()).unwrap();
        let options = PairOptions::default().with_schedule(Schedule::OnChange);
        let draft = PairDraft::new("Disk".into(), root(&left), root(&right), SyncMode::Sync)
            .unwrap()
            .with_options(options);
        let pair =
            PairConfig::from_stored(PairId::from_bytes([1; PairId::BYTE_LEN]).unwrap(), 1, draft)
                .unwrap();
        let start = Instant::now();
        let scheduler = Scheduler::new(start);
        *scheduler.watcher.lock().unwrap() = Some(PairWatcher::new(|_| {}).unwrap());
        let at = |seconds| start + Duration::from_secs(seconds);

        scheduler.watch(std::slice::from_ref(&pair), at(1));
        assert!(!scheduler.due(&pair, at(10)));

        fs::create_dir(&right).unwrap();
        scheduler.watch(std::slice::from_ref(&pair), at(20));
        assert!(!scheduler.due(&pair, at(21)));
        assert!(scheduler.due(&pair, at(26)));
    }

    #[test]
    fn shares_are_mounted_again_at_most_every_few_minutes() {
        let start = Instant::now();
        let scheduler = Scheduler::new(start);
        let id = PairId::from_bytes([2; PairId::BYTE_LEN]).unwrap();
        assert!(scheduler.may_reconnect(id, start));
        assert!(!scheduler.may_reconnect(id, start + Duration::from_secs(60)));
        assert!(scheduler.may_reconnect(id, start + RECONNECT));
    }

    #[test]
    fn a_due_pairs_folders_are_checked_once_and_rechecked_on_a_cadence() {
        let start = Instant::now();
        let scheduler = Scheduler::new(start);
        let id = PairId::from_bytes([3; PairId::BYTE_LEN]).unwrap();
        let at = |seconds| start + Duration::from_secs(seconds);

        assert!(matches!(scheduler.gate(id, at(0)), Gate::Probe));
        scheduler.probe_started(id);
        assert!(matches!(scheduler.gate(id, at(1)), Gate::Parked));
        scheduler.probe_done(id, Some(Side::Left));
        assert!(matches!(scheduler.gate(id, at(2)), Gate::Wait(Side::Left)));
        assert!(matches!(scheduler.gate(id, at(2) + PROBE), Gate::Probe));
        scheduler.probe_started(id);
        scheduler.probe_done(id, None);
        assert!(matches!(scheduler.gate(id, at(3) + PROBE), Gate::Start));
        assert!(matches!(
            scheduler.gate(id, at(3) + PROBE + PROBE),
            Gate::Probe
        ));

        // A crashed check is retried on the cadence, not every tick.
        scheduler.probe_failed(id, at(4) + PROBE + PROBE);
        assert!(matches!(
            scheduler.gate(id, at(5) + PROBE + PROBE),
            Gate::Parked
        ));
        assert!(matches!(
            scheduler.gate(id, at(4) + PROBE + PROBE + PROBE),
            Gate::Probe
        ));

        // A result carries no timestamp: it is fresh whenever a tick
        // first sees it, however long the check itself took.
        scheduler.probe_done(id, None);
        assert!(matches!(scheduler.gate(id, at(500)), Gate::Start));
    }
}
