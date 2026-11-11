//! A consistent read-only view of one pair's stored rows, captured under
//! the state's single writer and streamed to visitors later. The primary
//! connection stays the only writer; this view holds a read-only connection
//! and a clone of the writer lease until it drops.

use std::fs::{self, File};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Arc;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use super::{BUSY_TIMEOUT, DATABASE_NAME, Database, Error, JournalRow};

/// One base row borrowed from a pinned read view. The slices point into the
/// view's SQLite statement and are valid only for the visitor call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BaseBlob<'a> {
    pub path: &'a [u8],
    pub left_version: &'a [u8],
    pub right_version: &'a [u8],
}

/// One snapshot row borrowed from a pinned read view. The slices point into
/// the view's SQLite statement and are valid only for the visitor call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotBlob<'a> {
    pub path: &'a [u8],
    pub fingerprint: &'a [u8],
    pub version: &'a [u8],
}

/// A visitor failure, kept distinct from a driver failure.
#[derive(Debug, thiserror::Error)]
pub enum ReadViewError<E> {
    #[error("State database: {0}")]
    Driver(#[from] Error),
    #[error("Read view visitor failed")]
    Visitor(E),
}

/// A pair's stored base and snapshot rows, pinned at one revision. The
/// connection stays in a read transaction until the view drops, so every
/// visit sees the same WAL snapshot. Rows are streamed one at a time and
/// borrowed by the visitor; no whole-side vector is built.
pub struct ReadView {
    connection: Connection,
    _writer: Arc<File>,
    pair: Vec<u8>,
}

impl ReadView {
    /// Stored rows in this pinned base, without collecting their payloads.
    pub fn base_len(&self) -> Result<usize, Error> {
        let count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM base_entry WHERE pair_id=?1",
            [self.pair.as_slice()],
            |row| row.get(0),
        )?;
        usize::try_from(count)
            .map_err(|_| Error::InvalidState("Base row count exceeds address space"))
    }

