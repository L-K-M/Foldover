//! Names in another Unicode normalization (PLAN.md §4.10). SFTP servers
//! match names byte for byte, so Foldover writes new names there composed
//! (NFC), as Linux and Windows expect, and finds an entry by any spelling
//! that composes alike, as macOS volumes do. The folder therefore counts
//! as ignoring normalization, and the engine maps the spelling it reads
//! back to the one the pair knows (ADR 0030).
//!
//! ```text
//! pair path   café (decomposed, from an HFS+ disk)
//! written     café (composed)
//! looked up   as given ──missing──▶ each component as given, composed
//!                                   or decomposed, whichever exists
//! ```
//!
//! A name with a single spelling, such as any ASCII name, is looked up
//! once, as before.

use std::collections::hash_map::Entry;
use std::io;
use std::sync::Arc;

use foldover_net::sftp::{Attributes, Refused, Session};
use foldover_types::SyncPath;
use unicode_normalization::UnicodeNormalization;

use super::SftpEndpoint;
use crate::ScanError;

/// How many folder listings a run may hold before the cache starts
/// over rather than grow without bound.
const LISTED_FOLDERS: usize = 512;

/// One folder's listing as the run's mutations left it.
pub(super) enum FolderView {
    /// What `read_dir` last showed, kept exact by the run's `note_*`.
    Cached(Arc<Vec<Vec<u8>>>),
    /// A `read_dir` is in flight; its snapshot stores only if no
    /// mutation marked the folder meanwhile.
    Fetching,
    /// The run changed the folder while uncached or mid-read, or a
    /// mutation's outcome is unknown: the next listing must refetch.
    Dirty,
}

impl SftpEndpoint {
    /// Run `op` on where `path` is stored: as given, or in another
    /// spelling when that is missing.
    pub(super) fn on_stored<T>(
        &self,
        path: &[u8],
        op: impl Fn(&[u8]) -> io::Result<T>,
    ) -> io::Result<T> {
        let given = self.remote(path);
        match op(&given) {
            Err(error) if error.kind() == io::ErrorKind::NotFound && respellable(path) => {
                match self.respelled(path) {
                    Some(stored) => op(&stored),
                    None => Err(error),
                }
            }
            result => result,
        }
    }

    /// `on_stored` on one session throughout, so a handle `op` opens
    /// stays valid on it.
    pub(super) fn on_stored_with<T>(
        &self,
        session: &mut Session,
        path: &[u8],
        op: impl Fn(&mut Session, &[u8]) -> io::Result<T>,
    ) -> io::Result<T> {
        let given = self.remote(path);
        match op(session, &given) {
            Err(error) if error.kind() == io::ErrorKind::NotFound && respellable(path) => {
                match self.respelled_on(session, path) {
                    Some(stored) => op(session, &stored),
                    None => Err(error),
                }
            }
            result => result,
        }
    }

