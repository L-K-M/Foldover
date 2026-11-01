//! Items of one archived run read ahead of a client paging through them
//! (ADR 0046). A page holds 100 items, but reaching it can mean listing a
//! whole flat folder or opening an SSH connection, so one walk reads many
//! pages' worth and the following pages are answered from memory.
//!
//! ```text
//! after=none   walk ─▶ keeps items 1-1000, answers 1-100
//! after=100    kept ─▶ answers 101-200
//! after=1000   beyond what is kept ─▶ walk ─▶ keeps 1001-2000
//! ```
//!
//! Jobs and put-backs on this Mac change archives, so they drop what is
//! kept. Another computer's changes show once it expires.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use foldover_proto::local::{ARCHIVE_ITEM_LIMIT, ArchiveItem, ArchiveItems};
use foldover_types::{PairId, Side, SyncPath};

/// Items one walk reads. Bounds what the agent keeps, and what a Mac must
/// fit in one peer reply: JSON spends about 4 bytes per path byte, so 1,000
/// paths of 4 KiB stay under the 16 MiB message limit.
pub(super) const READ_AHEAD: usize = 1_000;

/// How long kept items answer, counted from before they were read.
const LIFETIME: Duration = Duration::from_secs(30);

const _: () = assert!(READ_AHEAD >= ARCHIVE_ITEM_LIMIT);

/// Which listing kept items belong to. An edited pair may point elsewhere,
/// hence the revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RunKey {
    pub(super) id: PairId,
    pub(super) revision: u64,
    pub(super) side: Side,
    pub(super) run: String,
}

/// A walk's items after `start`, in path order.
struct Window {
    key: RunKey,
    start: Option<SyncPath>,
    read_at: Instant,
    items: Vec<ArchiveItem>,
    more: bool,
}

impl Window {
    /// The page after `after`, when these items answer it exactly as a
    /// walk would. A short page while more follow is never one of those.
    fn page(&self, after: &SyncPath) -> Option<ArchiveItems> {
        if self.start.as_ref().is_some_and(|start| after < start) {
            return None;
        }
        let first = self
            .items
            .partition_point(|item| item.path.as_slice() <= after.as_bytes());
        let left = &self.items[first..];
        if self.more && left.len() < ARCHIVE_ITEM_LIMIT {
            return None;
        }
        Some(first_page(left, self.more))
    }
}

/// The first page of `items`, of which more follow when `more`.
fn first_page(items: &[ArchiveItem], more: bool) -> ArchiveItems {
    ArchiveItems {
        items: items.iter().take(ARCHIVE_ITEM_LIMIT).cloned().collect(),
        more: more || items.len() > ARCHIVE_ITEM_LIMIT,
    }
}

#[derive(Default)]
struct State {
    /// A single window, so memory stays bounded by `READ_AHEAD` items.
    window: Option<Window>,
    /// Counts `forget`s, so a walk overlapping one keeps nothing.
    generation: u64,
}

/// Proof of when a walk began, taken before it reads anything.
pub(super) struct WalkStart {
    generation: u64,
    at: Instant,
}

#[derive(Default)]
pub(crate) struct ReadAhead {
    state: Mutex<State>,
}

impl ReadAhead {
    /// The page after `after` from the kept items, if they can answer it.
    pub(super) fn page(
        &self,
        key: &RunKey,
        after: &SyncPath,
        now: Instant,
    ) -> Option<ArchiveItems> {
        let mut state = self.state();
        if state
            .window
            .as_ref()
            .is_some_and(|window| now.saturating_duration_since(window.read_at) >= LIFETIME)
        {
            state.window = None;
        }
        state
            .window
            .as_ref()
            .filter(|window| window.key == *key)
            .and_then(|window| window.page(after))
    }

    /// Call before a walk reads the folder.
    pub(super) fn begin(&self, now: Instant) -> WalkStart {
        WalkStart {
            generation: self.state().generation,
            at: now,
        }
    }

    /// Answer a walk's first page and keep its items for the next pages,
    /// unless they may predate a change that a `forget` announced.
    pub(super) fn keep(
        &self,
        walk: WalkStart,
        key: RunKey,
        start: Option<SyncPath>,
        items: Vec<ArchiveItem>,
        more: bool,
    ) -> ArchiveItems {
        let page = first_page(&items, more);
        let mut state = self.state();
        if state.generation == walk.generation {
            state.window = Some(Window {
                key,
                start,
                read_at: walk.at,
                items,
                more,
            });
        }
        page
    }

