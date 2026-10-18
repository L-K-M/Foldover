//! Unattended runs for pairs with an interval. A pair is due one interval
//! after its last scheduled start, or after the agent started. Pairs that
//! never completed a first sync are skipped: their first run is reviewed.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use foldover_types::{PairId, Schedule};

use crate::Service;
use crate::sync::Work;

/// How often the agent checks for due pairs.
const TICK: Duration = Duration::from_secs(30);
const SECONDS_PER_MINUTE: u64 = 60;

pub(crate) struct Scheduler {
    started: Instant,
    last: Mutex<HashMap<PairId, Instant>>,
}

impl Scheduler {
    #[cfg(test)]
    pub(crate) fn started(&self) -> Instant {
        self.started
    }

    pub(crate) fn new(started: Instant) -> Self {
        Self {
            started,
            last: Mutex::new(HashMap::new()),
        }
    }
}

impl Service {
    /// Start unattended runs for due pairs until the agent exits.
    pub fn start_scheduler(&self) {
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
                .filter_map(|pair| match pair.draft().options().schedule() {
                    Schedule::Manual => None,
                    Schedule::Every(interval) => engine
                        .first_synced(pair.id())
                        .ok()
                        .filter(|synced| *synced)
                        .map(|_| (pair.id(), interval.minutes())),
                })
                .collect::<Vec<_>>()
        };

        let mut last = self
            .scheduler
            .last
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut started = Vec::new();
        for (id, minutes) in candidates {
            let previous = last.get(&id).copied().unwrap_or(self.scheduler.started);
            let interval = Duration::from_secs(u64::from(minutes) * SECONDS_PER_MINUTE);
            if now.saturating_duration_since(previous) < interval {
                continue;
            }
            // A busy pair is not due again until its job ends.
            if self.jobs.start(&self.engine, id, Work::Scheduled).is_ok() {
                last.insert(id, now);
                started.push(id);
            }
        }
        started
    }
}