    /// Stored rows in one pinned snapshot side. Invalid cached versions
    /// still count here; the typed reader may later skip them.
    pub fn snapshot_len(&self, side: &str) -> Result<usize, Error> {
        let count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM snapshot_entry WHERE pair_id=?1 AND side=?2",
            params![self.pair.as_slice(), side],
            |row| row.get(0),
        )?;
        usize::try_from(count)
            .map_err(|_| Error::InvalidState("Snapshot row count exceeds address space"))
    }

    /// Visit the pair's base rows in raw path order. Each row is borrowed
    /// for the duration of the call; the driver never collects or copies
    /// the versions.
    pub fn base_rows<F, E>(&self, mut visit: F) -> Result<(), ReadViewError<E>>
    where
        F: FnMut(BaseBlob<'_>) -> Result<(), E>,
    {
        let mut statement = self
            .connection
            .prepare(
                "SELECT path,left_version,right_version FROM base_entry \
                 WHERE pair_id=?1 ORDER BY path",
            )
            .map_err(Error::from)?;
        let mut rows = statement
            .query(params![self.pair.as_slice()])
            .map_err(Error::from)?;
        while let Some(row) = rows.next().map_err(Error::from)? {
            let blob = BaseBlob {
                path: blob_bytes(row, 0)?,
                left_version: blob_bytes(row, 1)?,
                right_version: blob_bytes(row, 2)?,
            };
            if let Err(error) = visit(blob) {
                return Err(ReadViewError::Visitor(error));
            }
        }
        Ok(())
    }

    /// Visit one side's snapshot rows in raw path order. Each row is
    /// borrowed for the duration of the call; the driver never collects or
    /// copies the fingerprint or version.
    pub fn snapshot<F, E>(&self, side: &str, mut visit: F) -> Result<(), ReadViewError<E>>
    where
        F: FnMut(SnapshotBlob<'_>) -> Result<(), E>,
    {
        let mut statement = self
            .connection
            .prepare(
                "SELECT path,fingerprint,version FROM snapshot_entry \
                 WHERE pair_id=?1 AND side=?2 ORDER BY path",
            )
            .map_err(Error::from)?;
        let mut rows = statement
            .query(params![self.pair.as_slice(), side])
            .map_err(Error::from)?;
        while let Some(row) = rows.next().map_err(Error::from)? {
            let blob = SnapshotBlob {
                path: blob_bytes(row, 0)?,
                fingerprint: blob_bytes(row, 1)?,
                version: blob_bytes(row, 2)?,
            };
            if let Err(error) = visit(blob) {
                return Err(ReadViewError::Visitor(error));
            }
        }
        Ok(())
    }

    /// The pinned journal's newest run, if any. The answer comes from the
    /// same WAL snapshot as every other visit on this view.
    pub fn journal_run(&self) -> Result<Option<String>, Error> {
        let run: Option<String> = self.connection.query_row(
            "SELECT max(run) FROM run_journal WHERE pair_id=?1",
            [self.pair.as_slice()],
            |row| row.get(0),
        )?;
        Ok(run)
    }

    /// Visit the pinned journal's newest run in write order. Rows are
    /// streamed one at a time; no whole-run vector is built.
    pub fn journal_rows<F, E>(&self, mut visit: F) -> Result<(), ReadViewError<E>>
    where
        F: FnMut(JournalRow) -> Result<(), E>,
    {
        let mut statement = self
            .connection
            .prepare(
                "SELECT seq,path,side,action,version,base_left,base_right,source \
                 FROM run_journal WHERE pair_id=?1 \
                 AND run=(SELECT max(run) FROM run_journal WHERE pair_id=?1) \
                 ORDER BY seq",
            )
            .map_err(Error::from)?;
        let mut rows = statement
            .query(params![self.pair.as_slice()])
            .map_err(Error::from)?;
        while let Some(row) = rows.next().map_err(Error::from)? {
            let base = match (
                row.get::<_, Option<Vec<u8>>>(5).map_err(Error::from)?,
                row.get::<_, Option<Vec<u8>>>(6).map_err(Error::from)?,
            ) {
                (Some(left), Some(right)) => Some((left, right)),
                _ => None,
            };
            let journal = JournalRow {
                seq: row.get(0).map_err(Error::from)?,
                path: row.get(1).map_err(Error::from)?,
                side: row.get(2).map_err(Error::from)?,
                action: row.get(3).map_err(Error::from)?,
                version: row.get(4).map_err(Error::from)?,
                base,
                source: row.get(7).map_err(Error::from)?,
            };
            if let Err(error) = visit(journal) {
                return Err(ReadViewError::Visitor(error));
            }
        }
        Ok(())
    }
}

impl Drop for ReadView {
    fn drop(&mut self) {
        // The connection closes right after this, but an explicit rollback
        // releases the WAL snapshot even if close is deferred.
        let _ = self.connection.execute_batch("ROLLBACK");
    }
}

impl Database {
    /// Capture a consistent read-only view of one pair's stored rows at
    /// `revision`. The caller must hold the state's single writer lock; the
    /// view keeps the writer lease alive until it drops. The pair must
    /// exist and still be at `revision`.
    pub fn read_view(&self, pair: &[u8], revision: i64) -> Result<ReadView, Error> {
        let path = self.directory.join(DATABASE_NAME);
        verify_anchor(&path, &self.anchor)?;
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        // A deferred transaction pins the WAL snapshot at its first read.
        connection.execute_batch("BEGIN")?;
        let pinned = connection
            .query_row("SELECT revision FROM pair WHERE id=?1", [pair], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?;
        match pinned {
            Some(found) if found == revision => {}
            Some(_) => {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(Error::StaleRevision);
            }
            None => {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(Error::PairNotFound);
            }
        }
        // The path could have been swapped between the open and the pin;
        // never fall back to a different catalog.
        verify_anchor(&path, &self.anchor)?;
        Ok(ReadView {
            connection,
            _writer: Arc::clone(&self._writer),
            pair: pair.to_vec(),
        })
    }
}

/// The read-only connection must resolve to the same inode the primary
/// opened. A replaced path, a symlink, or a different file is rejected.
fn verify_anchor(path: &Path, anchor: &File) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path)?;
    let anchor = anchor.metadata()?;
    if metadata.file_type().is_symlink()
        || metadata.dev() != anchor.dev()
        || metadata.ino() != anchor.ino()
    {
        return Err(Error::ReplacedState);
    }
    Ok(())
}

