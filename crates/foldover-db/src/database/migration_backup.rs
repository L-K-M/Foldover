//! Keep one completed pre-upgrade recovery copy. Publish it only after the
//! online backup is durable; retries replace that schema's copy atomically.
//! Cleanup runs under Database's writer lease, including after crash recovery.

use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::{Error, FILE_MODE};

const TEMP_PREFIX: &str = ".state-backup-";
const BACKUP_PREFIX: &str = "state-before-v";
const COMPLETE: &str = "completed";

pub(super) fn create(connection: &Connection, directory: &Path, schema: i64) -> Result<(), Error> {
    let path = directory.join(format!("{TEMP_PREFIX}{}.db", uuid::Uuid::new_v4()));
    let destination = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(&path)?;
    let temporary = Temporary(path);
    drop(destination);
    connection.backup(
        rusqlite::MAIN_DB,
        &temporary.0,
        None::<fn(rusqlite::backup::Progress)>,
    )?;
    File::open(&temporary.0)?.sync_all()?;
    fs::rename(
        &temporary.0,
        directory.join(format!("{BACKUP_PREFIX}{schema}-{COMPLETE}.db")),
    )?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

struct Temporary(PathBuf);

impl Drop for Temporary {
    fn drop(&mut self) {
        // An interrupted copy can also leave SQLite sidecars. If removal
        // fails here, the next open retries it and reports any persistent error.
        let _ = fs::remove_file(&self.0);
        for extension in ["db-journal", "db-wal", "db-shm"] {
            let _ = fs::remove_file(self.0.with_extension(extension));
        }
    }
}

/// Remove recognized scratch files and superseded recovery copies. Legacy
/// UUID-named copies may be partial, so never select one as the retained copy.
/// Without a completed backup followed by a committed upgrade, retain them.
pub(super) fn cleanup(directory: &Path, current_schema: i64) -> Result<(), Error> {
    let mut backups = Vec::new();
    let mut keep = None;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let base = name
            .strip_suffix("-journal")
            .or_else(|| name.strip_suffix("-wal"))
            .or_else(|| name.strip_suffix("-shm"))
            .unwrap_or(name);
        if let Some(id) = base
            .strip_prefix(TEMP_PREFIX)
            .and_then(|s| s.strip_suffix(".db"))
        {
            if uuid::Uuid::parse_str(id).is_ok() {
                fs::remove_file(entry.path())?;
            }
            continue;
        }
        let Some((schema, identity)) = backup_name(base) else {
            continue;
        };
        // Do not touch a backup for an upgrade that has not committed yet,
        // including a failed attempt by a newer binary.
        if schema >= current_schema {
            continue;
        }
        let completed = identity == COMPLETE;
        if completed && base == name {
            keep = Some(keep.map_or(schema, |previous: i64| previous.max(schema)));
        }
        backups.push((schema, completed, entry.path()));
    }
    if let Some(keep) = keep {
        for (schema, completed, path) in backups {
            if schema <= keep && !(schema == keep && completed) {
                fs::remove_file(path)?;
            }
        }
    }
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn backup_name(name: &str) -> Option<(i64, &str)> {
    let (schema, identity) = name
        .strip_prefix(BACKUP_PREFIX)?
        .strip_suffix(".db")?
        .split_once('-')?;
    let schema = schema.parse::<i64>().ok().filter(|schema| *schema >= 0)?;
    if identity != COMPLETE && uuid::Uuid::parse_str(identity).is_err() {
        return None;
    }
    Some((schema, identity))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy(directory: &Path, schema: i64) -> PathBuf {
        let path = directory.join(format!(
            "state-before-v{schema}-{}.db",
            uuid::Uuid::new_v4()
        ));
        fs::write(&path, b"possibly incomplete legacy backup").unwrap();
        path
    }

    #[test]
    fn cleanup_waits_for_a_committed_upgrade_and_keeps_its_recovery_copy() {
        let directory = tempfile::tempdir().unwrap();
        let old = legacy(directory.path(), 26);
        let duplicate = legacy(directory.path(), 27);
        let journal = duplicate.with_extension("db-journal");
        fs::write(&journal, b"partial journal").unwrap();
        let future = legacy(directory.path(), 29);
        let unrelated = directory.path().join("state-before-v27-not-a-backup.db");
        fs::write(&unrelated, b"leave alone").unwrap();
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE original(value); INSERT INTO original VALUES (42)")
            .unwrap();
        create(&connection, directory.path(), 27).unwrap();
        cleanup(directory.path(), 27).unwrap();
        assert!(old.exists());
        assert!(duplicate.exists());
        cleanup(directory.path(), 28).unwrap();
        assert!(!old.exists());
        assert!(!duplicate.exists());
        assert!(!journal.exists());
        assert!(future.exists());
        assert!(unrelated.exists());
        let copy =
            Connection::open(directory.path().join("state-before-v27-completed.db")).unwrap();
        assert_eq!(
            copy.query_row("SELECT value FROM original", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            42
        );
    }

    #[test]
    fn legacy_files_survive_until_a_known_complete_backup_exists() {
        let directory = tempfile::tempdir().unwrap();
        let old = legacy(directory.path(), 26);
        cleanup(directory.path(), 30).unwrap();
        assert!(old.exists());
    }

    #[test]
    fn interrupted_backups_are_removed_without_following_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let name = format!("{TEMP_PREFIX}{}.db", uuid::Uuid::new_v4());
        let partial = directory.path().join(name);
        fs::write(&partial, b"partial").unwrap();
        let journal = partial.with_extension("db-journal");
        fs::write(&journal, b"journal").unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let link = directory
            .path()
            .join(format!("{TEMP_PREFIX}{}.db", uuid::Uuid::new_v4()));
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        cleanup(directory.path(), 30).unwrap();
        assert!(!partial.exists());
        assert!(!journal.exists());
        assert!(link.is_symlink());
        assert!(outside.path().exists());
    }

    #[test]
    fn retained_backups_keep_their_sidecars_until_superseded() {
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE original(value)")
            .unwrap();
        create(&connection, directory.path(), 27).unwrap();
        let sidecars = ["journal", "wal", "shm"].map(|suffix| {
            let path = directory
                .path()
                .join(format!("state-before-v27-completed.db-{suffix}"));
            fs::write(&path, b"recovery state").unwrap();
            path
        });
        cleanup(directory.path(), 28).unwrap();
        for path in &sidecars {
            assert!(path.exists(), "retained backup lost {}", path.display());
        }
        create(&connection, directory.path(), 28).unwrap();
        cleanup(directory.path(), 29).unwrap();
        for path in &sidecars {
            assert!(
                !path.exists(),
                "obsolete sidecar leaked: {}",
                path.display()
            );
        }
    }

    #[test]
    fn abandoned_wal_sidecars_are_removed_on_open() {
        let directory = tempfile::tempdir().unwrap();
        let name = format!("{TEMP_PREFIX}{}.db", uuid::Uuid::new_v4());
        for suffix in ["wal", "shm"] {
            fs::write(
                directory.path().join(format!("{name}-{suffix}")),
                b"partial",
            )
            .unwrap();
        }
        cleanup(directory.path(), 30).unwrap();
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn failed_backup_preserves_previous_complete_copy_and_removes_scratch() {
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE original(value)")
            .unwrap();
        create(&connection, directory.path(), 27).unwrap();
        let complete = directory.path().join("state-before-v27-completed.db");
        let before = fs::read(&complete).unwrap();
        connection
            .execute_batch("BEGIN IMMEDIATE; INSERT INTO original VALUES (1)")
            .unwrap();
        assert!(create(&connection, directory.path(), 27).is_err());
        assert_eq!(fs::read(&complete).unwrap(), before);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
