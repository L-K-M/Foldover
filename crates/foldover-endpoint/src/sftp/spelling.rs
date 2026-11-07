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
//!
//! A wide folder's listing is indexed, so `listed_on` and the `note_*`
//! mutations stay proportional to the names they touch rather than to
//! the whole folder: an exact map answers as given, a composed map
//! answers through normalization, and both preserve the server's
//! listing order for aliases that compose alike. The cache itself is
//! bounded by folder count and by a conservative byte estimate.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use foldover_net::sftp::{Attributes, Refused, Session};
use foldover_types::SyncPath;
use unicode_normalization::UnicodeNormalization;

use super::SftpEndpoint;
use crate::ScanError;

/// How many folder listings a run may hold before the cache starts
/// over rather than grow without bound.
const LISTED_FOLDERS: usize = 512;

/// How many bytes of cached folder indexes a run may hold before the
/// cache starts over rather than grow without bound. The estimate
/// charges index and key overhead, not just the raw names, so the real
/// cache stays under it.
const LISTING_BYTES: usize = 32 * 1024 * 1024;

/// Bytes charged per slot, live or tombstoned: the slot itself with
/// `Vec` growth slack.
const SLOT_BYTES: usize = 2 * size_of::<Option<Box<[u8]>>>();

/// Bytes charged per live name beyond its raw bytes and composed key:
/// its exact-map entry, its composed-map entry, the ordinal vector,
/// hash-table slack and allocator slack, rounded up so the estimate
/// stays conservative.
const NAME_OVERHEAD: usize = size_of::<(Box<[u8]>, usize)>()
    + size_of::<(Box<[u8]>, Vec<usize>)>()
    + 4 * size_of::<usize>()
    + 2 * size_of::<usize>()
    + 4 * size_of::<usize>();

/// Rebuild once tombstones outweigh live names, so a folder that loses
/// many entries cannot grow its slot vector without bound.
const COMPACT_SLOTS: usize = 64;

/// One folder's listing as the run's mutations left it.
pub(super) enum FolderView {
    /// What `read_dir` last showed, kept exact by the run's `note_*`.
    /// Behind a mutex so a mutation edits it in place instead of
    /// cloning the whole listing to shadow it.
    Cached(Arc<Mutex<IndexedNames>>),
    /// A `read_dir` is in flight; its snapshot stores only if no
    /// mutation marked the folder meanwhile.
    Fetching,
    /// The run changed the folder while uncached or mid-read, or a
    /// mutation's outcome is unknown: the next listing must refetch.
    Dirty,
}

/// A folder's names, indexed for the two lookups `listed_on` makes.
///
/// `slots` keeps the server's listing order and then the run's
/// `note_added` order. Removals leave a tombstone rather than shift the
/// vector, so the candidates that compose alike keep their relative
/// order and the first listed one still answers. `exact` maps raw bytes
/// to a slot for the as-given lookup; `nfc` maps composed bytes to live
/// slots, in order, for the normalization lookup.
pub(super) struct IndexedNames {
    slots: Vec<Option<Box<[u8]>>>,
    exact: HashMap<Box<[u8]>, usize>,
    nfc: HashMap<Box<[u8]>, Vec<usize>>,
    /// Conservative running byte estimate, for `LISTING_BYTES`.
    bytes: usize,
}

impl IndexedNames {
    fn new() -> Self {
        Self {
            slots: Vec::new(),
            exact: HashMap::new(),
            nfc: HashMap::new(),
            bytes: 0,
        }
    }

    /// A listing's names, in the order the server gave them.
    fn from_listing(names: Vec<Vec<u8>>) -> Self {
        let mut index = Self::new();
        for name in names {
            index.add(&name);
        }
        index
    }

    /// The conservative byte estimate this index charges the run.
    fn bytes(&self) -> usize {
        self.bytes
    }

    /// The listed name for `name`: exactly `name` when the folder holds
    /// it, else the first listed name that composes alike.
    fn listed_name(&self, name: &[u8]) -> Option<Box<[u8]>> {
        if let Some(&slot) = self.exact.get(name) {
            return self.slots[slot].clone();
        }
        let composed = compose(name).into_boxed_slice();
        self.nfc
            .get(composed.as_ref())
            .and_then(|slots| slots.iter().find_map(|&slot| self.slots[slot].as_ref()))
            .cloned()
    }

