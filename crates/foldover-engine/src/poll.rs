//! Quick checks for changes no notification reports (PLAN.md §4.8):
//! another computer's edits on a share or an SFTP server. A check lists
//! a folder's root and its recently active directories, one level each,
//! and compares names, kinds, sizes and modification times with the
//! previous check. Changes deeper down wait for the hourly verification
//! run.
//!
//! ```text
//! check 1   /                  photos/ modified 10:00    first look
//! check 2   /                  photos/ modified 10:04    changed; photos/
//!                                                        becomes active
//! check 3   /, photos/         photos/a.jpg grew         changed
//! ```

use std::collections::{BTreeMap, VecDeque};

use foldover_endpoint::{Endpoint, Filter, ListedEntry, ListedKind, Reach, ScanError};
use foldover_types::{PairConfig, PairRoot, Side, SyncPath};

use crate::Error;

/// Directories listed besides the root, per folder, so a check stays
/// cheap on a slow server.
const ACTIVE_LIMIT: usize = 32;

/// Whether a check reports differences or only records what it saw.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PollMode {
    /// Report a difference from the previous check.
    Compare,
    /// Record the folders as they are, e.g. after a run changed them.
    Baseline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Polled {
    Changed,
    Unchanged,
    /// Notifications cover both folders; nothing needs polling.
    NotNeeded,
}

/// One pair's polling state, kept by the caller between checks.
#[derive(Default)]
pub struct ChangePoll {
    left: FolderPoll,
    right: FolderPoll,
}

impl ChangePoll {
    /// Look at the pair's folders that send no notifications for other
    /// computers' changes: SFTP folders and network shares. A local disk
    /// is watched, and a paired Mac reports its own changes.
    pub fn check(
        &mut self,
        pair: &PairConfig,
        reach: &Reach,
        mode: PollMode,
    ) -> Result<Polled, Error> {
        let draft = pair.draft();
        let rules = draft.options().filters().rules();
        let mut polled = Polled::NotNeeded;
        for side in [Side::Left, Side::Right] {
            let (root, folder) = match side {
                Side::Left => (draft.left(), &mut self.left),
                Side::Right => (draft.right(), &mut self.right),
            };
            let endpoint = match root {
                PairRoot::Peer(_) => continue,
                PairRoot::Sftp(_) => Endpoint::open(root, reach)?,
                PairRoot::Local(_) => {
                    let endpoint = Endpoint::open(root, reach)?;
                    if endpoint.network_source()?.is_none() {
                        continue;
                    }
                    endpoint
                }
            };
            let changed = folder.check(&endpoint, &folder_filter(rules, &endpoint)?)?;
            polled = match (polled, changed) {
                (Polled::Changed, _) | (_, true) => Polled::Changed,
                _ => Polled::Unchanged,
            };
        }
        Ok(match (polled, mode) {
            (Polled::Changed, PollMode::Baseline) => Polled::Unchanged,
            (polled, _) => polled,
        })
    }
}

/// The pair's rules and the folder's own `.foldoverignore`, so what a run
/// leaves out never counts as a change.
fn folder_filter(rules: &[String], endpoint: &Endpoint) -> Result<Filter, Error> {
    let ignored = endpoint.ignore_rules()?;
    Ok(Filter::new(
        rules.iter().chain(&ignored).map(String::as_str),
    )?)
}

/// What one folder looked like at the last check.
#[derive(Default)]
struct FolderPoll {
    /// Each listed directory's entries by name; `None` is the root.
    seen: BTreeMap<Option<SyncPath>, Vec<ListedEntry>>,
    /// Directories listed besides the root, most recently active first.
    active: VecDeque<SyncPath>,
}

impl FolderPoll {
    /// List the root and the active directories; whether anything
    /// differs from the last check. A directory whose entry changed is
    /// where someone is working, so it is listed from now on, starting
    /// with this check so its first listing is current.
    fn check(&mut self, endpoint: &Endpoint, filter: &Filter) -> Result<bool, ScanError> {
        let mut pending: VecDeque<Option<SyncPath>> = std::iter::once(None)
            .chain(self.active.iter().cloned().map(Some))
            .collect();
        let mut seen = BTreeMap::new();
        let mut newly_active = Vec::new();
        let mut changed = false;
        while let Some(directory) = pending.pop_front() {
            let listing = match (endpoint.list(directory.as_ref(), filter), &directory) {
                (Ok(Some(listing)), _) => listing,
                (Ok(None), None) => return Err(ScanError::RootMissing),
                (Err(error), None) => return Err(error),
                // An active directory that is gone is a change for the run
                // to sort out.
                (Ok(None), Some(_)) => {
                    changed = true;
                    continue;
                }
                // One that failed to answer keeps its last listing, so a
                // server's hiccup starts no run and the next check still
                // compares against what was there.
                (Err(_), Some(_)) => {
                    if let Some(before) = self.seen.get(&directory) {
                        seen.insert(directory, before.clone());
                    }
                    continue;
                }
            };
            let mut listing = listing;
            listing.sort_by(|a, b| a.name().cmp(b.name()));
            if let Some(before) = self.seen.get(&directory) {
                // A directory whose own entries changed stays recently active.
                if *before != listing {
                    changed = true;
                    newly_active.extend(directory.clone());
                }
                for entry in &listing {
                    if entry.kind() != ListedKind::Directory || before.contains(entry) {
                        continue;
                    }
                    let Ok(path) = child(directory.as_ref(), entry.name()) else {
                        continue;
                    };
                    let listed = seen.contains_key(&Some(path.clone()))
                        || pending.contains(&Some(path.clone()));
                    if !listed && make_room(&mut self.active, &mut seen, &mut pending, &directory) {
                        pending.push_back(Some(path.clone()));
                    }
                    newly_active.push(path);
                }
            }
            seen.insert(directory, listing);
        }

        self.active
            .retain(|path| seen.contains_key(&Some(path.clone())));
        for path in newly_active.into_iter().rev() {
            if !seen.contains_key(&Some(path.clone())) {
                continue;
            }
            self.active.retain(|active| *active != path);
            self.active.push_front(path);
        }
        self.active.truncate(ACTIVE_LIMIT);
        seen.retain(|directory, _| {
            directory
                .as_ref()
                .is_none_or(|directory| self.active.contains(directory))
        });
        self.seen = seen;
        Ok(changed)
    }
}

/// Make room to list one more directory this check: the least recently
/// active ones, other than `current`, stop being watched. Whether there
/// is room.
fn make_room(
    active: &mut VecDeque<SyncPath>,
    seen: &mut BTreeMap<Option<SyncPath>, Vec<ListedEntry>>,
    pending: &mut VecDeque<Option<SyncPath>>,
    current: &Option<SyncPath>,
) -> bool {
    while seen.len() + pending.len() > ACTIVE_LIMIT {
        let Some(oldest) = active
            .iter()
            .rposition(|path| Some(path) != current.as_ref())
            .and_then(|oldest| active.remove(oldest))
        else {
            return false;
        };
        seen.remove(&Some(oldest.clone()));
        pending.retain(|queued| queued.as_ref() != Some(&oldest));
    }
    true
}

fn child(directory: Option<&SyncPath>, name: &[u8]) -> Result<SyncPath, ScanError> {
    let mut bytes = directory.map_or_else(Vec::new, |directory| {
        let mut bytes = directory.as_bytes().to_vec();
        bytes.push(b'/');
        bytes
    });
    bytes.extend_from_slice(name);
    Ok(SyncPath::try_from(bytes)?)
}

#[cfg(test)]
mod tests;
