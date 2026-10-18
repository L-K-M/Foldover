//! Unattended runs. `Every` pairs are due one interval after their last
//! scheduled start, or after the agent started. `OnChange` pairs are due
//! once their folders have been quiet briefly after a change, and hourly,
//! because change notifications can be missed. Pairs that never completed
//! a first sync are skipped: their first run is reviewed by a person.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

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
const SECONDS_PER_MINUTE: u64 = 60;

pub(crate) struct Scheduler {
    started: Instant,
    last: Mutex<HashMap<PairId, Instant>>,
    changed: Mutex<HashMap<PairId, Instant>>,
    watcher: Mutex<Option<PairWatcher>>,
    watched: Mutex<Vec<PairId>>,
}

impl Scheduler {
    pub(crate) fn new(started: Instant) -> Self {
        Self {
            started,
            last: Mutex::default(),
            changed: Mutex::default(),
            watcher: Mutex::default(),
            watched: Mutex::default(),
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
    fn watch(&self, pairs: &[PairConfig]) {
        let ids: Vec<_> = pairs.iter().map(PairConfig::id).collect();
        let mut watched = self.watched.lock().unwrap_or_else(PoisonError::into_inner);
        if *watched == ids {
            return;
        }
        if let Some(watcher) = self
            .watcher
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            // Unwatchable folders (unmounted disks) still get hourly runs.
            let _ = watcher.watch(pairs);
        }
        *watched = ids;
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
        self.scheduler.watch(&changing);

        let mut started = Vec::new();
        for pair in candidates {
            if !self.scheduler.due(&pair, now) {
                continue;
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