    /// Add `name` in listing order. A name already listed keeps the
    /// position the server gave it.
    fn add(&mut self, name: &[u8]) {
        if self.exact.contains_key(name) {
            return;
        }
        let composed = compose(name).into_boxed_slice();
        self.bytes += SLOT_BYTES + NAME_OVERHEAD + 2 * name.len() + composed.len();
        let slot = self.slots.len();
        self.slots.push(Some(name.into()));
        self.exact.insert(name.into(), slot);
        self.nfc.entry(composed).or_default().push(slot);
    }

    /// Remove `name`'s slot, leaving a tombstone that keeps the order
    /// of the names still listed.
    fn remove(&mut self, name: &[u8]) {
        let Some(slot) = self.exact.remove(name) else {
            return;
        };
        let composed = compose(name).into_boxed_slice();
        let empty = match self.nfc.get_mut(composed.as_ref()) {
            Some(slots) => {
                slots.retain(|&ordinal| ordinal != slot);
                slots.is_empty()
            }
            None => false,
        };
        if empty {
            self.nfc.remove(composed.as_ref());
        }
        self.slots[slot] = None;
        self.bytes = self
            .bytes
            .saturating_sub(NAME_OVERHEAD + 2 * name.len() + composed.len());
        if self.slots.len() > COMPACT_SLOTS && self.exact.len() * 2 < self.slots.len() {
            self.compact();
        }
    }

    /// Drop tombstones, keeping live names in listing order.
    fn compact(&mut self) {
        let mut slots = Vec::with_capacity(self.exact.len());
        let mut exact = HashMap::with_capacity(self.exact.len());
        let mut nfc: HashMap<Box<[u8]>, Vec<usize>> = HashMap::new();
        let mut bytes = 0;
        for listed in self.slots.drain(..).flatten() {
            let composed = compose(&listed).into_boxed_slice();
            bytes += SLOT_BYTES + NAME_OVERHEAD + 2 * listed.len() + composed.len();
            let slot = slots.len();
            exact.insert(listed.clone(), slot);
            nfc.entry(composed).or_default().push(slot);
            slots.push(Some(listed));
        }
        self.slots = slots;
        self.exact = exact;
        self.nfc = nfc;
        self.bytes = bytes;
    }
}

/// The run's folder views, bounded by folder count and by bytes.
pub(super) struct Listings {
    folders: HashMap<Vec<u8>, FolderView>,
    /// Conservative total of the cached indexes' bytes.
    bytes: usize,
}

impl Default for Listings {
    fn default() -> Self {
        Self::new()
    }
}

impl Listings {
    pub(super) fn new() -> Self {
        Self {
            folders: HashMap::new(),
            bytes: 0,
        }
    }

    /// Every view goes: a run past a bound starts over rather than
    /// hold every listing it ever read.
    fn clear(&mut self) {
        self.folders.clear();
        self.bytes = 0;
    }

    fn len(&self) -> usize {
        self.folders.len()
    }

    /// The cached index at `parent`, if the folder is cached.
    fn cached(&self, parent: &[u8]) -> Option<Arc<Mutex<IndexedNames>>> {
        match self.folders.get(parent) {
            Some(FolderView::Cached(index)) => Some(Arc::clone(index)),
            _ => None,
        }
    }

    /// Install `view` at `parent`, refunding the cached index it
    /// replaces and charging the new one.
    fn store(&mut self, parent: Vec<u8>, view: FolderView) {
        self.bytes = self.bytes.saturating_sub(
            self.folders
                .get(&parent)
                .and_then(FolderView::bytes)
                .unwrap_or(0),
        );
        if let Some(bytes) = FolderView::bytes(&view) {
            self.bytes = self.bytes.saturating_add(bytes);
        }
        self.folders.insert(parent, view);
    }

    /// Cache `index` at `parent`, starting over when `bound` would be
    /// exceeded. A single folder larger than the bound is still cached:
    /// its listing is already in hand, and refusing it would only add
    /// `read_dir`s without bounding the folder itself.
    fn store_cached(&mut self, parent: Vec<u8>, index: &Arc<Mutex<IndexedNames>>, bound: usize) {
        let bytes = index
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .bytes();
        if self.bytes.saturating_add(bytes) > bound {
            self.clear();
        }
        self.store(parent, FolderView::Cached(Arc::clone(index)));
    }
}

