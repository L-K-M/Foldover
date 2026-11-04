//! Each root keeps replaced and removed entries in
//! `.foldover/archive/<run>/<path>`. `<run>` is the run's UTC start time
//! and the pair that ran it, e.g.
//! `2026-10-18T09-30-00.123Z_0123456789abcdef0123456789abcdef`, so
//! retention can delete whole runs by age without reading their contents,
//! and pairs that share a folder, on one Mac or several, each list and
//! prune their own runs (ADR 0047). Runs from before the pair was named
//! carry the time alone and belong to every pair.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use foldover_fs::{DirectoryScope, NodeKind, WriteError};
use foldover_types::{Durability, PairId, SyncPath};

use super::{ApplyError, LocalEndpoint, ScanError, child_path, protected};

/// Names tried for one put-back: the original, then `(restored)`,
/// `(restored 2)` and so on.
const PUT_BACK_ATTEMPTS: u32 = 100;
const RESTORED: &str = "restored";
/// The longest file name APFS and most Unix filesystems store, in bytes.
const NAME_MAX: usize = 255;

/// One run's folder in a root's archive.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchivedRun {
    name: String,
    started: SystemTime,
    owner: Option<PairId>,
}

impl ArchivedRun {
    /// The run named `name`, which another Mac may have read; the pair
    /// comes from the name.
    pub(crate) fn new(name: String, started: SystemTime) -> Self {
        let owner = parse_run(name.as_bytes()).and_then(|(_, owner)| owner);
        Self {
            name,
            started,
            owner,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn started(&self) -> SystemTime {
        self.started
    }

    /// The pair that ran it; `None` for a run from before runs were
    /// named after their pair.
    pub fn owner(&self) -> Option<PairId> {
        self.owner
    }
}

/// Whose expired runs a prune removes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pruning {
    /// A run of this pair prunes by its own clock. Its runs, and runs not
    /// named after a pair, go at the retention. Other pairs' runs go only
    /// at `OTHER_PAIRS_RETENTION_FACTOR` times it: a Mac whose clock runs
    /// ahead cannot take another pair's runs early, and the runs of a pair
    /// that no longer exists still go.
    Pair(PairId),
    /// Every run goes at the retention: the Mac sharing a folder prunes it
    /// by its own clock for every Mac that writes it.
    Everyone,
}

/// How many retentions another pair's run is kept by a pair's prune.
const OTHER_PAIRS_RETENTION_FACTOR: u32 = 2;

impl Pruning {
    /// How old a run of `owner` must be before this prune removes it.
    pub(crate) fn expiry(self, owner: Option<PairId>, retention: Duration) -> Duration {
        match (self, owner) {
            (Self::Pair(pair), Some(owner)) if owner != pair => {
                retention.saturating_mul(OTHER_PAIRS_RETENTION_FACTOR)
            }
            _ => retention,
        }
    }
}

/// A file or link a run archived, at its original path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchivedItem {
    path: SyncPath,
    size: u64,
}

impl ArchivedItem {
    pub(crate) fn new(path: SyncPath, size: u64) -> Self {
        Self { path, size }
    }

    pub fn path(&self) -> &SyncPath {
        &self.path
    }

    pub fn size(&self) -> u64 {
        self.size
    }
}

pub(crate) const ARCHIVE_ROOT: &[u8] = b".foldover/archive";
/// Between a run's start time and its pair in the run's folder name.
const OWNER_SEPARATOR: u8 = b'_';
const SECONDS_PER_DAY: u64 = 86_400;
const SECONDS_PER_HOUR: u64 = 3_600;
const SECONDS_PER_MINUTE: u64 = 60;
const MILLIS_PER_SECOND: u64 = 1_000;
/// Days from 0000-03-01 to 1970-01-01 in the proleptic Gregorian calendar.
const EPOCH_DAY_OFFSET: u64 = 719_468;
const DAYS_PER_ERA: u64 = 146_097;

/// A sortable, Finder-safe name of the time alone, as runs had before
/// they were named after their pair; `run_name` adds the pair. Runs of
/// one pair never share a millisecond, because they are serialized.
pub fn archive_name(at: SystemTime) -> String {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = since.as_secs();
    let (year, month, day) = civil_from_days(seconds / SECONDS_PER_DAY);
    let time = seconds % SECONDS_PER_DAY;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}-{:02}-{:02}.{:03}Z",
        time / SECONDS_PER_HOUR,
        time % SECONDS_PER_HOUR / SECONDS_PER_MINUTE,
        time % SECONDS_PER_MINUTE,
        since.subsec_millis()
    )
}

/// The folder name of a run of `pair` that started at `at`: its start
/// time, then the pair, e.g.
/// `2026-10-18T09-30-00.123Z_0123456789abcdef0123456789abcdef`. The time
/// comes first, so names still sort by it.
pub fn run_name(at: SystemTime, pair: PairId) -> String {
    format!("{}{}{pair}", archive_name(at), char::from(OWNER_SEPARATOR))
}

