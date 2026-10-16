//! `s1-ffi` — the UniFFI surface spike S1 measures (PLAN.md section
//! 8): an async call in (`echo_after`), a high-rate event stream out
//! (`start_stream` pushing batches to a Swift `EventSink`), packaged
//! the way `foldover-ffi` is (proc-macro exports, staticlib +
//! cdylib).
//!
//! UniFFI's scaffolding expands to `unsafe extern "C"` shims, so this
//! crate cannot forbid unsafe code; it holds no hand-written
//! `unsafe`.

use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::sync::Notify;

/// What a Foldover change event looks like on the wire: a sequence
/// number, the affected path and the change kind.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Event {
    pub seq: u64,
    pub path: String,
    pub kind: EventKind,
}

/// The change kinds the stream emits, cycling deterministically so
/// every enum case crosses the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum EventKind {
    Created,
    Modified,
    Deleted,
    Renamed,
}

impl Event {
    /// The synthetic event for `seq`: a plausible-looking path so the
    /// marshaling cost includes a real per-event string.
    fn numbered(seq: u64) -> Self {
        const KINDS: [EventKind; 4] = [
            EventKind::Created,
            EventKind::Modified,
            EventKind::Deleted,
            EventKind::Renamed,
        ];
        Event {
            seq,
            path: format!("/bench/roots/pair-a/dir-{:03}/file-{seq:09}.dat", seq % 257),
            kind: KINDS[seq as usize % KINDS.len()],
        }
    }
}

/// The foreign trait Swift implements — the event-stream half of the
/// bridge. `foreign` only: no Rust implementation exists; the stream
/// thread calls `on_batch`. Synchronous on purpose: UniFFI's async
/// foreign callbacks are the part of the boundary whose Swift 6
/// support is unproven, so Foldover's shape batches instead (the
/// same trade-off the PLAN.md section 8 fallback names).
#[uniffi::export(foreign)]
pub trait EventSink: Send + Sync {
    /// One batch of events, invoked from the stream thread. `seq` is
    /// strictly increasing within and across batches.
    fn on_batch(&self, events: Vec<Event>);
}

/// `echoAfter` in Swift: the async-call-in half of the bridge, run
/// on tokio through UniFFI's `Compat` wrapper so `tokio::time` works.
#[uniffi::export(async_runtime = "tokio")]
pub async fn echo_after(delay_ms: u64, text: String) -> String {
    if delay_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
    }
    text
}

/// The shared state the stream thread writes and `wait` reads, plus
/// the `Notify` that resolves the waiter once the thread exits.
struct StreamState {
    cancelled: AtomicBool,
    done: AtomicBool,
    delivered_events: AtomicU64,
    delivered_batches: AtomicU64,
    done_notify: Notify,
}

impl StreamState {
    fn new() -> Self {
        StreamState {
            cancelled: AtomicBool::new(false),
            done: AtomicBool::new(false),
            delivered_events: AtomicU64::new(0),
            delivered_batches: AtomicU64::new(0),
            done_notify: Notify::new(),
        }
    }

    /// The report as it stands — called only after `done` is set.
    fn report(&self) -> StreamReport {
        StreamReport {
            delivered_events: self.delivered_events.load(Ordering::Acquire),
            delivered_batches: self.delivered_batches.load(Ordering::Acquire),
            cancelled: self.cancelled.load(Ordering::Acquire),
        }
    }
}

/// What `wait` reports when the stream thread exits.
#[derive(Debug, Clone, uniffi::Record)]
pub struct StreamReport {
    /// Events the sink was handed.
    pub delivered_events: u64,
    /// `on_batch` calls the sink answered.
    pub delivered_batches: u64,
    /// `cancel` ran at some point (the stream may still have finished
    /// first; compare `delivered_events` against `total`).
    pub cancelled: bool,
}

/// `startStream` in Swift: spawn the thread that pushes `total`
/// events to `sink` in batches of `batch_size`, then returns the
/// handle the thread reports through. A `batch_size` of 0 is clamped
/// to 1.
#[uniffi::export]
pub fn start_stream(
    sink: Arc<dyn EventSink>,
    total: u64,
    batch_size: u32,
) -> Arc<StreamHandle> {
    let state = Arc::new(StreamState::new());
    spawn_stream(Arc::clone(&state), move |batch| sink.on_batch(batch), total, batch_size);
    Arc::new(StreamHandle { state })
}

/// The object the stream thread reports through.
#[derive(uniffi::Object)]
pub struct StreamHandle {
    state: Arc<StreamState>,
}

// The impl-level `async_runtime` wraps the async `wait` in UniFFI's
// `Compat` tokio shim; synchronous methods (`cancel`) ignore it.
#[uniffi::export(async_runtime = "tokio")]
impl StreamHandle {
    /// Stop the stream after the current batch. Synchronous and in
    /// memory — the thread checks the flag between batches.
    pub fn cancel(&self) {
        self.state.cancelled.store(true, Ordering::SeqCst);
    }