    /// The entry at `path` and where it is stored, or `None`.
    pub(super) fn lookup(
        &self,
        path: &SyncPath,
    ) -> Result<Option<(Vec<u8>, Attributes)>, ScanError> {
        let found = self.on_stored(path.as_bytes(), |remote| {
            self.session()
                .lstat(remote)
                .map(|attributes| (remote.to_vec(), attributes))
        });
        match found {
            Ok(found) => Ok(Some(found)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(super::at(Some(path), error)),
        }
    }

    /// `stored` in the spelling its folder lists. A server that ignores
    /// normalization, as a Mac's does, finds an entry by either
    /// spelling, so only the listing tells which one the entry has.
    pub(super) fn listed(&self, stored: &[u8]) -> Vec<u8> {
        let (Some(parent), name) = split(stored) else {
            return stored.to_vec();
        };
        if !respellable(name) {
            return stored.to_vec();
        }
        let parent = match parent.is_empty() {
            true => b"/".as_slice(),
            false => parent,
        };
        let Some(names) = self.listing(parent) else {
            return stored.to_vec();
        };
        // Where both spellings are stored, as given wins.
        if names.iter().any(|listed| listed == name) {
            return stored.to_vec();
        }
        let composed = compose(name);
        names
            .iter()
            .find(|listed| compose(listed) == composed)
            .map_or_else(|| stored.to_vec(), |listed| join(parent, listed))
    }

    /// The names `read_dir` last showed for `parent`, or `None` when it
    /// failed to read it. Fetched once and kept exact by the run's
    /// mutations (`note_*`, `forget_*`), so a bulk replace in one
    /// folder pays one listing, not one per respellable name. Views
    /// track this run's own writes; another client's changes stay
    /// unseen until the run ends.
    fn listing(&self, parent: &[u8]) -> Option<Arc<Vec<Vec<u8>>>> {
        {
            let mut listings = self
                .listings
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(FolderView::Cached(names)) = listings.get(parent) {
                return Some(Arc::clone(names));
            }
            // Bounded: past this many folders the map clears rather
            // than hold every listing the run ever read.
            if listings.len() >= LISTED_FOLDERS {
                listings.clear();
            }
            listings.insert(parent.to_vec(), FolderView::Fetching);
        }
        let names: Option<Vec<Vec<u8>>> = self
            .session()
            .read_dir(parent)
            .ok()
            .map(|entries| entries.into_iter().map(|(name, _)| name).collect());
        let Some(names) = names else {
            // A failed read leaves the folder marked Dirty so the next
            // call tries again rather than trusting nothing.
            self.listings
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(parent.to_vec(), FolderView::Dirty);
            return None;
        };
        let mut listings = self
            .listings
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match listings.entry(parent.to_vec()) {
            // Untouched while read_dir ran: the snapshot is exact.
            Entry::Occupied(mut slot) if matches!(slot.get(), FolderView::Fetching) => {
                let names = Arc::new(names);
                *slot.get_mut() = FolderView::Cached(Arc::clone(&names));
                Some(names)
            }
            // Changed meanwhile — serve what was read but keep the
            // Dirty mark so the next call refetches rather than
            // trust a snapshot taken mid-mutation.
            _ => Some(Arc::new(names)),
        }
    }

    /// `added`'s name joined its parent's cached listing. A parent the
    /// run never listed gets a Dirty mark instead, so a `read_dir`
    /// already in flight cannot store a pre-mutation snapshot.
    pub(super) fn note_added(&self, added: &[u8]) {
        let parent = parent_key(added);
        let (_, name) = split(added);
        match self
            .listings
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(parent)
        {
            Entry::Occupied(mut slot) => match slot.get_mut() {
                FolderView::Cached(names) => {
                    let names = Arc::make_mut(names);
                    if !names.iter().any(|listed| listed == name) {
                        names.push(name.to_vec());
                    }
                }
                view => *view = FolderView::Dirty,
            },
            Entry::Vacant(slot) => {
                slot.insert(FolderView::Dirty);
            }
        }
    }

    /// `gone`'s name left its parent's cached listing.
    pub(super) fn note_removed(&self, gone: &[u8]) {
        let parent = parent_key(gone);
        let (_, name) = split(gone);
        match self
            .listings
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(parent)
        {
            Entry::Occupied(mut slot) => match slot.get_mut() {
                FolderView::Cached(names) => Arc::make_mut(names).retain(|listed| listed != name),
                view => *view = FolderView::Dirty,
            },
            Entry::Vacant(slot) => {
                slot.insert(FolderView::Dirty);
            }
        }
    }

    /// `from` moved to `to`: the name left one listing and joined the
    /// other's, and a moved folder keeps its children, so listings
    /// keyed at or below the old path no longer describe it.
    pub(super) fn note_moved(&self, from: &[u8], to: &[u8]) {
        self.note_removed(from);
        self.note_added(to);
        self.mark_under(from);
    }

    /// `remote` itself is gone: its name leaves the parent and its own
    /// cached listing dies with it. For a completed removal.
    pub(super) fn note_gone(&self, remote: &[u8]) {
        self.note_removed(remote);
        self.mark(remote);
    }

    /// `remote` and everything under it is gone. For a completed
    /// subtree removal.
    pub(super) fn note_gone_tree(&self, remote: &[u8]) {
        self.note_removed(remote);
        self.mark_under(remote);
    }

    /// `remote`'s parent may not hold what the cache claims — the
    /// version for a mutation whose outcome is unknown or half-applied,
    /// where a surgical name edit could lie about an entry still
    /// present.
    pub(super) fn forget_parent(&self, remote: &[u8]) {
        self.mark(&parent_key(remote));
    }

    /// Every view `remote`'s removal could have touched: its parent's,
    /// its own and every folder's below it. For a removal that may have
    /// stopped partway.
    pub(super) fn forget_tree(&self, remote: &[u8]) {
        self.forget_parent(remote);
        self.mark_under(remote);
    }

    /// `remote`'s own listing view may not match the server.
    fn mark(&self, remote: &[u8]) {
        self.listings
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(remote.to_vec(), FolderView::Dirty);
    }

    /// Every listing view keyed below `remote` may not match.
    fn mark_under(&self, remote: &[u8]) {
        let below = [remote, b"/"].concat();
        for (folder, view) in self
            .listings
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter_mut()
        {
            if folder.as_slice() == remote || folder.starts_with(&below) {
                *view = FolderView::Dirty;
            }
        }
    }

    /// Whether `error` is the server's own answer — authoritative —
    /// rather than a lost response whose request may have applied.
    pub(super) fn answered(error: &io::Error) -> bool {
        error.get_ref().is_some_and(|inner| inner.is::<Refused>())
    }

    /// A rename attempt whose request may never have landed: everything
    /// it could have moved or left behind stops being trusted —
    /// `forget_tree` drops each entry's parent's view, its own and
    /// every folder's below it.
    fn rename_lost(&self, from: &[u8], to: &[u8]) {
        self.forget_tree(from);
        self.forget_tree(to);
    }

    /// Rename `from` to `to` over `session`, keeping the run's folder
    /// views exact. A refused rename changed nothing; a lost answer
    /// may have applied.
    pub(super) fn renamed_on(
        &self,
        session: &mut Session,
        from: &[u8],
        to: &[u8],
    ) -> io::Result<()> {
        let moved = session.rename(from, to);
        match &moved {
            Ok(()) => self.note_moved(from, to),
            Err(error) if Self::answered(error) => {}
            Err(_) => self.rename_lost(from, to),
        }
        moved
    }

    /// `renamed_on` on a pooled session.
    pub(super) fn renamed(&self, from: &[u8], to: &[u8]) -> io::Result<()> {
        self.renamed_on(&mut self.session(), from, to)
    }

    /// Rename over the target if it exists, likewise tracked; a finished
    /// replace also drops the replaced tree's views.
    pub(super) fn rename_replacing_on(
        &self,
        session: &mut Session,
        from: &[u8],
        to: &[u8],
    ) -> io::Result<()> {
        let moved = session.rename_replacing(from, to);
        match &moved {
            Ok(()) => {
                self.note_moved(from, to);
                // `to`'s old tree was replaced wholesale.
                self.mark_under(to);
            }
            Err(error) if Self::answered(error) => {}
            Err(_) => self.rename_lost(from, to),
        }
        moved
    }

    /// `rename_replacing_on` on a pooled session.
    #[cfg(test)]
    pub(super) fn rename_replacing(&self, from: &[u8], to: &[u8]) -> io::Result<()> {
        self.rename_replacing_on(&mut self.session(), from, to)
    }

    /// Remove the file `remote` over `session`: its name leaves the
    /// parent's listing, a confirmed refusal leaves views exact, and
    /// a lost answer drops the parent's view since the entry may be
    /// gone after all.
    pub(super) fn removed_file_on(&self, session: &mut Session, remote: &[u8]) -> io::Result<()> {
        let removed = session.remove_file(remote);
        match &removed {
            Ok(()) => self.note_removed(remote),
            // The server confirmed the entry absent.
            Err(error) if error.kind() == io::ErrorKind::NotFound && Self::answered(error) => {
                self.note_removed(remote)
            }
            // The server refused: the entry stays, views stay exact.
            Err(error) if Self::answered(error) => {}
            Err(_) => self.forget_parent(remote),
        }
        removed
    }

    /// `removed_file_on` on a pooled session.
    pub(super) fn removed_file(&self, remote: &[u8]) -> io::Result<()> {
        self.removed_file_on(&mut self.session(), remote)
    }

    /// Folder path `path` as folders made for it are named: components
    /// that exist in the spelling stored, the rest composed, so a missing
    /// folder never gets a twin of one stored in the other spelling.
    pub(super) fn creatable(&self, path: &[u8]) -> Vec<u8> {
        if !respellable(path) {
            return path.to_vec();
        }
        let mut session = self.session();
        let mut stored: Vec<u8> = Vec::new();
        let mut missing = false;
        for component in path.split(|byte| *byte == b'/') {
            let within = |name: &[u8]| match stored.is_empty() {
                true => name.to_vec(),
                false => [stored.as_slice(), b"/", name].concat(),
            };
            let existing = match missing {
                true => None,
                false => spellings(component)
                    .into_iter()
                    .map(|spelling| within(&spelling))
                    .find(|candidate| session.lstat(&self.remote(candidate)).is_ok()),
            };
            stored = match existing {
                Some(existing) => existing,
                None => {
                    missing = true;
                    within(&compose(component))
                }
            };
        }
        stored
    }

    /// Where a new entry at `path` goes: in its parent as stored, under
    /// its composed name.
    pub(super) fn fresh(&self, path: &[u8]) -> Vec<u8> {
        let (parent, name) = split(path);
        join(&self.directory(parent), &compose(name))
    }

    /// A new temporary's server path in `path`'s parent as stored.
    pub(super) fn beside(&self, path: &[u8]) -> Vec<u8> {
        let (parent, _) = split(path);
        join(&self.directory(parent), &super::temporary_name())
    }

    /// The folder `parent` as stored; the root for `None`. A folder
    /// missing in every spelling keeps the spelling given.
    fn directory(&self, parent: Option<&[u8]>) -> Vec<u8> {
        let Some(parent) = parent else {
            return self.root.clone();
        };
        if !respellable(parent) {
            return self.remote(parent);
        }
        self.on_stored(parent, |remote| {
            self.session().lstat(remote).map(|_| remote.to_vec())
        })
        .unwrap_or_else(|_| self.remote(parent))
    }

    /// `path` with each component in the spelling that exists, or `None`
    /// when one exists in none or the result is `path` itself.
    fn respelled(&self, path: &[u8]) -> Option<Vec<u8>> {
        self.respelled_on(&mut self.session(), path)
    }

    fn respelled_on(&self, session: &mut Session, path: &[u8]) -> Option<Vec<u8>> {
        let mut stored = self.root.clone();
        for component in path.split(|byte| *byte == b'/') {
            stored = spellings(component)
                .into_iter()
                .map(|spelling| join(&stored, &spelling))
                .find(|candidate| session.lstat(candidate).is_ok())?;
        }
        (stored != self.remote(path)).then_some(stored)
    }
}

/// Whether `path` has a spelling in another normalization.
fn respellable(path: &[u8]) -> bool {
    if path.is_ascii() {
        return false;
    }
    let Ok(text) = std::str::from_utf8(path) else {
        return false;
    };
    !unicode_normalization::is_nfc(text) || !unicode_normalization::is_nfd(text)
}

/// `name` composed; bytes that are not UTF-8 stay as they are.
pub(super) fn compose(name: &[u8]) -> Vec<u8> {
    match std::str::from_utf8(name) {
        Ok(text) => text.nfc().collect::<String>().into_bytes(),
        Err(_) => name.to_vec(),
    }
}

/// `component` as given, composed and decomposed, without repeats.
fn spellings(component: &[u8]) -> Vec<Vec<u8>> {
    let mut spellings = vec![component.to_vec()];
    if let Ok(text) = std::str::from_utf8(component) {
        for spelling in [
            text.nfc().collect::<String>(),
            text.nfd().collect::<String>(),
        ] {
            if !spellings.contains(&spelling.as_bytes().to_vec()) {
                spellings.push(spelling.into_bytes());
            }
        }
    }
    spellings
}

/// `remote`'s parent's cache key: the parent, or `/` for a bare name —
/// `listed` caches the root listing under `/` too, so a root-level
/// rename must dirty that key rather than skip it.
fn parent_key(remote: &[u8]) -> Vec<u8> {
    let (parent, _) = split(remote);
    match parent {
        Some(parent) if !parent.is_empty() => parent.to_vec(),
        _ => b"/".to_vec(),
    }
}

fn split(path: &[u8]) -> (Option<&[u8]>, &[u8]) {
    match path.iter().rposition(|byte| *byte == b'/') {
        Some(slash) => (Some(&path[..slash]), &path[slash + 1..]),
        None => (None, path),
    }
}

/// `name` inside the server folder `directory`.
fn join(directory: &[u8], name: &[u8]) -> Vec<u8> {
    if directory.ends_with(b"/") {
        return [directory, name].concat();
    }
    [directory, b"/".as_slice(), name].concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_names_with_another_normalization_are_respelled() {
        assert!(!respellable(b"docs/report.txt"));
        assert!(!respellable(b"\xff"));
        assert!(respellable("caf\u{e9}".as_bytes()));
        assert!(respellable("cafe\u{301}".as_bytes()));
        // A character without a decomposition has one spelling.
        assert!(!respellable("\u{4e2d}".as_bytes()));
        assert_eq!(compose("cafe\u{301}".as_bytes()), "caf\u{e9}".as_bytes());
        assert_eq!(
            spellings("cafe\u{301}".as_bytes()),
            ["cafe\u{301}".as_bytes(), "caf\u{e9}".as_bytes()]
        );
        assert_eq!(join(b"/", b"a"), b"/a");
        assert_eq!(join(b"/srv", b"a"), b"/srv/a");
    }
}