/// The pair a run name carries; `None` for a name of the time alone, or
/// one that names no run.
pub fn run_owner(name: &str) -> Option<PairId> {
    parse_run(name.as_bytes()).and_then(|(_, owner)| owner)
}

/// The start time a run name encodes; `None` for any other name.
pub(crate) fn archive_time(name: &[u8]) -> Option<SystemTime> {
    parse_run(name).map(|(started, _)| started)
}

/// The start time and pair a run name encodes, the pair `None` for a
/// name of the time alone; `None` for any other name.
pub(crate) fn parse_run(name: &[u8]) -> Option<(SystemTime, Option<PairId>)> {
    let (time, owner) = match name.iter().position(|&byte| byte == OWNER_SEPARATOR) {
        None => (name, None),
        Some(separator) => {
            let owner = std::str::from_utf8(&name[separator + 1..]).ok()?;
            (&name[..separator], Some(owner.parse().ok()?))
        }
    };
    Some((start_time(time)?, owner))
}

/// The start time a name of the time alone encodes.
fn start_time(name: &[u8]) -> Option<SystemTime> {
    let text = std::str::from_utf8(name).ok()?;
    let number = |range: std::ops::Range<usize>| -> Option<u64> {
        let digits = text.get(range)?;
        digits
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| digits.parse().ok())?
    };
    let days = days_from_civil(number(0..4)?, number(5..7)?, number(8..10)?)?;
    let seconds = days * SECONDS_PER_DAY
        + number(11..13)? * SECONDS_PER_HOUR
        + number(14..16)? * SECONDS_PER_MINUTE
        + number(17..19)?;
    let time = UNIX_EPOCH + Duration::from_millis(seconds * MILLIS_PER_SECOND + number(20..23)?);
    // Only canonical names count, so nothing else in the archive is deleted.
    (archive_name(time) == text).then_some(time)
}

/// Howard Hinnant's days-to-civil conversion, from days since 1970-01-01.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let shifted = days + EPOCH_DAY_OFFSET;
    let era = shifted / DAYS_PER_ERA;
    let day_of_era = shifted % DAYS_PER_ERA;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    (year_of_era + era * 400 + u64::from(month <= 2), month, day)
}

/// The inverse, for dates from 1970; ranges are checked by the round trip.
fn days_from_civil(year: u64, month: u64, day: u64) -> Option<u64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || year < 1970 {
        return None;
    }
    let year = if month <= 2 { year - 1 } else { year };
    let era = year / 400;
    let year_of_era = year % 400;
    let month_index = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    (era * DAYS_PER_ERA + day_of_era).checked_sub(EPOCH_DAY_OFFSET)
}

fn archive_root() -> SyncPath {
    SyncPath::try_from(ARCHIVE_ROOT).expect("the archive path is valid")
}

/// The run's folder; only canonical run names, so nothing outside the
/// archive can be addressed.
pub(crate) fn run_folder(run: &str) -> Option<SyncPath> {
    archive_time(run.as_bytes())?;
    let mut bytes = ARCHIVE_ROOT.to_vec();
    bytes.push(b'/');
    bytes.extend_from_slice(run.as_bytes());
    SyncPath::try_from(bytes).ok()
}

pub(crate) fn join(parent: &SyncPath, child: &[u8]) -> Result<SyncPath, ScanError> {
    let mut bytes = parent.as_bytes().to_vec();
    bytes.push(b'/');
    bytes.extend_from_slice(child);
    Ok(SyncPath::try_from(bytes)?)
}

/// What one entry of a folder listing is: only files and links become
/// items; folders are walked, never through a link; other kinds are
/// ignored.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Found {
    Folder,
    Item { size: u64 },
    Ignored,
}

/// One entry of a folder listing under its sort key: a folder's key is
/// its name plus `/`, so its whole subtree sorts into one contiguous
/// range right after the key.
struct Entry {
    key: Vec<u8>,
    path: SyncPath,
    found: Found,
}