/// Borrow one column's bytes from a row without copying. The slice is valid
/// until the row is advanced.
fn blob_bytes<'a>(row: &'a rusqlite::Row<'_>, index: usize) -> Result<&'a [u8], Error> {
    let value = row.get_ref(index).map_err(Error::from)?;
    value
        .as_bytes()
        .map_err(|error| Error::Sqlite(error.into()))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::super::JournalChange;
    use super::super::tests::{location, put, snapshot_row, values};
    use super::*;

    #[test]
    fn a_view_pins_the_base_and_both_snapshots() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        database
            .change_base(
                &pair.id,
                pair.revision,
                &[put(b"base-a", b"base-1")],
                JournalChange::None,
            )
            .unwrap();
        database
            .replace_snapshot(&pair.id, pair.revision, "left", &[snapshot_row(b"left-a")])
            .unwrap();
        database
            .replace_snapshot(
                &pair.id,
                pair.revision,
                "right",
                &[snapshot_row(b"right-a")],
            )
            .unwrap();
        let view = database.read_view(&pair.id, pair.revision).unwrap();

        // The primary commits new rows, deletes a whole side, moves the
        // pair's configuration revision, and finally removes the pair.
        database
            .change_base(
                &pair.id,
                pair.revision,
                &[put(b"base-a", b"base-2"), put(b"base-b", b"base-3")],
                JournalChange::None,
            )
            .unwrap();
        database
            .replace_snapshot(&pair.id, pair.revision, "left", &[snapshot_row(b"left-b")])
            .unwrap();
        database
            .replace_snapshot(&pair.id, pair.revision, "right", &[])
            .unwrap();
        let updated = database
            .update_pair(&pair.id, pair.revision, &values())
            .unwrap();
        database.remove_pair(&pair.id, updated.revision).unwrap();
        assert_eq!(view.base_len().unwrap(), 1);
        assert_eq!(view.snapshot_len("left").unwrap(), 1);
        assert_eq!(view.snapshot_len("right").unwrap(), 1);

        let mut base = Vec::new();
        view.base_rows(|row| {
            base.push((
                row.path.to_vec(),
                row.left_version.to_vec(),
                row.right_version.to_vec(),
            ));
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(
            base,
            [(b"base-a".to_vec(), b"base-1".to_vec(), b"base-1".to_vec())]
        );

        let mut left = Vec::new();
        view.snapshot("left", |row| {
            left.push(row.path.to_vec());
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(left, [b"left-a".to_vec()]);

        let mut right = Vec::new();
        view.snapshot("right", |row| {
            right.push(row.path.to_vec());
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(right, [b"right-a".to_vec()]);
    }

    #[test]
    fn stale_and_missing_views_are_rejected() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        assert!(matches!(
            database.read_view(&pair.id, pair.revision + 1),
            Err(Error::StaleRevision)
        ));
        assert!(matches!(
            database.read_view(&pair.id, pair.revision - 1),
            Err(Error::StaleRevision)
        ));
        assert!(matches!(
            database.read_view(&[0; 16], pair.revision),
            Err(Error::PairNotFound)
        ));
    }

    #[test]
    fn a_view_keeps_the_writer_lease_until_it_drops() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let view = database.read_view(&pair.id, pair.revision).unwrap();
        drop(database);
        assert!(matches!(
            Database::open(&location),
            Err(Error::WriterLocked(_))
        ));
        drop(view);
        Database::open(&location).unwrap();
    }

    #[test]
    fn a_view_connection_is_read_only() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let view = database.read_view(&pair.id, pair.revision).unwrap();
        let error = view
            .connection
            .execute_batch("DELETE FROM base_entry")
            .unwrap_err();
        assert_eq!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::ReadOnly)
        );
    }

    #[test]
    fn a_replaced_database_path_is_rejected() {
        let (directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let path = database.directory().join(DATABASE_NAME);
        let replacement = directory.path().join("replacement.db");
        fs::write(&replacement, b"not a database").unwrap();
        let anchor = database.anchor.metadata().unwrap();
        let replacement_metadata = fs::metadata(&replacement).unwrap();
        assert_ne!(
            (replacement_metadata.dev(), replacement_metadata.ino()),
            (anchor.dev(), anchor.ino())
        );
        fs::rename(&replacement, &path).unwrap();
        assert!(matches!(
            database.read_view(&pair.id, pair.revision),
            Err(Error::ReplacedState)
        ));
    }

    #[test]
    fn anchor_verification_rejects_replacement_and_symlink() {
        let (directory, location) = location();
        let database = Database::open(&location).unwrap();
        let path = database.directory().join(DATABASE_NAME);
        verify_anchor(&path, &database.anchor).unwrap();

        let replacement = directory.path().join("replacement.db");
        fs::write(&replacement, b"other").unwrap();
        assert!(matches!(
            verify_anchor(&replacement, &database.anchor),
            Err(Error::ReplacedState)
        ));

        let link = directory.path().join("link.db");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(matches!(
            verify_anchor(&link, &database.anchor),
            Err(Error::ReplacedState)
        ));
    }

    #[test]
    fn visiting_is_lazy_and_bounded() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let rows: Vec<_> = (0..1000)
            .map(|index| snapshot_row(format!("p-{index:04}").as_bytes()))
            .collect();
        database
            .change_base(
                &pair.id,
                pair.revision,
                &[put(b"base", b"1")],
                JournalChange::None,
            )
            .unwrap();
        database
            .replace_snapshot(&pair.id, pair.revision, "left", &rows)
            .unwrap();
        let view = database.read_view(&pair.id, pair.revision).unwrap();

        let seen = Cell::new(0);
        let outcome = view.snapshot("left", |row| {
            let index = seen.get();
            assert_eq!(row.path, format!("p-{index:04}").as_bytes());
            seen.set(index + 1);
            if index == 2 { Err("stop") } else { Ok(()) }
        });
        assert!(matches!(outcome, Err(ReadViewError::Visitor("stop"))));
        assert_eq!(seen.get(), 3);

        let seen = Cell::new(0);
        let outcome = view.base_rows(|_| {
            seen.set(seen.get() + 1);
            if seen.get() == 1 { Err("stop") } else { Ok(()) }
        });
        assert!(matches!(outcome, Err(ReadViewError::Visitor("stop"))));
        assert_eq!(seen.get(), 1);
    }

    #[test]
    fn journal_read_view_consistency() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let first = [
            JournalRow {
                seq: 0,
                path: b"a".to_vec(),
                side: "left".into(),
                action: "create".into(),
                version: b"v0".to_vec(),
                base: None,
                source: None,
            },
            JournalRow {
                seq: 1,
                path: b"b".to_vec(),
                side: "right".into(),
                action: "replace".into(),
                version: b"v1".to_vec(),
                base: Some((b"l".to_vec(), b"r".to_vec())),
                source: None,
            },
        ];
        database
            .change_base(
                &pair.id,
                pair.revision,
                &[put(b"base", b"1")],
                JournalChange::Record {
                    run: "run-a",
                    rows: &first,
                },
            )
            .unwrap();
        database
            .replace_snapshot(&pair.id, pair.revision, "left", &[snapshot_row(b"s")])
            .unwrap();
        let view = database.read_view(&pair.id, pair.revision).unwrap();
        assert_eq!(view.journal_run().unwrap().as_deref(), Some("run-a"));
        let mut seen = Vec::new();
        view.journal_rows(|row| {
            seen.push(row);
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(seen, first.to_vec());

        // The writer commits a newer run and new base and snapshot rows;
        // the view keeps the snapshot it pinned.
        let second = [JournalRow {
            seq: 0,
            path: b"c".to_vec(),
            side: "left".into(),
            action: "metadata".into(),
            version: b"v2".to_vec(),
            base: None,
            source: None,
        }];
        database
            .change_base(
                &pair.id,
                pair.revision,
                &[put(b"base", b"2")],
                JournalChange::Record {
                    run: "run-b",
                    rows: &second,
                },
            )
            .unwrap();
        database
            .replace_snapshot(&pair.id, pair.revision, "left", &[snapshot_row(b"t")])
            .unwrap();
        assert_eq!(view.journal_run().unwrap().as_deref(), Some("run-a"));
        let mut seen = Vec::new();
        view.journal_rows(|row| {
            seen.push(row);
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(seen, first.to_vec());
        assert_eq!(view.base_len().unwrap(), 1);
        assert_eq!(view.snapshot_len("left").unwrap(), 1);
        let mut paths = Vec::new();
        view.snapshot("left", |row| {
            paths.push(row.path.to_vec());
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(paths, [b"s".to_vec()]);

        // A visitor error stops the stream and leaves the view usable.
        let outcome = view.journal_rows(|_| -> Result<(), &'static str> { Err("stop") });
        assert!(matches!(outcome, Err(ReadViewError::Visitor("stop"))));
        assert_eq!(view.journal_run().unwrap().as_deref(), Some("run-a"));

        // A pair with no journal reports none and visits nothing.
        let other = database.create_pair(&values()).unwrap();
        let empty = database.read_view(&other.id, other.revision).unwrap();
        assert_eq!(empty.journal_run().unwrap(), None);
        let mut visited = false;
        empty
            .journal_rows(|_| {
                visited = true;
                Ok::<_, ()>(())
            })
            .unwrap();
        assert!(!visited);
    }

    #[test]
    fn a_visitor_error_on_another_thread_releases_the_view() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        database
            .change_base(
                &pair.id,
                pair.revision,
                &[put(b"base", b"1")],
                JournalChange::None,
            )
            .unwrap();
        let view = database.read_view(&pair.id, pair.revision).unwrap();
        drop(database);

        let outcome = std::thread::spawn(move || {
            view.base_rows(|_| -> Result<(), &'static str> { Err("stop") })
        })
        .join()
        .unwrap();
        assert!(matches!(outcome, Err(ReadViewError::Visitor("stop"))));
        Database::open(&location).unwrap();
    }

    #[test]
    fn a_panicking_visitor_on_another_thread_releases_the_view() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        database
            .change_base(
                &pair.id,
                pair.revision,
                &[put(b"base", b"1")],
                JournalChange::None,
            )
            .unwrap();
        let view = database.read_view(&pair.id, pair.revision).unwrap();
        drop(database);

        let outcome = std::thread::spawn(move || {
            let _ = view.base_rows(|_| -> Result<(), &'static str> { panic!("boom") });
        })
        .join();
        assert!(outcome.is_err());
        Database::open(&location).unwrap();
    }
}
