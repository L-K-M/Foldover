//! Unattended runs. `Every` pairs are due one interval after their last
//! scheduled start, or after the agent started. `OnChange` pairs are due
//! once their folders have been quiet briefly after a change, after five
//! minutes of changes that never settle, and hourly, because change
//! notifications can be missed. When the agent starts, a folder whose
//! volume history shows a change since it was last quiet, or that has
//! no usable history, counts as changed; folders that hear of no other
//! computer's changes (shares, SFTP) are checked every few minutes
//! (PLAN.md §3.5, §4.8). `Daily` pairs are due once
//! their latest scheduled local moment has passed the last run's start.
//! Pairs that never completed a first sync are skipped: their first run
//! is reviewed by a person.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use foldover_engine::{
    ChangePoll, Environment, PairWatcher, PollMode, Polled, Reach, WatchCursor, local_time,
};
use foldover_proto::local::SyncStatus;
use foldover_types::{LocalDateTime, PairConfig, PairId, PairRoot, Schedule, Side};

use crate::Service;
use crate::sync::Work;

/// How often the agent checks for due pairs.
const TICK: Duration = Duration::from_secs(5);
/// Changes must stop this long before a run, so a copy in progress settles.
const QUIET: Duration = Duration::from_secs(10);
/// A folder that never stops changing runs anyway after this long.
const CHURN: Duration = Duration::from_secs(5 * 60);
/// How often folders without notifications for other computers' changes
/// are checked for them.
const POLL: Duration = Duration::from_secs(5 * 60);
/// How often a quiet pair records where its folders' event history
/// stands, so a restart replays only what came after.
const CURSOR_SAVE: Duration = Duration::from_secs(60);
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

/// Whether a pair's folders changed while the agent was not watching,
/// `changed_while_away` by default; the last argument is the agent's
/// state directory, whose own writes never count.
type AwayCheck =
    dyn Fn(&PairConfig, &[Option<WatchCursor>; 2], Option<&Path>) -> bool + Send + Sync;

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

/// One pair's checks for changes notifications miss.
struct PollSlot {
    /// The configuration revision checked; another starts over.
    revision: u64,
    /// `None` while a check runs on the blocking pool.
    poll: Option<ChangePoll>,
    /// When the last check started, on both clocks: run history records
    /// wall-clock times.
    checked: Option<(Instant, SystemTime)>,
    need: PollNeed,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum PollNeed {
    /// Not checked yet, or the last check failed.
    Unknown,
    /// A folder needs checking.
    Needed,
    /// Notifications cover both folders.
    NotNeeded,
}

impl PollSlot {
    fn new(revision: u64) -> Self {
        Self {
            revision,
            poll: Some(ChangePoll::default()),
            checked: None,
            need: PollNeed::Unknown,
        }
    }
}

pub(crate) struct Scheduler {
    started: Instant,
    last: Mutex<HashMap<PairId, Instant>>,
    changed: Mutex<HashMap<PairId, Instant>>,
    /// When each pair's folders first changed since its last run.
    churning: Mutex<HashMap<PairId, Instant>>,
    watcher: Mutex<Option<PairWatcher>>,
    /// `None` until the first tick watches anything. What is watched
    /// follows the pairs' folders, so an edit that moves a pair onto new
    /// folders re-subscribes although its id stayed (PLAN.md §4.8).
    watched: Mutex<Option<Vec<(PairId, PairRoot, PairRoot)>>>,
    /// Pairs with a folder that could not be watched, retried every tick.
    unwatchable: Mutex<HashSet<PairId>>,
    reconnected: Mutex<HashMap<PairId, Instant>>,
    /// Per-pair folder checks, kept off the tick.
    probes: Mutex<HashMap<PairId, Probe>>,
    /// Per-pair checks for changes notifications miss.
    polls: Mutex<HashMap<PairId, PollSlot>>,
    /// Pairs whose start-up replay is still running; their cursors wait.
    catching_up: Mutex<HashSet<PairId>>,
    /// When each pair last recorded its cursors.
    cursor_saved: Mutex<HashMap<PairId, Instant>>,
    /// Pairs recording their cursors on the blocking pool: one at a time,
    /// so a share that stalls cannot pile up waiting threads.
    saving: Mutex<HashSet<PairId>>,
    /// The file event at the previous tick: a cursor taken a tick late
    /// gives events time to arrive.
    tick_event: Mutex<Option<u64>>,
    /// The start-up replay; tests replace it.
    away: Mutex<Arc<AwayCheck>>,
    /// The folder check probes run; tests replace it.
    probe: Mutex<Arc<FolderCheck>>,
}

impl Scheduler {
    pub(crate) fn new(started: Instant) -> Self {
        Self {
            started,
            last: Mutex::default(),
            changed: Mutex::default(),
            churning: Mutex::default(),
            watcher: Mutex::default(),
            watched: Mutex::default(),
            unwatchable: Mutex::default(),
            reconnected: Mutex::default(),
            probes: Mutex::default(),
            polls: Mutex::default(),
            catching_up: Mutex::default(),
            cursor_saved: Mutex::default(),
            saving: Mutex::default(),
            tick_event: Mutex::default(),
            away: Mutex::new(Arc::new(foldover_engine::changed_while_away)),
            probe: Mutex::new(Arc::new(foldover_engine::missing_side)),
        }
    }

