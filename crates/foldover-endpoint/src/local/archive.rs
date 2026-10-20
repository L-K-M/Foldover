//! Each root keeps replaced and removed entries in
//! `.foldover/archive/<run>/<path>`. `<run>` is the run's UTC start time,
//! e.g. `2026-10-18T09-30-00.123Z`, so retention can delete whole runs by
//! age without reading their contents.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use foldover_fs::{DirectoryScope, NodeKind, WriteError};
use foldover_types::{Durability, SyncPath};

use super::{ApplyError, LocalEndpoint, ScanError, protected};

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
}

impl ArchivedRun {
    pub(crate) fn new(name: String, started: SystemTime) -> Self {
        Self { name, started }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn started(&self) -> SystemTime {
        self.started
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
const SECONDS_PER_DAY: u64 = 86_400;
const SECONDS_PER_HOUR: u64 = 3_600;
const SECONDS_PER_MINUTE: u64 = 60;
const MILLIS_PER_SECOND: u64 = 1_000;
/// Days from 0000-03-01 to 1970-01-01 in the proleptic Gregorian calendar.
const EPOCH_DAY_OFFSET: u64 = 719_468;
const DAYS_PER_ERA: u64 = 146_097;

/// A sortable, Finder-safe run name. Runs of one pair never share a
/// millisecond, because they are serialized.
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

/// The start time a run name encodes; `None` for any other name.
pub(crate) fn archive_time(name: &[u8]) -> Option<SystemTime> {
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
            let Some(started) = archive_time(entry.name()) else {
                continue;
            };
            if let Ok(name) = String::from_utf8(entry.name().to_vec()) {
                runs.push(ArchivedRun { name, started });
            }
        }
        runs.sort_by_key(|run| std::cmp::Reverse(run.started));
        Ok(runs)
    }

    /// Files and links `run` archived, in path order after `after`: at most
    /// `limit`, and whether there are more.
    pub fn archived_items(
        &self,
        run: &str,
        after: Option<&SyncPath>,
        limit: usize,
    ) -> Result<(Vec<ArchivedItem>, bool), ScanError> {
        let folder = run_folder(run).ok_or(ScanError::UnknownArchiveRun)?;
        let mut items = Vec::new();
        let mut pending: Vec<Option<SyncPath>> = vec![None];
        while let Some(relative) = pending.pop() {
            let directory = match &relative {
                None => folder.clone(),
                Some(relative) => join(&folder, relative.as_bytes())?,
            };
            let entries = self
                .root
                .entries(DirectoryScope::At(&directory))
                .map_err(|source| scan_error(&directory, source))?;
            for entry in entries {
                let entry = entry.map_err(|source| scan_error(&directory, source))?;
                let path = match &relative {
                    None => SyncPath::try_from(entry.name())?,
                    Some(relative) => join(relative, entry.name())?,
                };
                let listed = after.is_none_or(|after| &path > after);
                match entry.metadata().kind() {
                    NodeKind::Directory => pending.push(Some(path)),
                    NodeKind::File | NodeKind::Symlink if listed => items.push(ArchivedItem {
                        path,
                        size: entry.metadata().size(),
                    }),
                    NodeKind::File | NodeKind::Symlink | NodeKind::Other => {}
                }
            }
        }
        items.sort_by(|left, right| left.path.cmp(&right.path));
        let more = items.len() > limit;
        items.truncate(limit);
        Ok((items, more))
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

    /// Permanently delete archived runs that started more than `retention`
    /// before `now`. Other folders in the archive are kept. Returns the
    /// number of runs removed.
    pub fn prune_archive(&self, retention: Duration, now: SystemTime) -> Result<u64, ApplyError> {
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
            let Some(started) = archive_time(entry.name()) else {
                continue;
            };
            if now.duration_since(started).is_ok_and(|age| age > retention) {
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
    use std::fs;

    use super::*;

    fn at(millis: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(millis)
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
        assert_eq!(endpoint.prune_archive(retention, now).unwrap(), 1);
        let mut left: Vec<_> = fs::read_dir(&archive)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [archive_name(at(71 * day)), "kept by the user".into()]
        );
        assert_eq!(endpoint.prune_archive(retention, now).unwrap(), 0);

        let empty = tempfile::tempdir().unwrap();
        let endpoint = LocalEndpoint::open(empty.path()).unwrap();
        assert_eq!(endpoint.prune_archive(retention, now).unwrap(), 0);
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