    /// The pair's archives may have changed: drop what was read of them.
    pub(crate) fn forget(&self, id: PairId) {
        let mut state = self.state();
        state.generation += 1;
        if state
            .window
            .as_ref()
            .is_some_and(|window| window.key.id == id)
        {
            state.window = None;
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(index: usize) -> ArchiveItem {
        ArchiveItem {
            path: format!("i-{index:05}").into_bytes(),
            size: index as u64,
        }
    }

    fn items(indices: std::ops::Range<usize>) -> Vec<ArchiveItem> {
        indices.map(item).collect()
    }

    fn path(index: usize) -> SyncPath {
        SyncPath::try_from(item(index).path).unwrap()
    }

    fn key(id: u8, run: &str) -> RunKey {
        RunKey {
            id: PairId::from_bytes([id; PairId::BYTE_LEN]).unwrap(),
            revision: 1,
            side: Side::Left,
            run: run.to_owned(),
        }
    }

    /// Keep `kept`, read just now after `start`.
    fn kept(
        read_ahead: &ReadAhead,
        start: Option<usize>,
        kept: Vec<ArchiveItem>,
        more: bool,
    ) -> Instant {
        let now = Instant::now();
        let walk = read_ahead.begin(now);
        read_ahead.keep(walk, key(1, "run"), start.map(path), kept, more);
        now
    }

    /// What a walk over all of `everything` answers after `after`.
    fn walked(everything: &[ArchiveItem], after: &SyncPath) -> ArchiveItems {
        let left: Vec<_> = everything
            .iter()
            .filter(|item| item.path.as_slice() > after.as_bytes())
            .cloned()
            .collect();
        first_page(&left, false)
    }

    #[test]
    fn any_answered_page_is_what_a_walk_would_answer() {
        const COUNT: usize = 3 * ARCHIVE_ITEM_LIMIT + 7;
        let everything = items(0..COUNT);
        // One window ends the run; the other stops short of its end, and
        // what follows it stays unread.
        for (start, end) in [(10, COUNT), (10, 2 * ARCHIVE_ITEM_LIMIT + 3)] {
            let read_ahead = ReadAhead::default();
            let more = end < COUNT;
            let now = kept(&read_ahead, Some(start), items(start + 1..end), more);
            let mut answered = 0;
            for cursor in 0..COUNT + 1 {
                let page = read_ahead.page(&key(1, "run"), &path(cursor), now);
                let Some(page) = page else {
                    continue;
                };
                assert_eq!(page, walked(&everything, &path(cursor)), "after {cursor}");
                answered += 1;
            }
            assert!(answered > 0);
        }
    }

    #[test]
    fn a_cursor_before_what_was_read_or_beyond_it_must_walk() {
        let read_ahead = ReadAhead::default();
        let now = kept(
            &read_ahead,
            Some(100),
            items(101..101 + 3 * ARCHIVE_ITEM_LIMIT),
            true,
        );
        let page = |index| read_ahead.page(&key(1, "run"), &path(index), now);

        assert!(page(99).is_none());
        assert!(page(100).is_some());
        // The last full page ahead still answers, saying more follow.
        let last = 100 + 2 * ARCHIVE_ITEM_LIMIT;
        assert_eq!(page(last).unwrap().items.len(), ARCHIVE_ITEM_LIMIT);
        assert!(page(last).unwrap().more);
        // One item short: a short page would look like the run's end.
        assert!(page(last + 1).is_none());
    }

    #[test]
    fn the_end_of_a_run_answers_a_short_or_empty_page() {
        let read_ahead = ReadAhead::default();
        let now = kept(&read_ahead, None, items(0..ARCHIVE_ITEM_LIMIT + 10), false);
        let page = |index| {
            read_ahead
                .page(&key(1, "run"), &path(index), now)
                .expect("the run's end was read")
        };

        let last_full = 9;
        assert_eq!(page(last_full).items.len(), ARCHIVE_ITEM_LIMIT);
        assert!(!page(last_full).more);
        assert_eq!(page(last_full + 1).items.len(), ARCHIVE_ITEM_LIMIT - 1);
        assert!(page(ARCHIVE_ITEM_LIMIT + 9).items.is_empty());
    }

    #[test]
    fn kept_items_answer_only_their_own_run_while_fresh() {
        let read_ahead = ReadAhead::default();
        let now = kept(&read_ahead, None, items(0..500), true);
        let after = path(0);

        assert!(read_ahead.page(&key(1, "run"), &after, now).is_some());
        assert!(read_ahead.page(&key(1, "other"), &after, now).is_none());
        assert!(read_ahead.page(&key(2, "run"), &after, now).is_none());
        let mut edited = key(1, "run");
        edited.revision = 2;
        assert!(read_ahead.page(&edited, &after, now).is_none());
        let mut side = key(1, "run");
        side.side = Side::Right;
        assert!(read_ahead.page(&side, &after, now).is_none());

        let barely = now + LIFETIME - Duration::from_millis(1);
        assert!(read_ahead.page(&key(1, "run"), &after, barely).is_some());
        let expired = now + LIFETIME;
        assert!(read_ahead.page(&key(1, "run"), &after, expired).is_none());
        // Dropped, not just refused: an earlier clock does not revive it.
        assert!(read_ahead.page(&key(1, "run"), &after, now).is_none());
    }

    #[test]
    fn forgetting_drops_only_the_pairs_items() {
        let read_ahead = ReadAhead::default();
        let now = kept(&read_ahead, None, items(0..500), true);
        let after = path(0);

        read_ahead.forget(key(2, "run").id);
        assert!(read_ahead.page(&key(1, "run"), &after, now).is_some());

        read_ahead.forget(key(1, "run").id);
        assert!(read_ahead.page(&key(1, "run"), &after, now).is_none());
    }

    #[test]
    fn a_walk_overlapping_a_change_answers_but_keeps_nothing() {
        let read_ahead = ReadAhead::default();
        let now = Instant::now();
        let walk = read_ahead.begin(now);
        read_ahead.forget(key(1, "run").id);

        let page = read_ahead.keep(walk, key(1, "run"), None, items(0..500), true);

        assert_eq!(page.items.len(), ARCHIVE_ITEM_LIMIT);
        assert!(page.more);
        assert!(read_ahead.page(&key(1, "run"), &path(0), now).is_none());
    }

    #[test]
    fn the_lifetime_counts_from_before_the_walk_read_anything() {
        let read_ahead = ReadAhead::default();
        let began = Instant::now();
        let walk = read_ahead.begin(began);
        read_ahead.keep(walk, key(1, "run"), None, items(0..500), true);

        let after_lifetime = began + LIFETIME;
        assert!(
            read_ahead
                .page(&key(1, "run"), &path(0), after_lifetime)
                .is_none()
        );
    }
}