/// One page of `run`'s items in path order after `after`: at most
/// `limit` items and whether more follow, reading only the folders the
/// page reaches. `list` lists one folder of the run, given the folder's
/// path inside the root.
///
/// Path order is plain byte order over whole relative paths, so
/// everything below folder `a` starts with `a/` and one folder's items
/// are one contiguous range: with the names `a-b`, `a.txt`, `a/x` and
/// `a0`, folder `a`'s contents sort between `a.txt` and `a0` because
/// `-` is 0x2d, `.` 0x2e, `/` 0x2f and `0` 0x30. Sorting one folder's
/// entries by name, with a folder's key being its name plus `/`, and
/// walking depth first therefore yields global path order, so a page
/// stops once it fills and skips the folders before its cursor.
pub(crate) fn archived_page(
    run: &str,
    after: Option<&SyncPath>,
    limit: usize,
    mut list: impl FnMut(&SyncPath) -> Result<Vec<(Vec<u8>, Found)>, ScanError>,
) -> Result<(Vec<ArchivedItem>, bool), ScanError> {
    let folder = run_folder(run).ok_or(ScanError::UnknownArchiveRun)?;
    let mut items = Vec::new();
    // One frame per listed folder; entries come off descending, so the
    // walk visits them in ascending path order.
    let mut stack = vec![listing(None, &folder, after, &mut list)?];
    loop {
        let Some(entry) = stack.last_mut().and_then(Vec::pop) else {
            if stack.pop().is_none() {
                return Ok((items, false));
            }
            continue;
        };
        match entry.found {
            // `listing` drops these; kept for exhaustiveness.
            Found::Ignored => {}
            Found::Folder => stack.push(listing(Some(&entry.path), &folder, after, &mut list)?),
            Found::Item { size } => {
                // The extra item proves more follow; nothing beyond it
                // is read. `limit + 1` is never computed, so
                // `usize::MAX` pages are safe.
                if items.len() == limit {
                    return Ok((items, true));
                }
                items.push(ArchivedItem::new(entry.path, size));
            }
        }
    }
}

/// The page-reachable entries of one folder, sorted descending by key.
fn listing(
    relative: Option<&SyncPath>,
    folder: &SyncPath,
    after: Option<&SyncPath>,
    list: &mut impl FnMut(&SyncPath) -> Result<Vec<(Vec<u8>, Found)>, ScanError>,
) -> Result<Vec<Entry>, ScanError> {
    let directory = match relative {
        None => folder.clone(),
        Some(relative) => join(folder, relative.as_bytes())?,
    };
    let mut entries = Vec::new();
    for (name, found) in list(&directory)? {
        let path = child_path(relative, &name)?;
        let key = match found {
            Found::Folder => [name.as_slice(), b"/"].concat(),
            Found::Item { .. } | Found::Ignored => name,
        };
        entries.push(Entry { key, path, found });
    }
    entries.retain(|entry| match entry.found {
        // Never an item and never walked.
        Found::Ignored => false,
        Found::Item { .. } => after.is_none_or(|after| &entry.path > after),
        // One contiguous range, kept when `after` is before it
        // (`after < "d/"`) or inside it (starts with `d/`); `after` is
        // a client's cursor and may name a folder or no existing path.
        Found::Folder => after.is_none_or(|after| {
            let below = [entry.path.as_bytes(), b"/"].concat();
            after.as_bytes() < below.as_slice() || after.as_bytes().starts_with(&below)
        }),
    });
    // Descending, so popping from the end visits ascending.
    entries.sort_unstable_by(|left, right| right.key.cmp(&left.key));
    Ok(entries)
}

