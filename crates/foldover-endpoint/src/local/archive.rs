//! Each root keeps replaced and removed entries in
//! `.foldover/archive/<run>/<path>`. `<run>` is the run's UTC start time,
//! e.g. `2026-10-18T09-30-00.123Z`, so retention can delete whole runs by
//! age without reading their contents.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use foldover_fs::DirectoryScope;
use foldover_types::SyncPath;

use super::{ApplyError, LocalEndpoint, ScanError};

pub(super) const ARCHIVE_ROOT: &[u8] = b".foldover/archive";
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
pub(super) fn archive_time(name: &[u8]) -> Option<SystemTime> {
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

impl LocalEndpoint {
    /// Permanently delete archived runs that started more than `retention`
    /// before `now`. Other folders in the archive are kept. Returns the
    /// number of runs removed.
    pub fn prune_archive(&self, retention: Duration, now: SystemTime) -> Result<u64, ApplyError> {
        let archive = SyncPath::try_from(ARCHIVE_ROOT).expect("the archive path is valid");
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
}