    /// Resolve when the stream thread exits: all `total` events
    /// delivered, or an early `cancel`. Waiters that arrive after
    /// completion get the same report immediately.
    pub async fn wait(&self) -> StreamReport {
        // Enable the wait before checking `done`, so a completion
        // racing the first poll lands either in the flag or in the
        // enabled wait (the same pattern as a condvar check).
        let mut notified = pin!(self.state.done_notify.notified());
        notified.as_mut().enable();
        if !self.state.done.load(Ordering::Acquire) {
            notified.await;
        }
        self.state.report()
    }
}

/// The stream thread: batches of `Event::numbered`, pushed through
/// `emit`, counted into `state`, `done` flagged at the end. Generic
/// over `emit` so unit tests drive it without the FFI trait.
fn spawn_stream<E>(state: Arc<StreamState>, emit: E, total: u64, batch_size: u32)
where
    E: Fn(Vec<Event>) + Send + 'static,
{
    let batch_size = u64::from(batch_size).max(1);
    std::thread::Builder::new()
        .name("s1-stream".to_owned())
        .spawn(move || {
            let mut seq = 0;
            while seq < total && !state.cancelled.load(Ordering::Relaxed) {
                let count = batch_size.min(total - seq);
                let batch: Vec<Event> = (0..count)
                    .map(|offset| Event::numbered(seq + offset))
                    .collect();
                emit(batch);
                state.delivered_events.fetch_add(count, Ordering::Relaxed);
                state.delivered_batches.fetch_add(1, Ordering::Relaxed);
                seq += count;
            }
            state.done.store(true, Ordering::Release);
            state.done_notify.notify_waiters();
        })
        .expect("the stream thread must spawn for the stream to run");
}

uniffi::setup_scaffolding!();

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime builds")
    }

    /// A full stream delivers every event, in order, in ceil(total /
    /// batch_size) batches, and `wait` reports the same totals.
    #[test]
    fn full_stream_delivers_every_event_in_order() {
        let state = Arc::new(StreamState::new());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        spawn_stream(
            Arc::clone(&state),
            move |batch| sink.lock().expect("seen").extend(batch),
            1_000,
            64,
        );

        let handle = StreamHandle { state };
        let report = runtime().block_on(handle.wait());

        assert!(!report.cancelled);
        assert_eq!(report.delivered_events, 1_000);
        assert_eq!(report.delivered_batches, 16, "ceil(1000 / 64)");
        let events = seen.lock().expect("seen");
        assert_eq!(events.len(), 1_000);
        assert!(
            events.iter().enumerate().all(|(i, e)| e.seq == i as u64),
            "events are strictly seq-ordered"
        );
    }

    /// A stream cancelled before its first batch emits nothing and
    /// reports `cancelled`.
    #[test]
    fn a_pre_cancelled_stream_delivers_nothing() {
        let state = Arc::new(StreamState::new());
        state.cancelled.store(true, Ordering::SeqCst);
        spawn_stream(
            Arc::clone(&state),
            |_| unreachable!("a cancelled stream emits no batches"),
            1_000_000,
            64,
        );

        let report = runtime().block_on(StreamHandle { state }.wait());
        assert!(report.cancelled);
        assert_eq!(report.delivered_events, 0);
        assert_eq!(report.delivered_batches, 0);
    }

    /// `wait` after completion returns the same totals — the report
    /// is a recorded state, not a one-shot channel.
    #[test]
    fn wait_twice_reports_the_same_totals() {
        let state = Arc::new(StreamState::new());
        spawn_stream(Arc::clone(&state), |_| {}, 10, 4);
        let handle = StreamHandle { state };
        let runtime = runtime();

        let first = runtime.block_on(handle.wait());
        let second = runtime.block_on(handle.wait());
        assert_eq!(first.delivered_events, 10);
        assert_eq!(first.delivered_batches, 3);
        assert_eq!(second.delivered_events, first.delivered_events);
    }

    /// `echo_after` sleeps its delay on tokio and returns the text.
    #[test]
    fn echo_after_sleeps_then_echoes() {
        let runtime = runtime();
        let start = std::time::Instant::now();
        let reply = runtime.block_on(echo_after(20, "ping".to_owned()));
        assert_eq!(reply, "ping");
        assert!(start.elapsed() >= std::time::Duration::from_millis(20));
    }

    /// A zero `batch_size` is clamped, not honored: the stream still
    /// delivers everything, one event per batch.
    #[test]
    fn zero_batch_size_is_clamped_to_one() {
        let state = Arc::new(StreamState::new());
        spawn_stream(Arc::clone(&state), |_| {}, 3, 0);
        let report = runtime().block_on(StreamHandle { state }.wait());
        assert_eq!(report.delivered_events, 3);
        assert_eq!(report.delivered_batches, 3);
    }
}