impl LocalEndpoint {
    /// Archived runs, newest first. Other folders in the archive are skipped.
    pub fn archived_runs(&self) -> Result<Vec<ArchivedRun>, ScanError> {
        let archive = archive_root();
        let entries = match self.root.entries(DirectoryScope::At(&archive)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(scan_error(&archive, source)),
        };
        let mut runs = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| scan_error(&archive, source))?;
            // Only a real folder holds archived items; a stray timestamp-named
            // file or link is not a run.
            if entry.metadata().kind() != NodeKind::Directory {
                continue;
            }
            let Some((started, owner)) = parse_run(entry.name()) else {
                continue;
            };
            if let Ok(name) = String::from_utf8(entry.name().to_vec()) {
                runs.push(ArchivedRun {
                    name,
                    started,
                    owner,
                });
            }
        }
        runs.sort_by_key(|run| std::cmp::Reverse(run.started));
        Ok(runs)
    }

    /// Files and links `run` archived, in path order after `after`: at most
    /// `limit`, and whether there are more. Only the folders the page
    /// reaches are listed.
    pub fn archived_items(
        &self,
        run: &str,
        after: Option<&SyncPath>,
        limit: usize,
    ) -> Result<(Vec<ArchivedItem>, bool), ScanError> {
        archived_page(run, after, limit, |directory| {
            let entries = self
                .root
                .entries(DirectoryScope::At(directory))
                .map_err(|source| scan_error(directory, source))?;
            entries
                .map(|entry| {
                    let entry = entry.map_err(|source| scan_error(directory, source))?;
                    let metadata = entry.metadata();
                    let found = match metadata.kind() {
                        NodeKind::Directory => Found::Folder,
                        NodeKind::File | NodeKind::Symlink => Found::Item {
                            size: metadata.size(),
                        },
                        NodeKind::Other => Found::Ignored,
                    };
                    Ok((entry.name().to_vec(), found))
                })
                .collect()
        })
    }

    /// Put an archived item back at its original path, or beside it when
    /// something is there: `docs/report.txt` becomes
    /// `docs/report (restored).txt`. Nothing is replaced; the next sync
    /// carries the item to the other folder. Returns where it went.
    pub fn put_back(
        &self,
        run: &str,
        path: &SyncPath,
        durability: Durability,
    ) -> Result<SyncPath, ApplyError> {
        self.guarded_subtree(path)?;
        if path.components().any(protected) {
            return Err(ApplyError::Unsupported(path.clone()));
        }
        let folder = run_folder(run).ok_or(ScanError::UnknownArchiveRun)?;
        let archived = join(&folder, path.as_bytes())?;
        for attempt in 0..PUT_BACK_ATTEMPTS {
            let destination = match attempt {
                0 => path.clone(),
                attempt => sibling_name(path, RESTORED, attempt)?,
            };
            match self.root.restore(&archived, &destination, durability) {
                Ok(()) => return Ok(destination),
                Err(WriteError::Exists) => continue,
                Err(WriteError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(ApplyError::NotArchived(path.clone()));
                }
                Err(source) => {
                    return Err(ApplyError::Write {
                        path: destination,
                        source,
                    });
                }
            }
        }
        Err(ApplyError::Occupied(path.clone()))
    }

    /// Permanently delete archived runs that started more than their
    /// expiry before `now`: `retention`, or longer for another pair's
    /// runs (`Pruning`). Other folders in the archive are kept. Returns
    /// the number of runs removed.
    pub fn prune_archive(
        &self,
        retention: Duration,
        now: SystemTime,
        pruning: Pruning,
    ) -> Result<u64, ApplyError> {
        let archive = SyncPath::try_from(ARCHIVE_ROOT).expect("the archive path is valid");
        // Data kept from broken-off copies follows the same retention.
        self.root
            .prune_partials(retention, now)
            .map_err(|source| io_error(&archive, source))?;
        let entries = match self.root.entries(DirectoryScope::At(&archive)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(source) => return Err(io_error(&archive, source)),
        };
        let mut expired = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| io_error(&archive, source))?;
            let Some((started, owner)) = parse_run(entry.name()) else {
                continue;
            };
            let expiry = pruning.expiry(owner, retention);
            if now.duration_since(started).is_ok_and(|age| age > expiry) {
                let mut path = archive.as_bytes().to_vec();
                path.push(b'/');
                path.extend_from_slice(entry.name());
                expired.push(SyncPath::try_from(path)?);
            }
        }
        for path in &expired {
            self.root
                .remove_tree(path)
                .map_err(|source| ApplyError::Write {
                    path: path.clone(),
                    source,
                })?;
        }
        Ok(expired.len() as u64)
    }
}

/// The UTC date of `at`, e.g. `2026-10-18`.
pub(crate) fn date_name(at: SystemTime) -> String {
    let seconds = at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let (year, month, day) = civil_from_days(seconds / SECONDS_PER_DAY);
    format!("{year:04}-{month:02}-{day:02}")
}

/// `path` renamed beside itself with `label`: with `restored`,
/// `a/report.txt` becomes `a/report (restored).txt` for attempt 1 and
/// `a/report (restored 2).txt` for attempt 2. Long names lose the end of
/// their stem, never the extension.
pub(crate) fn sibling_name(
    path: &SyncPath,
    label: &str,
    attempt: u32,
) -> Result<SyncPath, ApplyError> {
    let name = path.file_name();
    let split = match name.iter().rposition(|byte| *byte == b'.') {
        Some(dot) if dot > 0 => dot,
        _ => name.len(),
    };
    let (stem, extension) = name.split_at(split);
    let suffix = match attempt {
        1 => format!(" ({label})"),
        attempt => format!(" ({label} {attempt})"),
    };
    let room = NAME_MAX.saturating_sub(suffix.len() + extension.len());
    let stem = &stem[..fitting(stem, room)];

    let mut bytes = match path.parent() {
        Some(parent) => [parent.as_bytes(), b"/"].concat(),
        None => Vec::new(),
    };
    bytes.extend_from_slice(stem);
    bytes.extend_from_slice(suffix.as_bytes());
    bytes.extend_from_slice(extension);
    Ok(SyncPath::try_from(bytes)?)
}

/// The length of `stem`'s longest prefix within `room` bytes that keeps a
/// UTF-8 name valid, which APFS requires.
fn fitting(stem: &[u8], room: usize) -> usize {
    if stem.len() <= room {
        return stem.len();
    }
    match std::str::from_utf8(stem) {
        Ok(text) => text.floor_char_boundary(room),
        Err(_) => room,
    }
}

fn scan_error(path: &SyncPath, source: std::io::Error) -> ScanError {
    ScanError::Io {
        path: Some(path.clone()),
        source,
    }
}