    #[cfg(test)]
    pub(crate) fn started(&self) -> Instant {
        self.started
    }

    /// Record a change; the pair runs once changes stop for `QUIET`, or
    /// `CHURN` after the first one.
    pub(crate) fn mark_changed(&self, id: PairId, at: Instant) {
        self.changed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, at);
        self.churning
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(id)
            .or_insert(at);
    }

    /// `last_run` is the pair's last recorded run start in local
    /// wall-clock time; only `Daily` consults it, because `Instant`
    /// survives neither restarts nor sleep.
    fn due(
        &self,
        pair: &PairConfig,
        now: Instant,
        local_now: Option<LocalDateTime>,
        last_run: Option<LocalDateTime>,
    ) -> bool {
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
                let first = self
                    .churning
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&pair.id())
                    .copied();
                changed.is_some_and(|at| now.saturating_duration_since(at) >= QUIET)
                    || first.is_some_and(|at| now.saturating_duration_since(at) >= CHURN)
                    || since >= VERIFY
            }
            // The latest scheduled moment must be newer than the last
            // run's start: a moment missed while the agent slept or was
            // off is caught up once here, never replayed, and a pair
            // with no recorded run is due once.
            Schedule::Daily(daily) => match local_now {
                Some(local) => last_run.is_none_or(|last| daily.latest_at_or_before(local) > last),
                None => false,
            },
        }
    }

    /// Forget the polls of pairs no longer changing-triggered, and those
    /// of pairs whose configuration changed.
    fn keep_polls(&self, pairs: &[PairConfig]) {
        let mut polls = self.polls.lock().unwrap_or_else(PoisonError::into_inner);
        polls.retain(|id, slot| {
            pairs
                .iter()
                .any(|pair| pair.id() == *id && pair.revision() == slot.revision)
        });
    }

    /// Whether the pair's folders may need a check now: one is not
    /// running, and the last did not find notifications cover them.
    fn may_poll(&self, pair: &PairConfig) -> bool {
        let mut polls = self.polls.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = polls
            .entry(pair.id())
            .or_insert_with(|| PollSlot::new(pair.revision()));
        slot.poll.is_some() && slot.need != PollNeed::NotNeeded
    }

    /// Take the pair's poll when a check is due: `POLL` after the last,
    /// or at once to only record the folders when there was none yet or
    /// a run finished since, so the run's own writes are not changes.
    fn take_poll(
        &self,
        id: PairId,
        now: Instant,
        last_finished: Option<SystemTime>,
    ) -> Option<(ChangePoll, PollMode, SystemTime)> {
        let mut polls = self.polls.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = polls.get_mut(&id)?;
        let mode = match slot.checked {
            None => PollMode::Baseline,
            Some((_, checked)) if last_finished.is_some_and(|finished| finished >= checked) => {
                PollMode::Baseline
            }
            Some((at, _)) if now.saturating_duration_since(at) >= POLL => PollMode::Compare,
            Some(_) => return None,
        };
        let poll = slot.poll.take()?;
        let started = SystemTime::now();
        slot.checked = Some((now, started));
        Some((poll, mode, started))
    }

    /// Return a pair's poll after its check under `revision`. A pair
    /// forgotten meanwhile stays forgotten, and one edited meanwhile keeps
    /// its fresh poll: this one looked at the old folders.
    fn poll_done(&self, id: PairId, revision: u64, poll: ChangePoll, need: PollNeed) {
        let mut polls = self.polls.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = polls.get_mut(&id).filter(|slot| slot.revision == revision) {
            slot.poll = Some(poll);
            slot.need = need;
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
        self.churning
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
    /// since it may have changed while it was away. At the first tick,
    /// returns the watchable pairs: nothing watched them before the agent
    /// started, so their histories need replaying.
    fn watch(&self, pairs: &[PairConfig], reach: &Reach, now: Instant) -> Vec<PairId> {
        let ids: Vec<_> = pairs.iter().map(PairConfig::id).collect();
        let folders: Vec<_> = pairs
            .iter()
            .map(|pair| {
                let draft = pair.draft();
                (pair.id(), draft.left().clone(), draft.right().clone())
            })
            .collect();
        let mut watched = self.watched.lock().unwrap_or_else(PoisonError::into_inner);
        let mut unwatchable = self
            .unwatchable
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if watched.as_ref() == Some(&folders) && unwatchable.is_empty() {
            return Vec::new();
        }
        let first = watched.is_none();
        *watched = Some(folders);
        let mut watcher = self.watcher.lock().unwrap_or_else(PoisonError::into_inner);
        let failed: HashSet<_> = match watcher.as_mut() {
            Some(watcher) => watcher
                .watch(pairs, reach)
                .into_iter()
                .map(|(id, _)| id)
                .collect(),
            None => HashSet::new(),
        };

        let back = unwatchable
            .difference(&failed)
            .filter(|id| ids.contains(id));
        for id in back {
            self.mark_changed(*id, now);
        }
        let away = ids
            .iter()
            .filter(|id| first && !failed.contains(id))
            .copied()
            .collect();
        *unwatchable = failed;
        away
    }

    /// Whether a pair may record its cursors now: no change waits for a
    /// run, its start-up replay is done, its folders are watched (by a
    /// watcher that runs), no recording is under way, and it has not
    /// recorded them within `CURSOR_SAVE`. Records the time.
    fn quiet(&self, id: PairId, now: Instant) -> bool {
        // Without notifications, nothing proves the folders unchanged.
        if self
            .watcher
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none()
        {
            return false;
        }
        if self
            .saving
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&id)
        {
            return false;
        }
        let pending = self
            .changed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&id);
        let catching_up = self
            .catching_up
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&id);
        let unwatched = self
            .unwatchable
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&id);
        if pending || catching_up || unwatched {
            return false;
        }
        let mut saved = self
            .cursor_saved
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if saved
            .get(&id)
            .is_some_and(|at| now.saturating_duration_since(*at) < CURSOR_SAVE)
        {
            return false;
        }
        saved.insert(id, now);
        true
    }

    /// The event the previous tick saw; records this tick's.
    fn swap_tick_event(&self, event: Option<u64>) -> Option<u64> {
        std::mem::replace(
            &mut *self
                .tick_event
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            event,
        )
    }

    fn away_check(&self) -> Arc<AwayCheck> {
        self.away
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    #[cfg(test)]
    pub(crate) fn set_away(
        &self,
        away: impl Fn(&PairConfig, &[Option<WatchCursor>; 2], Option<&Path>) -> bool
        + Send
        + Sync
        + 'static,
    ) {
        *self.away.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(away);
    }

    /// Test hook: no start-up replay is still running.
    #[cfg(test)]
    pub(crate) fn caught_up(&self) -> bool {
        self.catching_up
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
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
    /// The environment is read once per tick so every candidate is
    /// judged against the same moment.
    pub(crate) fn run_due(&self, now: Instant) -> Vec<PairId> {
        // Crash-loop safe mode starts nothing until a person resumes.
        if self.safe_mode.active().is_some() {
            return Vec::new();
        }
        self.run_due_in(now, Environment::now())
    }

    /// `run_due` with the environment given; tests inject one.
    pub(crate) fn run_due_in(&self, now: Instant, environment: Environment) -> Vec<PairId> {
        self.run_due_at(now, environment, local_time(SystemTime::now()))
    }

    /// `run_due_in` with the machine's local wall-clock time given;
    /// `Daily` pairs compare it against their last recorded run start.
    pub(crate) fn run_due_at(
        &self,
        now: Instant,
        environment: Environment,
        local_now: Option<LocalDateTime>,
    ) -> Vec<PairId> {
        let (candidates, held, last_run) = {
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
            let candidates = pairs
                .into_iter()
                .filter(|pair| pair.draft().options().schedule() != Schedule::Manual)
                .filter(|pair| engine.first_synced(pair.id()).unwrap_or(false))
                // After an undo; an unreadable hold counts as held.
                .filter(|pair| !engine.automation_held(pair.id()).unwrap_or(true))
                .collect::<Vec<_>>();
            // The last run start lives in run history, so daily pairs
            // stay correct across agent restarts and sleep.
            let last_run: HashMap<PairId, LocalDateTime> = candidates
                .iter()
                .filter(|pair| matches!(pair.draft().options().schedule(), Schedule::Daily(_)))
                .filter_map(|pair| {
                    let started = engine.history(pair.id(), 1).ok()?.first()?.started;
                    local_time(started).map(|at| (pair.id(), at))
                })
                .collect();
            // Held by Low Power Mode or a costly network: still watched,
            // so changes meanwhile make them due when the hold lifts.
            let held: HashSet<PairId> = candidates
                .iter()
                .filter(|pair| engine.automatic_hold(pair, environment).is_some())
                .map(PairConfig::id)
                .collect();
            (candidates, held, last_run)
        };
        let changing: Vec<_> = candidates
            .iter()
            .filter(|pair| pair.draft().options().schedule() == Schedule::OnChange)
            .cloned()
            .collect();
        let reach = self
            .engine
            .lock()
            .map(|engine| engine.reach())
            .unwrap_or_default();
        let away = self.scheduler.watch(&changing, &reach, now);
        self.catch_up(&changing, &away, now);
        self.poll(&changing, &held, &reach, now);
        self.save_cursors(&changing, &held, now);

        let mut started = Vec::new();
        for pair in candidates {
            if held.contains(&pair.id())
                || !self
                    .scheduler
                    .due(&pair, now, local_now, last_run.get(&pair.id()).copied())
            {
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

    /// Replay, on the blocking pool, the histories of folders nothing
    /// watched before the agent started; a change, or no usable history,
    /// counts as a change at this tick.
    fn catch_up(&self, pairs: &[PairConfig], away: &[PairId], now: Instant) {
        for pair in pairs.iter().filter(|pair| away.contains(&pair.id())) {
            let id = pair.id();
            let (cursors, state) = {
                let engine = self.engine.lock().unwrap_or_else(PoisonError::into_inner);
                (
                    engine.watch_cursors(id).ok().unwrap_or_default(),
                    Some(engine.state_path().to_path_buf()),
                )
            };
            self.scheduler
                .catching_up
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(id);
            let (scheduler, pair, check) = (
                Arc::clone(&self.scheduler),
                pair.clone(),
                self.scheduler.away_check(),
            );
            let replay = move || {
                // A replay that blew up proves nothing: count it as a change.
                let changed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    check(&pair, &cursors, state.as_deref())
                }))
                .unwrap_or(true);
                if changed {
                    scheduler.mark_changed(id, now);
                }
                scheduler
                    .catching_up
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&id);
            };
            if tokio::runtime::Handle::try_current().is_ok() {
                tokio::task::spawn_blocking(replay);
            } else {
                replay();
            }
        }
    }

    /// Record, for quiet pairs, the event the previous tick saw: nothing
    /// before it is left unsynced, so a restart replays from there.
    /// Reading volumes can stall on a share, so it runs on the blocking
    /// pool, and held pairs are skipped like their checks. Systems
    /// without an event history record nothing.
    fn save_cursors(&self, pairs: &[PairConfig], held: &HashSet<PairId>, now: Instant) {
        let Some(event) = self.scheduler.swap_tick_event(foldover_engine::event_now()) else {
            return;
        };
        for pair in pairs {
            let id = pair.id();
            if held.contains(&id)
                || matches!(self.jobs.status(id), SyncStatus::Working { .. })
                || !self.scheduler.quiet(id, now)
            {
                continue;
            }
            self.scheduler
                .saving
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(id);
            let (engine, scheduler, pair) = (
                Arc::clone(&self.engine),
                Arc::clone(&self.scheduler),
                pair.clone(),
            );
            let save = move || {
                let cursors = foldover_engine::quiet_cursors(&pair, event);
                if !cursors.is_empty()
                    && let Ok(mut engine) = engine.lock()
                {
                    let _ = engine.save_watch_cursors(id, &cursors);
                }
                scheduler
                    .saving
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&id);
            };
            if tokio::runtime::Handle::try_current().is_ok() {
                tokio::task::spawn_blocking(save);
            } else {
                save();
            }
        }
    }

    /// Check, on the blocking pool, the folders of changing-triggered
    /// pairs that hear of no other computer's changes. Held pairs are
    /// skipped: a check spends the costly network or battery the hold
    /// saves. So are running ones, whose own writes are not changes.
    fn poll(&self, pairs: &[PairConfig], held: &HashSet<PairId>, reach: &Reach, now: Instant) {
        self.scheduler.keep_polls(pairs);
        for pair in pairs {
            let id = pair.id();
            if held.contains(&id)
                || matches!(self.jobs.status(id), SyncStatus::Working { .. })
                || !self.scheduler.may_poll(pair)
            {
                continue;
            }
            let last_finished = self.last_finished(id);
            let Some((poll, mode, started)) = self.scheduler.take_poll(id, now, last_finished)
            else {
                continue;
            };
            let (service, pair, reach) = (self.clone(), pair.clone(), reach.clone());
            let check = move || service.check_changes(&pair, &reach, poll, mode, started);
            if tokio::runtime::Handle::try_current().is_ok() {
                tokio::task::spawn_blocking(check);
            } else {
                check();
            }
        }
    }

    /// When the pair's last run finished, from its history.
    fn last_finished(&self, id: PairId) -> Option<SystemTime> {
        self.engine
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .history(id, 1)
            .ok()?
            .first()
            .map(|run| run.finished)
    }

    /// One pair's check for changes notifications miss, started at
    /// `started`. A change seen while a run went on may be that run's own
    /// writing, and the run looked at the folders anyway: it counts only
    /// when no run is working or finished since the check began.
    fn check_changes(
        &self,
        pair: &PairConfig,
        reach: &Reach,
        mut poll: ChangePoll,
        mode: PollMode,
        started: SystemTime,
    ) {
        let id = pair.id();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            poll.check(pair, reach, mode)
        }));
        let need = match outcome {
            Ok(Ok(Polled::Changed)) => {
                let working = matches!(self.jobs.status(id), SyncStatus::Working { .. });
                let ran = self
                    .last_finished(id)
                    .is_some_and(|finished| finished >= started);
                if !working && !ran {
                    self.scheduler.mark_changed(id, Instant::now());
                }
                PollNeed::Needed
            }
            Ok(Ok(Polled::Unchanged)) => PollNeed::Needed,
            Ok(Ok(Polled::NotNeeded)) => PollNeed::NotNeeded,
            // An unreachable folder is checked again on the cadence.
            Ok(Err(_)) => PollNeed::Unknown,
            // A panic may have left the poll half updated; start over.
            Err(_) => {
                poll = ChangePoll::default();
                PollNeed::Unknown
            }
        };
        self.scheduler.poll_done(id, pair.revision(), poll, need);
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

    use foldover_types::{
        DailySchedule, LocalRoot, PairDraft, PairOptions, SyncMode, TimeOfDay, Weekday, Weekdays,
    };

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

        scheduler.watch(std::slice::from_ref(&pair), &Reach::default(), at(1));
        assert!(!scheduler.due(&pair, at(10), None, None));

        fs::create_dir(&right).unwrap();
        scheduler.watch(std::slice::from_ref(&pair), &Reach::default(), at(20));
        assert!(!scheduler.due(&pair, at(21), None, None));
        assert!(scheduler.due(&pair, at(20) + QUIET, None, None));
    }

    /// A Sync pair over two folders in `directory`, both present.
    fn changing_pair(directory: &Path, id: u8) -> PairConfig {
        let (left, right) = (directory.join("left"), directory.join("right"));
        fs::create_dir_all(&left).unwrap();
        fs::create_dir_all(&right).unwrap();
        let root =
            |path: &Path| LocalRoot::from_bytes(path.as_os_str().as_bytes().to_vec()).unwrap();
        let options = PairOptions::default().with_schedule(Schedule::OnChange);
        let draft = PairDraft::new("Live".into(), root(&left), root(&right), SyncMode::Sync)
            .unwrap()
            .with_options(options);
        PairConfig::from_stored(
            PairId::from_bytes([id; PairId::BYTE_LEN]).unwrap(),
            1,
            draft,
        )
        .unwrap()
    }

    /// A pair edited onto other folders re-subscribes its watcher
    /// (PLAN.md §4.8): what is watched follows the pair's folders, not
    /// only its id. A stale subscription misses the new folders' changes
    /// and reports the old ones as the pair's.
    #[test]
    fn an_edit_to_a_pairs_folders_resubscribes_its_watcher() {
        use std::sync::mpsc;

        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let pair = changing_pair(first.path(), 9);
        let edited = changing_pair(second.path(), 9);
        assert!(pair.id() == edited.id(), "the edit must keep the pair's id");
        let start = Instant::now();
        let scheduler = Scheduler::new(start);
        let (sender, receiver) = mpsc::channel();
        *scheduler.watcher.lock().unwrap() = Some(
            PairWatcher::new(move |_| {
                let _ = sender.send(());
            })
            .unwrap(),
        );
        let at = |seconds| start + Duration::from_secs(seconds);

        scheduler.watch(std::slice::from_ref(&pair), &Reach::default(), at(1));
        scheduler.watch(std::slice::from_ref(&edited), &Reach::default(), at(2));

        // The pair's new folder reports changes...
        fs::write(second.path().join("right/new"), b"x").unwrap();
        assert!(receiver.recv_timeout(Duration::from_secs(5)).is_ok());
        // ...and its old one is no longer watched. One write reports
        // through several events; wait for a quiet stretch so its
        // stragglers cannot slip past the drain below.
        let quiet_by = Instant::now() + Duration::from_secs(2);
        while Instant::now() < quiet_by && receiver.recv_timeout(Duration::from_millis(250)).is_ok()
        {
        }
        while receiver.try_recv().is_ok() {}
        fs::write(first.path().join("right/old"), b"x").unwrap();
        assert!(receiver.recv_timeout(Duration::from_secs(2)).is_err());
    }

    #[test]
    fn only_the_first_tick_asks_for_a_replay() {
        let directory = tempfile::tempdir().unwrap();
        let pair = changing_pair(directory.path(), 5);
        let start = Instant::now();
        let scheduler = Scheduler::new(start);
        *scheduler.watcher.lock().unwrap() = Some(PairWatcher::new(|_| {}).unwrap());
        let at = |seconds| start + Duration::from_secs(seconds);

        let away = scheduler.watch(std::slice::from_ref(&pair), &Reach::default(), at(1));
        assert_eq!(away, [pair.id()]);
        // Asking is no change by itself; the replay decides.
        assert!(!scheduler.due(&pair, at(1) + QUIET, None, None));

        // A later tick, even one that watches the pair anew, asks nothing.
        assert!(scheduler.watch(&[], &Reach::default(), at(20)).is_empty());
        let again = scheduler.watch(std::slice::from_ref(&pair), &Reach::default(), at(25));
        assert!(again.is_empty());
        assert!(!scheduler.due(&pair, at(60), None, None));
    }

    #[test]
    fn a_pair_records_its_cursors_only_while_quiet() {
        let start = Instant::now();
        let scheduler = Scheduler::new(start);
        let id = PairId::from_bytes([8; PairId::BYTE_LEN]).unwrap();
        let at = |seconds| start + Duration::from_secs(seconds);

        // Review: with no watcher running, nothing proves a quiet folder
        // unchanged.
        assert!(!scheduler.quiet(id, at(0)));
        *scheduler.watcher.lock().unwrap() = Some(PairWatcher::new(|_| {}).unwrap());

        // A change waiting for its run would be skipped by a replay.
        scheduler.mark_changed(id, at(0));
        assert!(!scheduler.quiet(id, at(1)));
        scheduler.started_run(id, at(10));
        // So would one the start-up replay has yet to report.
        scheduler.catching_up.lock().unwrap().insert(id);
        assert!(!scheduler.quiet(id, at(11)));
        scheduler.catching_up.lock().unwrap().remove(&id);

        assert!(scheduler.quiet(id, at(12)));
        assert!(!scheduler.quiet(id, at(13)));
        // One recording at a time, however long it takes.
        scheduler.saving.lock().unwrap().insert(id);
        assert!(!scheduler.quiet(id, at(12) + CURSOR_SAVE));
        scheduler.saving.lock().unwrap().remove(&id);
        assert!(scheduler.quiet(id, at(12) + CURSOR_SAVE));
    }

    #[test]
    fn a_folder_that_never_settles_runs_after_five_minutes() {
        let directory = tempfile::tempdir().unwrap();
        let pair = changing_pair(directory.path(), 6);
        let start = Instant::now();
        let scheduler = Scheduler::new(start);
        let at = |seconds| start + Duration::from_secs(seconds);

        // A change every few seconds keeps the folder from going quiet.
        let mut second = 0;
        while second < CHURN.as_secs() {
            scheduler.mark_changed(pair.id(), at(second));
            assert!(!scheduler.due(&pair, at(second + 1), None, None));
            second += 3;
        }
        scheduler.mark_changed(pair.id(), at(second));
        assert!(scheduler.due(&pair, at(second), None, None));

        // A run starts the count over.
        scheduler.started_run(pair.id(), at(second));
        scheduler.mark_changed(pair.id(), at(second + 1));
        assert!(!scheduler.due(&pair, at(second + 2), None, None));
    }

    #[test]
    fn polls_record_first_check_on_a_cadence_and_after_each_run() {
        let directory = tempfile::tempdir().unwrap();
        let pair = changing_pair(directory.path(), 7);
        let id = pair.id();
        let start = Instant::now();
        let scheduler = Scheduler::new(start);
        let at = |seconds| start + Duration::from_secs(seconds);
        let take = |seconds, finished| {
            assert!(scheduler.may_poll(&pair));
            scheduler
                .take_poll(id, at(seconds), finished)
                .map(|(poll, mode, _)| {
                    scheduler.poll_done(id, pair.revision(), poll, PollNeed::Needed);
                    mode
                })
        };

        assert_eq!(take(0, None), Some(PollMode::Baseline));
        assert_eq!(take(1, None), None);
        assert_eq!(take(POLL.as_secs(), None), Some(PollMode::Compare));
        // A run finished since the last check: its writes are no change.
        // The next check starts after it, as in the agent; macOS's clock
        // can read the same microsecond twice, which would make the run
        // look unfinished at that check too.
        let finished = SystemTime::now();
        let deadline = Instant::now() + Duration::from_secs(5);
        while SystemTime::now() <= finished {
            assert!(Instant::now() < deadline, "the clock stopped or went back");
            std::hint::spin_loop();
        }
        assert_eq!(
            take(POLL.as_secs() + 5, Some(finished)),
            Some(PollMode::Baseline)
        );
        assert_eq!(take(POLL.as_secs() + 10, Some(finished)), None);

        // While a check runs, no other starts.
        assert!(scheduler.may_poll(&pair));
        let (poll, _, _) = scheduler
            .take_poll(id, at(3 * POLL.as_secs()), None)
            .unwrap();
        assert!(!scheduler.may_poll(&pair));
        // Folders notifications cover are not checked again...
        scheduler.poll_done(id, pair.revision(), poll, PollNeed::NotNeeded);
        assert!(!scheduler.may_poll(&pair));
        // ...until the pair's configuration changes.
        let edited =
            PairConfig::from_stored(id, pair.revision() + 1, pair.draft().clone()).unwrap();
        scheduler.keep_polls(std::slice::from_ref(&edited));
        assert!(scheduler.may_poll(&edited));

        // Review: a check of the old folders, still running across the
        // edit, must not land in the fresh poll.
        let (_, mode, _) = scheduler
            .take_poll(id, at(4 * POLL.as_secs()), None)
            .unwrap();
        assert_eq!(mode, PollMode::Baseline);
        let stale = PollNeed::NotNeeded;
        scheduler.poll_done(id, pair.revision(), ChangePoll::default(), stale);
        assert!(!scheduler.may_poll(&edited), "the fresh check still runs");
        scheduler.poll_done(
            id,
            edited.revision(),
            ChangePoll::default(),
            PollNeed::Needed,
        );
        assert!(scheduler.may_poll(&edited));
    }

    #[test]
    fn a_daily_pair_is_due_once_after_its_local_moment() {
        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left");
        let right = directory.path().join("right");
        fs::create_dir(&left).unwrap();
        fs::create_dir(&right).unwrap();
        let root =
            |path: &Path| LocalRoot::from_bytes(path.as_os_str().as_bytes().to_vec()).unwrap();
        let options = PairOptions::default().with_schedule(Schedule::Daily(DailySchedule::new(
            TimeOfDay::from_minutes(18 * 60).unwrap(),
            Weekdays::MONDAY_TO_FRIDAY,
        )));
        let draft = PairDraft::new("Work".into(), root(&left), root(&right), SyncMode::Sync)
            .unwrap()
            .with_options(options);
        let pair =
            PairConfig::from_stored(PairId::from_bytes([4; PairId::BYTE_LEN]).unwrap(), 1, draft)
                .unwrap();
        let start = Instant::now();
        let scheduler = Scheduler::new(start);
        let now = start + Duration::from_secs(30);
        let local = |day, weekday, minute| Some(LocalDateTime::new(2026, 10, day, weekday, minute));

        // Friday 17:00: today's 18:00 is still ahead.
        assert!(!scheduler.due(
            &pair,
            now,
            local(16, Weekday::Friday, 17 * 60),
            local(15, Weekday::Thursday, 18 * 60),
        ));
        // Friday 18:01, last run Thursday 18:00: due once.
        let friday = local(16, Weekday::Friday, 18 * 60 + 1);
        assert!(scheduler.due(&pair, now, friday, local(15, Weekday::Thursday, 18 * 60)));
        // The moment covered by a run at Friday 18:00 stays covered: the
        // same tick sees an equal last start, not a newer moment.
        assert!(!scheduler.due(&pair, now, friday, local(16, Weekday::Friday, 18 * 60)));
        // A run later Friday evening already passed the moment too.
        assert!(!scheduler.due(&pair, now, friday, local(16, Weekday::Friday, 19 * 60)));
        // Saturday: the Friday moment is behind a Friday run either way.
        let saturday = local(17, Weekday::Saturday, 9 * 60);
        assert!(!scheduler.due(&pair, now, saturday, local(16, Weekday::Friday, 18 * 60)));
        assert!(scheduler.due(&pair, now, saturday, local(15, Weekday::Thursday, 18 * 60)));
        // No recorded run: the pair is due once at the next tick.
        assert!(scheduler.due(&pair, now, friday, None));
        // Without local time the pair cannot be due.
        assert!(!scheduler.due(&pair, now, None, None));
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
