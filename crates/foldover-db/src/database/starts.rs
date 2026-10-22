//! How the agent's runs ended, for crash-loop safe mode (PLAN.md §4.2): a
//! marker while one runs, and when a start found the previous run still
//! marked, its stop having been unclean. Plain files beside the database,
//! read before anything else could crash again.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{Database, Error, private_file};

const RUNNING: &str = "running";
const UNCLEAN_STOPS: &str = "unclean-stops";
/// A record being rewritten, beside the one it replaces.
const STAGED_EXTENSION: &str = "new";
/// Unclean stops remembered, newest last.
const REMEMBERED: usize = 16;

impl Database {
    /// Record that the agent starts at `now`, and return the starts that
    /// found the previous run without a clean stop, oldest first, this one
    /// included when it did.
    pub fn note_start(&self, now: SystemTime) -> Result<Vec<SystemTime>, Error> {
        let running = self.directory.join(RUNNING);
        let unclean = self.directory.join(UNCLEAN_STOPS);
        let mut stops = read_times(&unclean)?;
        if running.exists() {
            stops.push(now);
            let excess = stops.len().saturating_sub(REMEMBERED);
            stops.drain(..excess);
            write_times(&unclean, &stops)?;
        }
        write_times(&running, &[now])?;
        Ok(stops)
    }

    /// The marker `note_start` leaves while the agent runs, to clear at a
    /// clean stop without waiting for the database.
    pub fn run_marker(&self) -> RunMarker {
        RunMarker {
            running: self.directory.join(RUNNING),
        }
    }

    /// Forget earlier unclean stops, as a person resuming after safe mode
    /// does.
    pub fn forget_unclean_stops(&self) -> Result<(), Error> {
        remove(&self.directory.join(UNCLEAN_STOPS))
    }
}

/// The file that marks a running agent.
#[derive(Clone, Debug)]
pub struct RunMarker {
    running: PathBuf,
}

impl RunMarker {
    /// Record a clean stop: the next start does not count this run.
    pub fn note_clean_stop(&self) -> Result<(), Error> {
        remove(&self.running)
    }
}

/// Milliseconds since the Unix epoch, one per line; a missing file is
/// none, and a malformed line is skipped rather than blocking startup.
fn read_times(path: &Path) -> Result<Vec<SystemTime>, Error> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut bytes = Vec::new();
    private_file(path)?.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|line| line.trim().parse::<u64>().ok())
        .map(|milliseconds| UNIX_EPOCH + Duration::from_millis(milliseconds))
        .collect())
}

/// Replace the record whole: written beside it, then renamed over it, so
/// an agent killed or a disk filled midway leaves the old record rather
/// than an empty one, which would forget the stops safe mode counts.
fn write_times(path: &Path, times: &[SystemTime]) -> Result<(), Error> {
    let staged = path.with_extension(STAGED_EXTENSION);
    let mut file = private_file(&staged)?;
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    for time in times {
        let milliseconds = time
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        writeln!(file, "{milliseconds}")?;
    }
    file.sync_data()?;
    fs::rename(&staged, path)?;
    // The rename moved a directory entry, which a machine crash can roll
    // back; flush the directory too, as `backup` does.
    if let Some(directory) = path.parent() {
        fs::File::open(directory)?.sync_all()?;
    }
    Ok(())
}

fn remove(path: &Path) -> Result<(), Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::location;
    use super::*;

    #[test]
    fn starts_after_unclean_stops_are_remembered_until_forgotten() {
        let (directory, location) = location();
        let database = Database::open(&location).unwrap();
        let at = |seconds| UNIX_EPOCH + Duration::from_secs(seconds);

        assert!(database.note_start(at(10)).unwrap().is_empty());
        database.run_marker().note_clean_stop().unwrap();
        assert!(database.note_start(at(20)).unwrap().is_empty());
        // No clean stop: the next two starts each find the run before.
        assert_eq!(database.note_start(at(30)).unwrap(), [at(30)]);
        assert_eq!(database.note_start(at(40)).unwrap(), [at(30), at(40)]);
        database.run_marker().note_clean_stop().unwrap();
        assert_eq!(database.note_start(at(50)).unwrap(), [at(30), at(40)]);

        database.forget_unclean_stops().unwrap();
        database.run_marker().note_clean_stop().unwrap();
        assert!(database.note_start(at(60)).unwrap().is_empty());

        // A damaged record is read as far as it makes sense.
        let record = directory.path().join(UNCLEAN_STOPS);
        fs::write(&record, b"\xff\n30000\n").unwrap();
        fs::set_permissions(&record, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
        database.run_marker().note_clean_stop().unwrap();
        assert_eq!(database.note_start(at(70)).unwrap(), [at(30)]);

        // Only the newest are kept.
        for second in 0..2 * REMEMBERED as u64 {
            database.note_start(at(100 + second)).unwrap();
        }
        let stops = database.note_start(at(1_000)).unwrap();
        assert_eq!(stops.len(), REMEMBERED);
        assert_eq!(stops.last(), Some(&at(1_000)));
        // Each record is replaced whole, nothing staged left beside it.
        let staged = record.with_extension(STAGED_EXTENSION);
        assert!(!staged.exists());
    }
}