#[cfg(test)]
impl Listings {
    /// Mark `parent` as a read in flight, as `listing_on` does.
    pub(super) fn fetching(&mut self, parent: &[u8]) {
        self.folders.insert(parent.to_vec(), FolderView::Fetching);
    }

    /// Whether `parent`'s view is Dirty.
    pub(super) fn is_dirty(&self, parent: &[u8]) -> bool {
        matches!(self.folders.get(parent), Some(FolderView::Dirty))
    }
}

impl FolderView {
    /// The cached index's conservative bytes, if cached.
    fn bytes(&self) -> Option<usize> {
        match self {
            FolderView::Cached(index) => Some(
                index
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .bytes(),
            ),
            _ => None,
        }
    }
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
        self.lookup_on(&mut self.session(), path)
    }

    /// `lookup` without borrowing another pooled session.
    pub(super) fn lookup_on(
        &self,
        session: &mut Session,
        path: &SyncPath,
    ) -> Result<Option<(Vec<u8>, Attributes)>, ScanError> {
        let found = self.on_stored_with(session, path.as_bytes(), |session, remote| {
            session
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
    #[cfg(test)]
    pub(super) fn listed(&self, stored: &[u8]) -> Vec<u8> {
        self.listed_on(&mut self.session(), stored)
    }

    /// `listed` without borrowing another pooled session.
    pub(super) fn listed_on(&self, session: &mut Session, stored: &[u8]) -> Vec<u8> {
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
        let Some(index) = self.listing_on(session, parent) else {
            return stored.to_vec();
        };
        // A short folder lock: it reads the index and nothing else.
        // The global listings lock was released before this, so a
        // mutation and a lookup never hold the two locks in opposite
        // orders.
        let listed = index
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .listed_name(name);
        // Where both spellings are stored, as given wins.
        match listed {
            Some(listed) if listed.as_ref() == name => stored.to_vec(),
            Some(listed) => join(parent, &listed),
            None => stored.to_vec(),
        }
    }

    /// The names `read_dir` last showed for `parent`, or `None` when it
    /// failed to read it. Fetched once and kept exact by the run's
    /// mutations (`note_*`, `forget_*`), so a bulk replace in one
    /// folder pays one listing, not one per respellable name. Views
    /// track this run's own writes; another client's changes stay
    /// unseen until the run ends.
    fn listing_on(&self, session: &mut Session, parent: &[u8]) -> Option<Arc<Mutex<IndexedNames>>> {
        {
            let mut listings = self
                .listings
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(index) = listings.cached(parent) {
                return Some(index);
            }
            // Bounded: past this many folders the map clears rather
            // than hold every listing the run ever read.
            if listings.len() >= LISTED_FOLDERS {
                listings.clear();
            }
            listings
                .folders
                .insert(parent.to_vec(), FolderView::Fetching);
        }
        // Remote I/O without either lock held.
        let names: Option<Vec<Vec<u8>>> = session
            .read_dir(parent)
            .ok()
            .map(|entries| entries.into_iter().map(|(name, _)| name).collect());
        let Some(names) = names else {
            // A failed read leaves the folder marked Dirty so the next
            // call tries again rather than trusting nothing.
            self.listings
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .store(parent.to_vec(), FolderView::Dirty);
            return None;
        };
        let index = Arc::new(Mutex::new(IndexedNames::from_listing(names)));
        let mut listings = self
            .listings
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Untouched while read_dir ran: the snapshot is exact. Changed
        // meanwhile — serve what was read but keep the Dirty mark so
        // the next call refetches rather than trust a snapshot taken
        // mid-mutation.
        if matches!(listings.folders.get(parent), Some(FolderView::Fetching)) {
            listings.store_cached(parent.to_vec(), &index, LISTING_BYTES);
        }
        Some(index)
    }

    /// `added`'s name joined its parent's cached listing. A parent the
    /// run never listed gets a Dirty mark instead, so a `read_dir`
    /// already in flight cannot store a pre-mutation snapshot.
    pub(super) fn note_added(&self, added: &[u8]) {
        let parent = parent_key(added);
        let (_, name) = split(added);
        let mut listings = self
            .listings
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match listings.folders.get_mut(&parent) {
            Some(FolderView::Cached(index)) => {
                let mut index = index.lock().unwrap_or_else(|error| error.into_inner());
                let before = index.bytes();
                index.add(name);
                let growth = index.bytes().saturating_sub(before);
                drop(index);
                listings.bytes = listings.bytes.saturating_add(growth);
            }
            // A read in flight, or a view already untrustworthy: the
            // mutation leaves the folder Dirty.
            Some(view) => *view = FolderView::Dirty,
            None => {
                listings.folders.insert(parent, FolderView::Dirty);
            }
        }
    }

    /// `gone`'s name left its parent's cached listing.
    pub(super) fn note_removed(&self, gone: &[u8]) {
        let parent = parent_key(gone);
        let (_, name) = split(gone);
        let mut listings = self
            .listings
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match listings.folders.get_mut(&parent) {
            Some(FolderView::Cached(index)) => {
                let mut index = index.lock().unwrap_or_else(|error| error.into_inner());
                let before = index.bytes();
                index.remove(name);
                let shrink = before.saturating_sub(index.bytes());
                drop(index);
                listings.bytes = listings.bytes.saturating_sub(shrink);
            }
            Some(view) => *view = FolderView::Dirty,
            None => {
                listings.folders.insert(parent, FolderView::Dirty);
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
            .store(remote.to_vec(), FolderView::Dirty);
    }

    /// Every listing view keyed below `remote` may not match.
    fn mark_under(&self, remote: &[u8]) {
        // A `remote` ending in `/` (the root) already prefixes its
        // children — appending another slash would match none of them.
        let below = match remote.ends_with(b"/") {
            true => remote.to_vec(),
            false => [remote, b"/"].concat(),
        };
        let mut listings = self
            .listings
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut refund: usize = 0;
        for (folder, view) in listings.folders.iter_mut() {
            if folder.as_slice() == remote || folder.starts_with(&below) {
                refund = refund.saturating_add(FolderView::bytes(view).unwrap_or(0));
                *view = FolderView::Dirty;
            }
        }
        listings.bytes = listings.bytes.saturating_sub(refund);
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
        self.creatable_on(&mut self.session(), path)
    }

    /// `creatable` without borrowing another pooled session.
    pub(super) fn creatable_on(&self, session: &mut Session, path: &[u8]) -> Vec<u8> {
        if !respellable(path) {
            return path.to_vec();
        }
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
        self.fresh_on(&mut self.session(), path)
    }

    pub(super) fn fresh_on(&self, session: &mut Session, path: &[u8]) -> Vec<u8> {
        let (parent, name) = split(path);
        join(&self.directory_on(session, parent), &compose(name))
    }

    /// A new temporary's server path in `path`'s parent as stored.
    pub(super) fn beside_on(&self, session: &mut Session, path: &[u8]) -> Vec<u8> {
        let (parent, _) = split(path);
        join(
            &self.directory_on(session, parent),
            &super::temporary_name(),
        )
    }

    /// The folder `parent` as stored; the root for `None`. A folder
    /// missing in every spelling keeps the spelling given.
    fn directory_on(&self, session: &mut Session, parent: Option<&[u8]>) -> Vec<u8> {
        let Some(parent) = parent else {
            return self.root.clone();
        };
        if !respellable(parent) {
            return self.remote(parent);
        }
        self.on_stored_with(session, parent, |session, remote| {
            session.lstat(remote).map(|_| remote.to_vec())
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
    #[cfg(test)]
    compose_calls::tick();
    match std::str::from_utf8(name) {
        Ok(text) => text.nfc().collect::<String>().into_bytes(),
        Err(_) => name.to_vec(),
    }
}

/// Counts `compose` calls in tests, to show a lookup's complexity.
/// Test-only: the counter and its thread local do not exist in a
/// production build.
#[cfg(test)]
mod compose_calls {
    use std::cell::Cell;

    thread_local! {
        static CALLS: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn tick() {
        CALLS.with(|calls| calls.set(calls.get() + 1));
    }

    pub(super) fn reset() {
        CALLS.with(|calls| calls.set(0));
    }

    pub(super) fn count() -> usize {
        CALLS.with(|calls| calls.get())
    }
}

/// Reset the test-only `compose` counter, so a test can measure one
/// lookup. Test-only, like the counter itself.
#[cfg(test)]
pub(super) fn reset_compose_calls() {
    compose_calls::reset();
}

/// How many times `compose` has run on this thread.
#[cfg(test)]
pub(super) fn compose_call_count() -> usize {
    compose_calls::count()
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

    /// The scan `listed_on` made before the index, kept so a test can
    /// show the complexity the index removes.
    fn linear_listed_name(names: &[Vec<u8>], name: &[u8]) -> Option<Box<[u8]>> {
        if names.iter().any(|listed| listed == name) {
            return Some(name.into());
        }
        let composed = compose(name);
        names
            .iter()
            .find(|listed| compose(listed) == composed)
            .map(|listed| listed.as_slice().into())
    }

    #[test]
    fn an_indexed_lookup_normalizes_at_most_twice() {
        const CACHED: usize = 10_000;
        let mut listing = Vec::with_capacity(CACHED);
        for file in 0..CACHED - 1 {
            listing.push(format!("file-{file:05}.txt").into_bytes());
        }
        // The match is last, so the old scan composes every name before
        // it finds one.
        listing.push("caf\u{e9}.txt".as_bytes().to_vec());
        let index = IndexedNames::from_listing(listing.clone());
        let asked = "cafe\u{301}.txt".as_bytes();

        // The old scan composes once per cached name before the match.
        compose_calls::reset();
        let scanned = linear_listed_name(&listing, asked);
        assert_eq!(scanned.as_deref(), Some("caf\u{e9}.txt".as_bytes()));
        assert!(
            compose_calls::count() > 2,
            "the linear scan composed {} names",
            compose_calls::count()
        );

        // The index composes only the asked name.
        compose_calls::reset();
        let found = index.listed_name(asked);
        assert_eq!(found.as_deref(), Some("caf\u{e9}.txt".as_bytes()));
        assert!(
            compose_calls::count() <= 2,
            "the index composed {} names",
            compose_calls::count()
        );
    }

    #[test]
    fn exact_spelling_wins_over_a_normalized_alias() {
        let index = IndexedNames::from_listing(vec![
            "cafe\u{301}.txt".as_bytes().to_vec(),
            "caf\u{e9}.txt".as_bytes().to_vec(),
        ]);
        let asked = "caf\u{e9}.txt".as_bytes();
        assert_eq!(index.listed_name(asked).as_deref(), Some(asked));
    }

    #[test]
    fn ambiguous_aliases_keep_their_listing_order() {
        // Two spellings that compose alike but are neither the composed
        // form, so the composed ask cannot be answered exactly.
        let (first, second) = ("a\u{301}\u{323}", "a\u{323}\u{301}");
        // The composed form of both: `a` with a dot below, then acute.
        let composed = "\u{1ea1}\u{301}";
        assert_eq!(compose(first.as_bytes()), composed.as_bytes());
        assert_eq!(compose(second.as_bytes()), composed.as_bytes());
        let mut index =
            IndexedNames::from_listing(vec![first.as_bytes().to_vec(), second.as_bytes().to_vec()]);
        // The first listed candidate answers.
        assert_eq!(
            index.listed_name(composed.as_bytes()).as_deref(),
            Some(first.as_bytes())
        );
        // Removing it leaves the second, still in order.
        index.remove(first.as_bytes());
        assert_eq!(
            index.listed_name(composed.as_bytes()).as_deref(),
            Some(second.as_bytes())
        );
        // The composed form is its own exact match once listed.
        index.add(composed.as_bytes());
        assert_eq!(
            index.listed_name(composed.as_bytes()).as_deref(),
            Some(composed.as_bytes())
        );
    }

    #[test]
    fn names_that_are_not_utf8_stay_as_they_are() {
        let raw: &[u8] = b"caf\xff";
        let index = IndexedNames::from_listing(vec![raw.to_vec(), b"plain".to_vec()]);
        // Exact bytes answer exactly.
        assert_eq!(index.listed_name(raw).as_deref(), Some(raw));
        // A composed name never matches bytes that are not UTF-8.
        assert_eq!(index.listed_name("caf\u{e9}".as_bytes()), None);
    }

    #[test]
    fn concurrent_mutations_keep_the_index_consistent() {
        use std::thread;

        const THREADS: usize = 4;
        const STEPS: usize = 200;
        let index = Arc::new(Mutex::new(IndexedNames::new()));
        let mut threads = Vec::new();
        for thread in 0..THREADS {
            let index = Arc::clone(&index);
            threads.push(thread::spawn(move || {
                for step in 0..STEPS {
                    let name = format!("t{thread}-{step:03}");
                    let mut index = index.lock().unwrap();
                    index.add(name.as_bytes());
                    assert_eq!(
                        index.listed_name(name.as_bytes()).as_deref(),
                        Some(name.as_bytes())
                    );
                    if step % 3 == 0 {
                        index.remove(name.as_bytes());
                        assert_eq!(index.listed_name(name.as_bytes()), None);
                    }
                }
            }));
        }
        let reader = {
            let index = Arc::clone(&index);
            thread::spawn(move || {
                for _ in 0..2_000 {
                    let index = index.lock().unwrap();
                    let _ = index.listed_name(b"t1-001");
                }
            })
        };
        for thread in threads {
            thread.join().unwrap();
        }
        reader.join().unwrap();
        // Every name this run did not remove resolves; every removed one
        // does not.
        let index = index.lock().unwrap();
        for thread in 0..THREADS {
            for step in 0..STEPS {
                let name = format!("t{thread}-{step:03}");
                let found = index.listed_name(name.as_bytes());
                if step % 3 == 0 {
                    assert_eq!(found, None, "{name} should be gone");
                } else {
                    assert_eq!(found.as_deref(), Some(name.as_bytes()), "{name}");
                }
            }
        }
    }

    #[test]
    fn a_cached_folder_over_the_byte_bound_starts_over() {
        let index = Arc::new(Mutex::new(IndexedNames::from_listing(vec![
            b"first".to_vec(),
            b"second".to_vec(),
        ])));
        let mut listings = Listings::new();
        // A tiny bound stands in for `LISTING_BYTES`: the real one is
        // impractical to fill in a unit test.
        listings.store_cached(b"/one".to_vec(), &index, 1);
        assert_eq!(listings.len(), 1);
        assert!(matches!(
            listings.folders.get(b"/one".as_slice()),
            Some(FolderView::Cached(_))
        ));
        // The next folder does not fit, so the cache starts over and
        // holds only the new one.
        listings.store_cached(b"/two".to_vec(), &index, 1);
        assert_eq!(listings.len(), 1);
        assert!(matches!(
            listings.folders.get(b"/two".as_slice()),
            Some(FolderView::Cached(_))
        ));
        assert!(!listings.folders.contains_key(b"/one".as_slice()));
    }

    #[test]
    fn byte_accounting_matches_the_index() {
        let mut index = IndexedNames::new();
        let mut expected = 0usize;
        let mut live = 0usize;
        let mut tombstones = 0usize;
        for step in 0..300 {
            let name = format!("caf\u{e9}-{step:03}");
            let composed = compose(name.as_bytes());
            expected += SLOT_BYTES + NAME_OVERHEAD + 2 * name.len() + composed.len();
            live += 1;
            index.add(name.as_bytes());
            assert_eq!(index.bytes(), expected, "after adding {name}");
            if step % 2 == 0 {
                expected -= NAME_OVERHEAD + 2 * name.len() + composed.len();
                live -= 1;
                tombstones += 1;
                if live + tombstones > COMPACT_SLOTS && live * 2 < live + tombstones {
                    expected -= tombstones * SLOT_BYTES;
                    tombstones = 0;
                }
                index.remove(name.as_bytes());
                assert_eq!(index.bytes(), expected, "after removing {name}");
            }
        }
    }

    #[test]
    fn a_replaced_cached_view_refunds_its_bytes() {
        let index = Arc::new(Mutex::new(IndexedNames::from_listing(vec![b"a".to_vec()])));
        let mut listings = Listings::new();
        listings.store(b"/one".to_vec(), FolderView::Cached(Arc::clone(&index)));
        let charged = listings.bytes;
        assert!(charged > 0);
        listings.store(b"/one".to_vec(), FolderView::Dirty);
        assert_eq!(listings.bytes, 0);
        listings.store(b"/one".to_vec(), FolderView::Cached(Arc::clone(&index)));
        assert_eq!(listings.bytes, charged);
    }
}
