//! Unattended runs. `Every` pairs are due one interval after their last
//! scheduled start, or after the agent started. `OnChange` pairs are due
//! once their folders have been quiet briefly after a change, and hourly,
//! because change notifications can be missed. Pairs that never completed
//! a first sync are skipped: their first run is reviewed by a person.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use foldover_engine::PairWatcher;
use foldover_types::{PairConfig, PairId, Schedule};

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
const SECONDS_PER_MINUTE: u64 = 60;

pub(crate) struct Scheduler {
    started: Instant,
    last: Mutex<HashMap<PairId, Instant>>,
    changed: Mutex<HashMap<PairId, Instant>>,
    watcher: Mutex<Option<PairWatcher>>,
    watched: Mutex<Vec<PairId>>,
    /// Pairs with a folder that could not be watched, retried every tick.
    unwatchable: Mutex<HashSet<PairId>>,
    reconnected: Mutex<HashMap<PairId, Instant>>,
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
            // An unmounted disk or share is waited for, not reported as a
            // failure; the pair stays due and runs once the folder is back.
            // A share is mounted again from where it last came from.
            if let Some(side) = foldover_engine::missing_side(&pair) {
                let source = self
                    .engine
                    .lock()
                    .ok()
                    .and_then(|engine| engine.mounts(pair.id()).ok())
                    .and_then(|mounts| mounts.side(side).map(str::to_owned));
                let back = source.is_some_and(|source| {
                    self.scheduler.may_reconnect(pair.id(), now)
                        && foldover_engine::reconnect(&pair, side, &source)
                });
                if !back {
                    self.jobs.wait_for(pair.id(), side);
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
}
