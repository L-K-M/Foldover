//! Changes made while nothing watched a pair's folders (ADR 0002). While
//! a pair is quiet, the agent records for each local folder the file
//! event before which nothing was left unsynced. When the agent starts,
//! the folder's volume history is replayed from there. A folder without
//! a usable record, or one only a scan can check (SFTP, another Mac, a
//! volume that keeps no history), may have changed.
//!
//! ```text
//! quiet at event 900 ──▶ cursor (root, volume history ID, 900)
//! agent stops; someone edits a file; agent starts
//! replay root's history from 900 ──▶ the edit ──▶ changed: run
//! ```

use std::time::Duration;

use foldover_endpoint::{changed_since_event, event_history};
use foldover_state::WatchCursor;
use foldover_types::{PairConfig, PairRoot, Side};

/// How long a replay may take before its folder counts as changed.
const REPLAY_TIMEOUT: Duration = Duration::from_secs(30);

/// Cursors at `event` for `pair`'s local folders on volumes that keep an
/// event history. Reads each folder's volume, so call it off locks: a
/// share can be slow to answer.
pub fn quiet_cursors(pair: &PairConfig, event: u64) -> Vec<(Side, WatchCursor)> {
    [Side::Left, Side::Right]
        .into_iter()
        .filter_map(|side| {
            let PairRoot::Local(root) = root(pair, side) else {
                return None;
            };
            let history = event_history(root).ok()??;
            Some((
                side,
                WatchCursor {
                    root: root.clone(),
                    history,
                    event,
                },
            ))
        })
        .collect()
}

/// Whether `pair`'s folders may have changed since `cursors` were saved.
/// Replays volume histories, which can take seconds; call it off locks.
pub fn changed_while_away(pair: &PairConfig, cursors: &[Option<WatchCursor>; 2]) -> bool {
    [Side::Left, Side::Right].into_iter().any(|side| {
        let PairRoot::Local(root) = root(pair, side) else {
            return true;
        };
        let cursor = match side {
            Side::Left => &cursors[0],
            Side::Right => &cursors[1],
        };
        let Some(cursor) = cursor.as_ref().filter(|cursor| cursor.root == *root) else {
            return true;
        };
        // A reset history no longer reaches back to the cursor.
        if !matches!(event_history(root), Ok(Some(history)) if history == cursor.history) {
            return true;
        }
        changed_since_event(root, cursor.event, REPLAY_TIMEOUT).unwrap_or(true)
    })
}

fn root(pair: &PairConfig, side: Side) -> &PairRoot {
    match side {
        Side::Left => pair.draft().left(),
        Side::Right => pair.draft().right(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use foldover_types::{LocalRoot, PairDraft, PairId, SyncMode};

    use super::*;

    fn pair(directory: &Path) -> PairConfig {
        let (left, right) = (directory.join("left"), directory.join("right"));
        fs::create_dir_all(&left).unwrap();
        fs::create_dir_all(&right).unwrap();
        let root = |path: &Path| {
            LocalRoot::from_bytes(path.as_os_str().as_encoded_bytes().to_vec()).unwrap()
        };
        let draft =
            PairDraft::new("Disk".into(), root(&left), root(&right), SyncMode::Sync).unwrap();
        PairConfig::from_stored(PairId::from_bytes([1; PairId::BYTE_LEN]).unwrap(), 1, draft)
            .unwrap()
    }

    #[test]
    fn a_folder_without_a_usable_cursor_may_have_changed() {
        let directory = tempfile::tempdir().unwrap();
        let pair = pair(directory.path());
        assert!(changed_while_away(&pair, &[None, None]));

        // A cursor taken for another folder says nothing about this one.
        let other = WatchCursor {
            root: LocalRoot::from_bytes(b"/elsewhere".to_vec()).unwrap(),
            history: "history".into(),
            event: 1,
        };
        assert!(changed_while_away(
            &pair,
            &[Some(other.clone()), Some(other)]
        ));
    }

    /// Volumes keep an event history only on macOS; elsewhere nothing is
    /// recorded and every folder may have changed.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn without_an_event_history_nothing_is_recorded() {
        let directory = tempfile::tempdir().unwrap();
        assert!(quiet_cursors(&pair(directory.path()), 7).is_empty());
    }

    /// The whole round trip on a volume with an event history.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_quiet_folder_replays_as_unchanged_until_it_changes() {
        // Long enough for the history to record recent events (ADR 0002).
        const SETTLE: Duration = Duration::from_secs(3);
        let directory = tempfile::tempdir().unwrap();
        let pair = pair(directory.path());
        std::thread::sleep(SETTLE);
        let event = foldover_endpoint::event_now().unwrap();
        let saved = quiet_cursors(&pair, event);
        assert_eq!(saved.len(), 2);
        let cursors = [Some(saved[0].1.clone()), Some(saved[1].1.clone())];

        // Foldover's own bookkeeping is no change.
        fs::create_dir(directory.path().join("left/.foldover")).unwrap();
        std::thread::sleep(SETTLE);
        assert!(!changed_while_away(&pair, &cursors));

        fs::write(directory.path().join("right/note.txt"), b"edited").unwrap();
        std::thread::sleep(SETTLE);
        assert!(changed_while_away(&pair, &cursors));
    }
}