fn io_error(path: &SyncPath, source: std::io::Error) -> ApplyError {
    ApplyError::Scan(ScanError::Io {
        path: Some(path.clone()),
        source,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::fs;

    use proptest::prelude::*;

    use super::*;

    fn at(millis: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(millis)
    }

    /// A tiny in-memory run behind `archived_page`'s closure, with no
    /// filesystem.
    #[derive(Clone, Debug)]
    enum Node {
        Folder(BTreeMap<Vec<u8>, Node>),
        Item(u64),
        Ignored,
    }

    fn folder<T: AsRef<[u8]>>(entries: impl IntoIterator<Item = (T, Node)>) -> Node {
        Node::Folder(
            entries
                .into_iter()
                .map(|(name, node)| (name.as_ref().to_vec(), node))
                .collect(),
        )
    }

    /// The closure listing `tree`, recording the relative folder each
    /// call reads.
    fn in_memory<'a>(
        tree: &'a Node,
        run: &str,
        listed: &'a RefCell<Vec<Vec<u8>>>,
    ) -> impl FnMut(&SyncPath) -> Result<Vec<(Vec<u8>, Found)>, ScanError> + 'a {
        let prefix = format!(".foldover/archive/{run}").into_bytes();
        move |directory| {
            let relative = directory
                .as_bytes()
                .strip_prefix(prefix.as_slice())
                .expect("a listed folder is under the run")
                .strip_prefix(b"/")
                .unwrap_or(b"");
            listed.borrow_mut().push(relative.to_vec());
            let mut node = tree;
            for name in relative
                .split(|byte| *byte == b'/')
                .filter(|name| !name.is_empty())
            {
                let Node::Folder(entries) = node else {
                    return Ok(Vec::new());
                };
                node = entries.get(name).expect("a listed folder exists");
            }
            let Node::Folder(entries) = node else {
                return Ok(Vec::new());
            };
            Ok(entries
                .iter()
                .map(|(name, child)| {
                    let found = match child {
                        Node::Folder(_) => Found::Folder,
                        Node::Item(size) => Found::Item { size: *size },
                        Node::Ignored => Found::Ignored,
                    };
                    (name.clone(), found)
                })
                .collect())
        }
    }

    /// Every item of `tree` in path order: the reference a page must
    /// match (keep `> after`, sort, truncate, `more` when any is left).
    fn naive(tree: &Node) -> Vec<(Vec<u8>, u64)> {
        fn collect(node: &Node, prefix: &[u8], items: &mut Vec<(Vec<u8>, u64)>) {
            let Node::Folder(entries) = node else {
                return;
            };
            for (name, child) in entries {
                let mut path = prefix.to_vec();
                if !path.is_empty() {
                    path.push(b'/');
                }
                path.extend_from_slice(name);
                match child {
                    Node::Folder(_) => collect(child, &path, items),
                    Node::Item(size) => items.push((path, *size)),
                    Node::Ignored => {}
                }
            }
        }
        let mut items = Vec::new();
        collect(tree, b"", &mut items);
        items.sort();
        items
    }

    /// The naive page after `after`, as (path, size) pairs.
    fn naive_page(tree: &Node, after: Option<&[u8]>, limit: usize) -> (Vec<(Vec<u8>, u64)>, bool) {
        let kept: Vec<_> = naive(tree)
            .into_iter()
            .filter(|(path, _)| after.is_none_or(|after| path.as_slice() > after))
            .collect();
        let more = kept.len() > limit;
        let mut kept = kept;
        kept.truncate(limit);
        (kept, more)
    }

    /// The paths of one page, for comparison with byte names.
    fn paths(items: &[ArchivedItem]) -> Vec<Vec<u8>> {
        items
            .iter()
            .map(|item| item.path().as_bytes().to_vec())
            .collect()
    }

    /// A page of the walk over `tree`, as (path, size) pairs.
    fn page(
        tree: &Node,
        run: &str,
        after: Option<&SyncPath>,
        limit: usize,
    ) -> (Vec<(Vec<u8>, u64)>, bool) {
        let listed = RefCell::new(Vec::new());
        let (items, more) =
            archived_page(run, after, limit, in_memory(tree, run, &listed)).unwrap();
        let items = items
            .into_iter()
            .map(|item| (item.path().as_bytes().to_vec(), item.size()))
            .collect();
        (items, more)
    }

    /// The paths a run holds and the paths that never existed but a
    /// client can still name as a cursor.
    fn cursors(tree: &Node) -> Vec<Option<SyncPath>> {
        fn collect(node: &Node, prefix: &[u8], cursors: &mut Vec<Option<Vec<u8>>>) {
            let Node::Folder(entries) = node else {
                return;
            };
            for (name, child) in entries {
                let mut path = prefix.to_vec();
                if !path.is_empty() {
                    path.push(b'/');
                }
                path.extend_from_slice(name);
                cursors.push(Some(path.clone()));
                if matches!(child, Node::Folder(_)) {
                    let mut below = path.clone();
                    below.extend_from_slice(b"/zzz");
                    cursors.push(Some(below.clone()));
                    below.extend_from_slice(b"/yyy");
                    cursors.push(Some(below));
                }
                collect(child, &path, cursors);
            }
        }
        let mut raw = vec![None];
        collect(tree, b"", &mut raw);
        raw.extend([b"a-".as_slice(), b"zzz", b"a/aa", b"z0"].map(|name| Some(name.to_vec())));
        raw.into_iter()
            .map(|path| {
                path.map(|bytes| SyncPath::try_from(bytes).expect("a cursor is a valid path"))
            })
            .collect()
    }

    const NAMES: [&[u8]; 6] = [b"a", b"a-b", b"a.txt", b"a0", b"b", b"z"];

    /// Random runs to depth 3 with names that order around `/`.
    fn nodes(depth: u32) -> impl Strategy<Value = Node> {
        let leaf = prop_oneof![Just(Node::Ignored), (0u64..4).prop_map(Node::Item)];
        if depth == 0 {
            return leaf.boxed();
        }
        prop_oneof![
            leaf,
            proptest::collection::btree_map(
                proptest::sample::select(NAMES.map(Vec::from).to_vec()),
                nodes(depth - 1),
                0..6,
            )
            .prop_map(Node::Folder),
        ]
        .boxed()
    }

    #[test]
    fn a_page_lists_only_the_folders_it_reaches() {
        let run = archive_name(at(1_000));
        let tree = folder([
            (
                b"a",
                folder([
                    (b"i0", Node::Item(0)),
                    (b"i1", Node::Item(0)),
                    (b"i2", Node::Item(0)),
                ]),
            ),
            (b"b", folder([(b"i0", Node::Item(0))])),
            (
                b"z",
                folder([
                    (b"i0", Node::Item(0)),
                    (b"i1", Node::Item(0)),
                    (b"i2", Node::Item(0)),
                    (b"i3", Node::Item(0)),
                ]),
            ),
        ]);
        let listed = RefCell::new(Vec::new());

        // The page fills inside the first folder: only the run's top
        // folder and that folder are read.
        let (items, more) = archived_page(&run, None, 2, in_memory(&tree, &run, &listed)).unwrap();
        assert_eq!(paths(&items), [b"a/i0".to_vec(), b"a/i1".to_vec()]);
        assert!(more);
        assert_eq!(*listed.borrow(), [b"".to_vec(), b"a".to_vec()]);

        // A page starting inside the last folder never reads the
        // earlier siblings.
        listed.borrow_mut().clear();
        let after = SyncPath::try_from(b"z/i1".as_slice()).unwrap();
        let (items, more) =
            archived_page(&run, Some(&after), 2, in_memory(&tree, &run, &listed)).unwrap();
        assert_eq!(paths(&items), [b"z/i2".to_vec(), b"z/i3".to_vec()]);
        assert!(!more);
        assert_eq!(*listed.borrow(), [b"".to_vec(), b"z".to_vec()]);
    }

    proptest! {
        /// Every page equals the naive walk: keep `> after`, sort,
        /// truncate, `more` when any is left.
        #[test]
        fn fuzz_archive_pages_match_the_naive_walk(
            tree in nodes(3),
            pick in any::<usize>(),
            limit in prop_oneof![Just(0usize), Just(1), Just(2), Just(usize::MAX), 0..5usize],
        ) {
            let run = archive_name(at(1_000));
            let options = cursors(&tree);
            let after = options[pick % options.len()].as_ref();
            let expected = naive_page(&tree, after.map(|path| path.as_bytes()), limit);
            prop_assert_eq!(page(&tree, &run, after, limit), expected);
        }

        /// Paging a whole run by feeding each page's last path back as
        /// the cursor visits every item once, in order.
        #[test]
        fn fuzz_archive_paging_visits_every_item_once(
            tree in nodes(3),
            limit in prop_oneof![Just(1usize), Just(2), Just(usize::MAX), 1..5usize],
        ) {
            let run = archive_name(at(1_000));
            let all = naive(&tree);
            let mut seen = Vec::new();
            let mut after = None;
            let mut pages = 0;
            loop {
                let (items, more) = page(&tree, &run, after.as_ref(), limit);
                pages += 1;
                prop_assert!(pages <= all.len() + 2, "paging did not advance");
                seen.extend(items.iter().cloned());
                if !more {
                    break;
                }
                after = items.last().map(|(path, _)| SyncPath::try_from(path.as_slice()).unwrap());
            }
            prop_assert_eq!(seen, all);
        }
    }

    #[test]
    fn names_around_slash_page_in_byte_order() {
        let directory = tempfile::tempdir().unwrap();
        let run = archive_name(at(1_000));
        let root = directory.path().join(".foldover/archive").join(&run);
        fs::create_dir_all(root.join("a")).unwrap();
        fs::write(root.join("a-b"), b"").unwrap();
        fs::write(root.join("a.txt"), b"").unwrap();
        fs::write(root.join("a/x"), b"").unwrap();
        fs::write(root.join("a0"), b"").unwrap();
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();

        let (items, more) = endpoint.archived_items(&run, None, 3).unwrap();
        assert_eq!(
            paths(&items),
            [b"a-b".to_vec(), b"a.txt".to_vec(), b"a/x".to_vec()]
        );
        assert!(more);
        let (rest, more) = endpoint
            .archived_items(&run, Some(items[2].path()), 3)
            .unwrap();
        assert_eq!(paths(&rest), [b"a0".to_vec()]);
        assert!(!more);
    }

    #[test]
    fn names_are_utc_timestamps_that_parse_back_exactly() {
        for (millis, name) in [
            (0, "1970-01-01T00-00-00.000Z"),
            (951_868_799_999, "2000-02-29T23-59-59.999Z"),
            (1_792_315_800_123, "2026-10-18T09-30-00.123Z"),
        ] {
            assert_eq!(archive_name(at(millis)), name);
            assert_eq!(archive_time(name.as_bytes()), Some(at(millis)));
        }
        for name in [
            "recovered",
            "2026-02-30T00-00-00.000Z",
            "2026-10-18T24-00-00.000Z",
            "2026-10-18T09-30-00.123",
            "2026-10-18T09:30:00.123Z",
            "+026-10-18T09-30-00.123Z",
            "2026-10-18T09-30-00.123Z-copy",
        ] {
            assert_eq!(archive_time(name.as_bytes()), None, "{name}");
        }
    }

    #[test]
    fn run_names_carry_their_pair_after_the_time() {
        let pair: PairId = "0123456789abcdef0123456789abcdef".parse().unwrap();
        let name = run_name(at(1_792_315_800_123), pair);
        assert_eq!(
            name,
            "2026-10-18T09-30-00.123Z_0123456789abcdef0123456789abcdef"
        );
        assert_eq!(
            parse_run(name.as_bytes()),
            Some((at(1_792_315_800_123), Some(pair)))
        );
        assert_eq!(
            parse_run(b"2026-10-18T09-30-00.123Z"),
            Some((at(1_792_315_800_123), None))
        );
        assert_eq!(ArchivedRun::new(name.clone(), at(0)).owner(), Some(pair));
        assert!(run_folder(&name).is_some());
        for name in [
            "2026-10-18T09-30-00.123Z_",
            "2026-10-18T09-30-00.123Z_0123456789ABCDEF0123456789ABCDEF",
            "2026-10-18T09-30-00.123Z_0123456789abcdef",
            "2026-10-18T09-30-00.123Z_00000000000000000000000000000000",
            "2026-10-18T09-30-00.123Z_0123456789abcdef0123456789abcdef_x",
            "2026-10-18T09-30-00.123_0123456789abcdef0123456789abcdef",
        ] {
            assert_eq!(parse_run(name.as_bytes()), None, "{name}");
            assert!(run_folder(name).is_none(), "{name}");
        }
    }

    #[test]
    fn pruning_deletes_only_expired_runs() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join(".foldover/archive");
        let day = 86_400_000;
        let now = at(100 * day);
        for (name, age) in [("old", 31), ("recent", 29)] {
            let run = archive.join(archive_name(at(100 * day - age * day)));
            fs::create_dir_all(run.join("folder")).unwrap();
            fs::write(run.join("folder/file"), name).unwrap();
        }
        fs::create_dir_all(archive.join("kept by the user")).unwrap();
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();

        let retention = Duration::from_secs(30 * 86_400);
        assert_eq!(
            endpoint
                .prune_archive(retention, now, Pruning::Everyone)
                .unwrap(),
            1
        );
        let mut left: Vec<_> = fs::read_dir(&archive)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [archive_name(at(71 * day)), "kept by the user".into()]
        );
        assert_eq!(
            endpoint
                .prune_archive(retention, now, Pruning::Everyone)
                .unwrap(),
            0
        );

        let empty = tempfile::tempdir().unwrap();
        let endpoint = LocalEndpoint::open(empty.path()).unwrap();
        assert_eq!(
            endpoint
                .prune_archive(retention, now, Pruning::Everyone)
                .unwrap(),
            0
        );
    }

    /// Pairs sharing a folder each prune their own runs at the retention;
    /// another pair's only at twice it, and untagged runs at it.
    #[test]
    fn a_pair_prunes_its_own_runs_and_others_only_much_later() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join(".foldover/archive");
        let day = 86_400_000;
        let now = at(100 * day);
        let ours: PairId = "0123456789abcdef0123456789abcdef".parse().unwrap();
        let theirs: PairId = "fedcba9876543210fedcba9876543210".parse().unwrap();
        let names = [
            run_name(at(100 * day - 31 * day), ours),
            run_name(at(100 * day - 31 * day), theirs),
            run_name(at(100 * day - 61 * day), theirs),
            archive_name(at(100 * day - 31 * day)),
            run_name(at(100 * day - 29 * day), ours),
        ];
        for name in &names {
            fs::create_dir_all(archive.join(name)).unwrap();
        }
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();
        let retention = Duration::from_secs(30 * 86_400);
        assert_eq!(
            endpoint
                .prune_archive(retention, now, Pruning::Pair(ours))
                .unwrap(),
            3
        );
        let mut left: Vec<_> = fs::read_dir(&archive)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        let mut kept = vec![names[1].clone(), names[4].clone()];
        kept.sort();
        assert_eq!(left, kept);
        // The Mac sharing the folder prunes everyone's by the retention.
        assert_eq!(
            endpoint
                .prune_archive(retention, now, Pruning::Everyone)
                .unwrap(),
            1
        );
    }

    #[test]
    fn runs_list_newest_first_and_items_go_back_where_they_were() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let older = archive_name(at(1_000));
        let newer = archive_name(at(2_000));
        fs::create_dir_all(root.join(".foldover/archive").join(&older).join("docs")).unwrap();
        fs::create_dir_all(root.join(".foldover/archive").join(&newer)).unwrap();
        fs::create_dir_all(root.join(".foldover/archive/not a run")).unwrap();
        let run = root.join(".foldover/archive").join(&older);
        fs::write(run.join("docs/b"), b"bb").unwrap();
        fs::write(run.join("docs/a"), b"a").unwrap();
        fs::write(run.join("top"), b"t").unwrap();
        let endpoint = LocalEndpoint::open(root).unwrap();

        let runs = endpoint.archived_runs().unwrap();
        assert_eq!(
            runs.iter().map(ArchivedRun::name).collect::<Vec<_>>(),
            [&newer, &older]
        );
        let (items, more) = endpoint.archived_items(&older, None, 2).unwrap();
        assert_eq!(
            items
                .iter()
                .map(|item| item.path().as_bytes())
                .collect::<Vec<_>>(),
            [b"docs/a".as_slice(), b"docs/b"]
        );
        assert!(more);
        assert_eq!(items[1].size(), 2);
        let (rest, more) = endpoint
            .archived_items(&older, Some(items[1].path()), 2)
            .unwrap();
        assert_eq!(rest.len(), 1);
        assert!(!more);
        assert!(matches!(
            endpoint.archived_items("../etc", None, 10),
            Err(ScanError::UnknownArchiveRun)
        ));

        let path = SyncPath::try_from(b"docs/a".as_slice()).unwrap();
        let placed = endpoint
            .put_back(&older, &path, Durability::Standard)
            .unwrap();
        assert_eq!(placed, path);
        assert_eq!(fs::read(root.join("docs/a")).unwrap(), b"a");
        assert!(matches!(
            endpoint.put_back(&older, &path, Durability::Standard),
            Err(ApplyError::NotArchived(_))
        ));
        fs::write(run.join("docs/a"), b"again").unwrap();
        let placed = endpoint
            .put_back(&older, &path, Durability::Standard)
            .unwrap();
        assert_eq!(placed.as_bytes(), b"docs/a (restored)");
        assert_eq!(fs::read(root.join("docs/a")).unwrap(), b"a");
        assert_eq!(fs::read(root.join("docs/a (restored)")).unwrap(), b"again");
        let internal = SyncPath::try_from(b".foldover/id".as_slice()).unwrap();
        assert!(
            endpoint
                .put_back(&older, &internal, Durability::Standard)
                .is_err()
        );
    }

    #[test]
    fn run_listing_skips_timestamp_named_files_and_links() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join(".foldover/archive");
        let real = archive_name(at(3_000));
        fs::create_dir_all(archive.join(&real)).unwrap();
        fs::write(archive.join(archive_name(at(2_000))), b"file").unwrap();
        std::os::unix::fs::symlink(&real, archive.join(archive_name(at(1_000)))).unwrap();
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();

        let runs = endpoint.archived_runs().unwrap();
        assert_eq!(
            runs.iter().map(ArchivedRun::name).collect::<Vec<_>>(),
            [real.as_str()]
        );
    }

    #[test]
    fn restored_names_keep_extensions_and_fit_the_name_limit() {
        let name = |path: &[u8], attempt| {
            let path = SyncPath::try_from(path).unwrap();
            sibling_name(&path, RESTORED, attempt)
                .unwrap()
                .as_bytes()
                .to_vec()
        };
        assert_eq!(date_name(at(86_400_000 * 365)), "1971-01-01");
        assert_eq!(name(b"a/report.txt", 1), b"a/report (restored).txt");
        assert_eq!(name(b"report.tar.gz", 2), b"report.tar (restored 2).gz");
        assert_eq!(name(b".profile", 1), b".profile (restored)");

        let long = "é".repeat(200) + ".txt";
        let restored = name(long.as_bytes(), 1);
        assert!(restored.len() <= NAME_MAX);
        assert!(
            String::from_utf8(restored)
                .unwrap()
                .ends_with("é (restored).txt")
        );
    }
}
