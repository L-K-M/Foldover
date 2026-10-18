use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

mod history;
pub use history::HistoryRow;

const APPLICATION_ID: i64 = 0x464f4c44; // FOLD
const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;
const DIRECTORY_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
const PERMISSION_BITS: u32 = 0o777;
const DATABASE_NAME: &str = "state.db";
const WRITER_LOCK: &str = "writer.lock";
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const PAIR_COLUMNS: &str = "id, revision, name, left_root, right_root, mode, modification_times, extended_attributes, permissions, durability, schedule, schedule_minutes, filters";
// Step N upgrades schema N-1 to N. Steps are append-only: never edit one
// that has shipped, because existing databases already ran it.
const MIGRATIONS: [&str; 9] = [
    SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7, SCHEMA_V8,
    SCHEMA_V9,
];
const SCHEMA_V1: &str = "
CREATE TABLE state_meta (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    minimum_reader INTEGER NOT NULL CHECK(minimum_reader > 0)
) STRICT;
INSERT INTO state_meta VALUES (1, 1);
CREATE TABLE pair (
    id BLOB PRIMARY KEY NOT NULL CHECK(length(id) = 16 AND id != zeroblob(16)),
    revision INTEGER NOT NULL CHECK(revision > 0),
    name TEXT NOT NULL CHECK(length(trim(name)) > 0 AND instr(name, char(0)) = 0),
    left_root BLOB NOT NULL CHECK(length(left_root) > 0 AND substr(left_root,1,1) = x'2f' AND instr(left_root,x'00') = 0),
    right_root BLOB NOT NULL CHECK(length(right_root) > 0 AND substr(right_root,1,1) = x'2f' AND instr(right_root,x'00') = 0),
    mode TEXT NOT NULL CHECK(mode IN ('sync','mirror-left','mirror-right','backup-left','backup-right')),
    CHECK(left_root != right_root)
) STRICT;
";
// Last-synced versions per pair and path; see foldover-state for encoding.
const SCHEMA_V2: &str = "
CREATE TABLE base_entry (
    pair_id BLOB NOT NULL REFERENCES pair(id) ON DELETE CASCADE,
    path BLOB NOT NULL CHECK(length(path) > 0 AND substr(path,1,1) != x'2f' AND instr(path,x'00') = 0),
    left_version BLOB NOT NULL,
    right_version BLOB NOT NULL,
    PRIMARY KEY (pair_id, path)
) STRICT, WITHOUT ROWID;
";
// Per-pair options; existing pairs get the defaults.
const SCHEMA_V3: &str = "
ALTER TABLE pair ADD COLUMN modification_times TEXT NOT NULL DEFAULT 'sync' CHECK(modification_times IN ('sync','ignore'));
ALTER TABLE pair ADD COLUMN extended_attributes TEXT NOT NULL DEFAULT 'sync' CHECK(extended_attributes IN ('sync','ignore'));
ALTER TABLE pair ADD COLUMN permissions TEXT NOT NULL DEFAULT 'ignore' CHECK(permissions IN ('sync','ignore'));
ALTER TABLE pair ADD COLUMN durability TEXT NOT NULL DEFAULT 'standard' CHECK(durability IN ('standard','full'));
";
// Endpoint identities recorded on a pair's first sync; unknown until then.
const SCHEMA_V4: &str = "
ALTER TABLE pair ADD COLUMN left_identity BLOB CHECK(left_identity IS NULL OR length(left_identity) = 16);
ALTER TABLE pair ADD COLUMN right_identity BLOB CHECK(right_identity IS NULL OR length(right_identity) = 16);
";
// Unattended sync interval in minutes; NULL syncs only when asked.
const SCHEMA_V5: &str = "
ALTER TABLE pair ADD COLUMN schedule TEXT NOT NULL DEFAULT 'manual' CHECK(schedule IN ('manual','every','changes'));
ALTER TABLE pair ADD COLUMN schedule_minutes INTEGER CHECK(schedule_minutes IS NULL OR schedule_minutes BETWEEN 1 AND 10080);
";
// Conflict choices bound to the versions a person saw; NULL means absent.
const SCHEMA_V6: &str = "
CREATE TABLE resolution (
    pair_id BLOB NOT NULL REFERENCES pair(id) ON DELETE CASCADE,
    path BLOB NOT NULL CHECK(length(path) > 0 AND substr(path,1,1) != x'2f' AND instr(path,x'00') = 0),
    keep TEXT NOT NULL CHECK(keep IN ('left','right')),
    left_version BLOB,
    right_version BLOB,
    PRIMARY KEY (pair_id, path)
) STRICT, WITHOUT ROWID;
";
// Each side's last observation, to skip rereading unchanged entries.
const SCHEMA_V7: &str = "
CREATE TABLE snapshot_entry (
    pair_id BLOB NOT NULL REFERENCES pair(id) ON DELETE CASCADE,
    side TEXT NOT NULL CHECK(side IN ('left','right')),
    path BLOB NOT NULL CHECK(length(path) > 0 AND substr(path,1,1) != x'2f' AND instr(path,x'00') = 0),
    fingerprint BLOB NOT NULL,
    version BLOB NOT NULL,
    PRIMARY KEY (pair_id, side, path)
) STRICT, WITHOUT ROWID;
";
// Exclusion rules, one per line.
const SCHEMA_V8: &str = "
ALTER TABLE pair ADD COLUMN filters TEXT NOT NULL DEFAULT '' CHECK(instr(filters, char(0)) = 0);
";
// Finished runs; times are milliseconds since the Unix epoch.
const SCHEMA_V9: &str = "
CREATE TABLE run_history (
    pair_id BLOB NOT NULL REFERENCES pair(id) ON DELETE CASCADE,
    started_ms INTEGER NOT NULL CHECK(started_ms >= 0),
    finished_ms INTEGER NOT NULL CHECK(finished_ms >= started_ms),
    trigger TEXT NOT NULL CHECK(trigger IN ('manual','scheduled')),
    outcome TEXT NOT NULL CHECK(outcome IN ('completed','cancelled','needs_approval','failed')),
    copied INTEGER NOT NULL CHECK(copied >= 0),
    updated INTEGER NOT NULL CHECK(updated >= 0),
    archived INTEGER NOT NULL CHECK(archived >= 0),
    recovered INTEGER NOT NULL CHECK(recovered >= 0),
    conflicts INTEGER NOT NULL CHECK(conflicts >= 0),
    failures INTEGER NOT NULL CHECK(failures >= 0),
    archive TEXT,
    message TEXT,
    PRIMARY KEY (pair_id, started_ms)
) STRICT, WITHOUT ROWID;
";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("State I/O: {0}")]
    Io(#[from] io::Error),
    #[error("State database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("State directory or file has unsafe ownership, type or permissions")]
    UnsafeLocation,
    #[error("State storage must be on a supported local filesystem")]
    NonlocalStorage,
    #[error("Another agent owns the state writer lock: {0}")]
    WriterLocked(String),
    #[error("Refusing an unrecognized database; existing data was not migrated")]
    ForeignDatabase,
    #[error("State requires schema reader {found}; this build supports {supported}")]
    UnsupportedReader { found: i64, supported: i64 },
    #[error("Invalid state metadata: {0}")]
    InvalidState(&'static str),
    #[error("Pair does not exist")]
    PairNotFound,
    #[error("Pair configuration changed; refresh before updating it")]
    StaleRevision,
}

/// Endpoint identity bytes, unknown until a pair's first sync.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IdentityRow {
    pub left: Option<Vec<u8>>,
    pub right: Option<Vec<u8>>,
}

/// One stored conflict choice. Version bytes are opaque to this driver.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolutionRow {
    pub path: Vec<u8>,
    pub keep: String,
    pub left_version: Option<Vec<u8>>,
    pub right_version: Option<Vec<u8>>,
}

/// One side's stored observation of a path. Bytes are opaque to this driver.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotRow {
    pub path: Vec<u8>,
    pub fingerprint: Vec<u8>,
    pub version: Vec<u8>,
}

/// One stored base row. Version bytes are opaque to this driver.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BaseRow {
    pub path: Vec<u8>,
    pub left_version: Vec<u8>,
    pub right_version: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
pub enum BaseChange<'a> {
    Put {
        path: &'a [u8],
        left_version: &'a [u8],
        right_version: &'a [u8],
    },
    Remove {
        path: &'a [u8],
    },
}

#[derive(Clone, Debug)]
pub enum StateLocation {
    Default,
    Directory(PathBuf),
}

impl StateLocation {
    fn directory(&self) -> Result<PathBuf, Error> {
        if let Self::Directory(path) = self {
            if !path.is_absolute() {
                return Err(Error::InvalidState("State directory must be absolute"));
            }
            return Ok(path.clone());
        }
        #[cfg(target_os = "macos")]
        let path = home()?.join("Library/Application Support/Foldover");
        #[cfg(not(target_os = "macos"))]
        let path = match std::env::var_os("XDG_DATA_HOME") {
            Some(base) => PathBuf::from(base).join("foldover"),
            None => home()?.join(".local/share/foldover"),
        };
        if !path.is_absolute() {
            return Err(Error::InvalidState("State base directory must be absolute"));
        }
        Ok(path)
    }
}

fn home() -> Result<PathBuf, Error> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(Error::InvalidState("HOME is unavailable"))
}

/// Storage records stay separate from validated domain configuration.
#[derive(Clone, Debug)]
pub struct PairValues {
    pub name: String,
    pub left_root: Vec<u8>,
    pub right_root: Vec<u8>,
    pub mode: String,
    pub modification_times: String,
    pub extended_attributes: String,
    pub permissions: String,
    pub durability: String,
    pub schedule: String,
    pub schedule_minutes: Option<i64>,
    pub filters: String,
}

#[derive(Clone, Debug)]
pub struct PairRow {
    pub id: Vec<u8>,
    pub revision: i64,
    pub values: PairValues,
}

pub struct Database {
    connection: Connection,
    _writer: File,
    directory: PathBuf,
}

#[derive(Clone, Copy)]
enum FileShape {
    Empty,
    Existing,
}

impl Database {
    pub fn open(location: &StateLocation) -> Result<Self, Error> {
        let directory = location.directory()?;
        DirBuilder::new()
            .recursive(true)
            .mode(DIRECTORY_MODE)
            .create(&directory)?;
        let metadata = fs::symlink_metadata(&directory)?;
        if !metadata.is_dir()
            || metadata.uid() != current_user()
            || metadata.mode() & PERMISSION_BITS != DIRECTORY_MODE
        {
            return Err(Error::UnsafeLocation);
        }
        // SQLite NOFOLLOW also rejects system parent aliases such as macOS /var.
        let directory = fs::canonicalize(directory)?;
        require_local(&File::open(&directory)?)?;
        let writer = private_file(&directory.join(WRITER_LOCK))?;
        writer
            .try_lock()
            .map_err(|error| Error::WriterLocked(error.to_string()))?;
        let path = directory.join(DATABASE_NAME);
        let file = private_file(&path)?;
        let shape = if file.metadata()?.len() == 0 {
            FileShape::Empty
        } else {
            FileShape::Existing
        };
        drop(file);

        // Compatibility checks precede writable SQLite opens and journal pragmas.
        let reader = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        let old_schema = inspect(&reader, shape)?;
        drop(reader);
        let mut connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        let schema = inspect(&connection, shape)?;
        if schema != old_schema {
            return Err(Error::InvalidState("Schema changed while opening state"));
        }
        if schema < SCHEMA_VERSION {
            if matches!(shape, FileShape::Existing) {
                backup(&connection, &directory, schema)?;
            }
            migrate(&mut connection, schema, &MIGRATIONS)?;
        }
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        let mode: String =
            connection.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(Error::InvalidState("SQLite refused WAL mode"));
        }
        Ok(Self {
            connection,
            _writer: writer,
            directory,
        })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn create_pair(&mut self, values: &PairValues) -> Result<PairRow, Error> {
        let id = uuid::Uuid::new_v4().into_bytes();
        self.connection.execute(
            "INSERT INTO pair (id,revision,name,left_root,right_root,mode,modification_times,extended_attributes,permissions,durability,schedule,schedule_minutes,filters) VALUES (?1,1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                id.as_slice(),
                values.name,
                values.left_root,
                values.right_root,
                values.mode,
                values.modification_times,
                values.extended_attributes,
                values.permissions,
                values.durability,
                values.schedule,
                values.schedule_minutes,
                values.filters
            ],
        )?;
        self.pair(&id)?
            .ok_or(Error::InvalidState("Inserted pair disappeared"))
    }

    pub fn pairs(&self) -> Result<Vec<PairRow>, Error> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {PAIR_COLUMNS} FROM pair ORDER BY name COLLATE BINARY,id"
        ))?;
        let rows = statement.query_map([], pair_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn pairs_after(&self, after: Option<&[u8]>, limit: usize) -> Result<Vec<PairRow>, Error> {
        let limit = i64::try_from(limit)
            .ok()
            .filter(|limit| *limit > 0)
            .ok_or(Error::InvalidState("Pair page limit must be positive"))?;
        let mut statement = self.connection.prepare(&format!(
            "SELECT {PAIR_COLUMNS} FROM pair WHERE ?1 IS NULL OR id > ?1 ORDER BY id LIMIT ?2"
        ))?;
        let rows = statement.query_map(params![after, limit], pair_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn pair(&self, id: &[u8]) -> Result<Option<PairRow>, Error> {
        Ok(self
            .connection
            .query_row(
                &format!("SELECT {PAIR_COLUMNS} FROM pair WHERE id=?1"),
                [id],
                pair_row,
            )
            .optional()?)
    }

    pub fn update_pair(
        &mut self,
        id: &[u8],
        revision: i64,
        values: &PairValues,
    ) -> Result<PairRow, Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // A base and conflict choices hold versions of these roots under
        // these metadata categories; changing either invalidates them.
        for table in ["base_entry", "resolution", "snapshot_entry"] {
            transaction.execute(
                &format!("DELETE FROM {table} WHERE pair_id=?1 AND EXISTS (SELECT 1 FROM pair WHERE id=?1 AND revision=?2 AND (left_root!=?3 OR right_root!=?4 OR modification_times!=?5 OR extended_attributes!=?6 OR permissions!=?7))"),
                params![
                    id,
                    revision,
                    values.left_root,
                    values.right_root,
                    values.modification_times,
                    values.extended_attributes,
                    values.permissions
                ],
            )?;
        }
        let changed = transaction.execute(
            // A changed root is a different endpoint; forget its identity.
            "UPDATE pair SET revision=revision+1,name=?3,left_root=?4,right_root=?5,mode=?6,modification_times=?7,extended_attributes=?8,permissions=?9,durability=?10,schedule=?11,schedule_minutes=?12,filters=?13,left_identity=CASE WHEN left_root=?4 THEN left_identity END,right_identity=CASE WHEN right_root=?5 THEN right_identity END WHERE id=?1 AND revision=?2",
            params![
                id,
                revision,
                values.name,
                values.left_root,
                values.right_root,
                values.mode,
                values.modification_times,
                values.extended_attributes,
                values.permissions,
                values.durability,
                values.schedule,
                values.schedule_minutes,
                values.filters
            ],
        )?;
        if changed == 0 {
            return Err(revision_error(&transaction, id)?);
        }
        let row = transaction.query_row(
            &format!("SELECT {PAIR_COLUMNS} FROM pair WHERE id=?1"),
            [id],
            pair_row,
        )?;
        transaction.commit()?;
        Ok(row)
    }

    /// The pair's recorded endpoint identities.
    pub fn identities(&self, pair: &[u8]) -> Result<IdentityRow, Error> {
        self.connection
            .query_row(
                "SELECT left_identity,right_identity FROM pair WHERE id=?1",
                [pair],
                |row| {
                    Ok(IdentityRow {
                        left: row.get(0)?,
                        right: row.get(1)?,
                    })
                },
            )
            .optional()?
            .ok_or(Error::PairNotFound)
    }

    /// Record identities, only while the pair keeps `revision`.
    pub fn record_identities(
        &mut self,
        pair: &[u8],
        revision: i64,
        left: &[u8],
        right: &[u8],
    ) -> Result<(), Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if transaction.execute(
            "UPDATE pair SET left_identity=?3,right_identity=?4 WHERE id=?1 AND revision=?2",
            params![pair, revision, left, right],
        )? == 0
        {
            return Err(revision_error(&transaction, pair)?);
        }
        transaction.commit()?;
        Ok(())
    }

    /// One side's stored observation, ordered by path.
    pub fn snapshot(&self, pair: &[u8], side: &str) -> Result<Vec<SnapshotRow>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT path,fingerprint,version FROM snapshot_entry WHERE pair_id=?1 AND side=?2 ORDER BY path",
        )?;
        let rows = statement.query_map(params![pair, side], |row| {
            Ok(SnapshotRow {
                path: row.get(0)?,
                fingerprint: row.get(1)?,
                version: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Replace one side's observation, only while the pair keeps `revision`.
    pub fn replace_snapshot(
        &mut self,
        pair: &[u8],
        revision: i64,
        side: &str,
        rows: &[SnapshotRow],
    ) -> Result<(), Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<i64> = transaction
            .query_row("SELECT revision FROM pair WHERE id=?1", [pair], |row| {
                row.get(0)
            })
            .optional()?;
        match current {
            None => return Err(Error::PairNotFound),
            Some(current) if current != revision => return Err(Error::StaleRevision),
            Some(_) => {}
        }
        transaction.execute(
            "DELETE FROM snapshot_entry WHERE pair_id=?1 AND side=?2",
            params![pair, side],
        )?;
        {
            let mut insert = transaction.prepare(
                "INSERT INTO snapshot_entry (pair_id,side,path,fingerprint,version) VALUES (?1,?2,?3,?4,?5)",
            )?;
            for row in rows {
                insert.execute(params![pair, side, row.path, row.fingerprint, row.version])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn resolutions(&self, pair: &[u8]) -> Result<Vec<ResolutionRow>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT path,keep,left_version,right_version FROM resolution WHERE pair_id=?1 ORDER BY path",
        )?;
        let rows = statement.query_map([pair], |row| {
            Ok(ResolutionRow {
                path: row.get(0)?,
                keep: row.get(1)?,
                left_version: row.get(2)?,
                right_version: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Store or replace a conflict choice, only while the pair keeps `revision`.
    pub fn record_resolution(
        &mut self,
        pair: &[u8],
        revision: i64,
        row: &ResolutionRow,
    ) -> Result<(), Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<i64> = transaction
            .query_row("SELECT revision FROM pair WHERE id=?1", [pair], |row| {
                row.get(0)
            })
            .optional()?;
        match current {
            None => return Err(Error::PairNotFound),
            Some(current) if current != revision => return Err(Error::StaleRevision),
            Some(_) => {}
        }
        transaction.execute(
            "INSERT INTO resolution (pair_id,path,keep,left_version,right_version) VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(pair_id,path) DO UPDATE SET keep=excluded.keep,left_version=excluded.left_version,right_version=excluded.right_version",
            params![pair, row.path, row.keep, row.left_version, row.right_version],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Every base row of a pair, ordered by raw path bytes.
    pub fn base_rows(&self, pair: &[u8]) -> Result<Vec<BaseRow>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT path,left_version,right_version FROM base_entry WHERE pair_id=?1 ORDER BY path",
        )?;
        let rows = statement.query_map([pair], |row| {
            Ok(BaseRow {
                path: row.get(0)?,
                left_version: row.get(1)?,
                right_version: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Apply base changes atomically, only while the pair keeps `revision`.
    /// A stale revision means the plan was built for other roots or mode.
    pub fn change_base(
        &mut self,
        pair: &[u8],
        revision: i64,
        changes: &[BaseChange<'_>],
    ) -> Result<(), Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<i64> = transaction
            .query_row("SELECT revision FROM pair WHERE id=?1", [pair], |row| {
                row.get(0)
            })
            .optional()?;
        match current {
            None => return Err(Error::PairNotFound),
            Some(current) if current != revision => return Err(Error::StaleRevision),
            Some(_) => {}
        }
        {
            let mut put = transaction.prepare(
                "INSERT INTO base_entry (pair_id,path,left_version,right_version) VALUES (?1,?2,?3,?4)
                 ON CONFLICT(pair_id,path) DO UPDATE SET left_version=excluded.left_version,right_version=excluded.right_version",
            )?;
            let mut remove =
                transaction.prepare("DELETE FROM base_entry WHERE pair_id=?1 AND path=?2")?;
            // A synced path's conflict choice is spent.
            let mut resolved =
                transaction.prepare("DELETE FROM resolution WHERE pair_id=?1 AND path=?2")?;
            for change in changes {
                match *change {
                    BaseChange::Put {
                        path,
                        left_version,
                        right_version,
                    } => {
                        put.execute(params![pair, path, left_version, right_version])?;
                        resolved.execute(params![pair, path])?;
                    }
                    BaseChange::Remove { path } => {
                        remove.execute(params![pair, path])?;
                        resolved.execute(params![pair, path])?;
                    }
                }
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn remove_pair(&mut self, id: &[u8], revision: i64) -> Result<(), Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if transaction.execute(
            "DELETE FROM pair WHERE id=?1 AND revision=?2",
            params![id, revision],
        )? == 0
        {
            return Err(revision_error(&transaction, id)?);
        }
        transaction.commit()?;
        Ok(())
    }
}

fn revision_error(connection: &Connection, id: &[u8]) -> Result<Error, Error> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM pair WHERE id=?1)",
        [id],
        |row| row.get(0),
    )?;
    Ok(if exists {
        Error::StaleRevision
    } else {
        Error::PairNotFound
    })
}

fn current_user() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn private_file(path: &Path) -> Result<File, Error> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(FILE_MODE)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != current_user()
        || metadata.mode() & PERMISSION_BITS != FILE_MODE
    {
        return Err(Error::UnsafeLocation);
    }
    Ok(file)
}

fn require_local(file: &File) -> Result<(), Error> {
    let filesystem = rustix::fs::fstatfs(file).map_err(io::Error::from)?;
    #[cfg(target_os = "macos")]
    if filesystem.f_flags & libc::MNT_LOCAL as u32 == 0 {
        return Err(Error::NonlocalStorage);
    }
    #[cfg(target_os = "linux")]
    {
        use linux_raw_sys::general::{
            BTRFS_SUPER_MAGIC, EXT4_SUPER_MAGIC, OVERLAYFS_SUPER_MAGIC, TMPFS_MAGIC,
            XFS_SUPER_MAGIC,
        };
        // Unknown/network/FUSE storage is not a proven WAL location.
        if ![
            BTRFS_SUPER_MAGIC,
            EXT4_SUPER_MAGIC,
            OVERLAYFS_SUPER_MAGIC,
            TMPFS_MAGIC,
            XFS_SUPER_MAGIC,
        ]
        .contains(&(filesystem.f_type as u32))
        {
            return Err(Error::NonlocalStorage);
        }
    }
    Ok(())
}

fn inspect(connection: &Connection, shape: FileShape) -> Result<i64, Error> {
    let application: i64 =
        connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let schema: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if schema < 0 {
        return Err(Error::InvalidState("Negative schema version"));
    }
    if application != APPLICATION_ID {
        let objects: i64 = connection.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )?;
        if matches!(shape, FileShape::Existing) || application != 0 || schema != 0 || objects != 0 {
            return Err(Error::ForeignDatabase);
        }
    }
    if schema > SCHEMA_VERSION {
        return Err(Error::UnsupportedReader {
            found: schema,
            supported: SCHEMA_VERSION,
        });
    }
    if schema > 0 {
        if application != APPLICATION_ID {
            return Err(Error::ForeignDatabase);
        }
        let minimum: i64 = connection.query_row(
            "SELECT minimum_reader FROM state_meta WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        if minimum <= 0 {
            return Err(Error::InvalidState("Invalid minimum reader"));
        }
        if minimum > SCHEMA_VERSION {
            return Err(Error::UnsupportedReader {
                found: minimum,
                supported: SCHEMA_VERSION,
            });
        }
    } else {
        let objects: i64 = connection.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )?;
        if objects != 0 {
            return Err(Error::ForeignDatabase);
        }
    }
    Ok(schema)
}

fn backup(connection: &Connection, directory: &Path, schema: i64) -> Result<(), Error> {
    let path = directory.join(format!(
        "state-before-v{schema}-{}.db",
        uuid::Uuid::new_v4()
    ));
    let destination = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(&path)?;
    drop(destination);
    connection.backup(rusqlite::MAIN_DB, &path, None)?;
    File::open(&path)?.sync_all()?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

/// Run every step after `from` in one transaction, so a failure leaves the
/// database at its previous schema.
fn migrate(connection: &mut Connection, from: i64, steps: &[&str]) -> Result<(), Error> {
    let from = usize::try_from(from).map_err(|_| Error::InvalidState("Negative schema version"))?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for statements in steps.get(from..).unwrap_or_default() {
        transaction.execute_batch(statements)?;
    }
    transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}

fn pair_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PairRow> {
    Ok(PairRow {
        id: row.get(0)?,
        revision: row.get(1)?,
        values: PairValues {
            name: row.get(2)?,
            left_root: row.get(3)?,
            right_root: row.get(4)?,
            mode: row.get(5)?,
            modification_times: row.get(6)?,
            extended_attributes: row.get(7)?,
            permissions: row.get(8)?,
            durability: row.get(9)?,
            schedule: row.get(10)?,
            schedule_minutes: row.get(11)?,
            filters: row.get(12)?,
        },
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    pub(super) fn location() -> (tempfile::TempDir, StateLocation) {
        let directory = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(DIRECTORY_MODE))
            .tempdir()
            .unwrap();
        let location = StateLocation::Directory(directory.path().to_owned());
        (directory, location)
    }

    pub(super) fn values() -> PairValues {
        PairValues {
            name: "pair".into(),
            left_root: b"/left".to_vec(),
            right_root: b"/right".to_vec(),
            mode: "sync".into(),
            modification_times: "sync".into(),
            extended_attributes: "sync".into(),
            permissions: "ignore".into(),
            durability: "standard".into(),
            schedule: "manual".into(),
            schedule_minutes: None,
            filters: String::new(),
        }
    }

    #[test]
    fn persists_pairs_in_wal_and_has_one_writer() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        assert!(matches!(
            Database::open(&location),
            Err(Error::WriterLocked(_))
        ));
        let saved = database.create_pair(&values()).unwrap();
        let mode: String = database
            .connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        drop(database);
        let database = Database::open(&location).unwrap();
        assert_eq!(database.pairs().unwrap().len(), 1);
        assert_eq!(
            database.pair(&saved.id).unwrap().unwrap().values.name,
            "pair"
        );
    }

    #[test]
    fn schema_constraints_and_revision_guard_preserve_existing_data() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let saved = database.create_pair(&values()).unwrap();
        let mut update = values();
        update.name = "'; DROP TABLE pair; --".into();
        let changed = database
            .update_pair(&saved.id, saved.revision, &update)
            .unwrap();
        assert_eq!(changed.revision, saved.revision + 1);
        assert!(matches!(
            database.update_pair(&saved.id, saved.revision, &values()),
            Err(Error::StaleRevision)
        ));
        assert_eq!(
            database.pair(&saved.id).unwrap().unwrap().values.name,
            update.name
        );
        update.left_root = b"relative".to_vec();
        assert!(database.create_pair(&update).is_err());
        update = values();
        update.mode = "unknown".into();
        assert!(database.create_pair(&update).is_err());
        assert_eq!(database.pairs().unwrap().len(), 1);
    }

    #[test]
    fn refuses_future_and_foreign_databases_without_changing_bytes() {
        for future in [true, false] {
            let (directory, location) = location();
            let path = directory.path().join(DATABASE_NAME);
            let file = private_file(&path).unwrap();
            drop(file);
            let connection = Connection::open(&path).unwrap();
            if future {
                connection
                    .pragma_update(None, "application_id", APPLICATION_ID)
                    .unwrap();
                connection
                    .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                    .unwrap();
            } else {
                connection
                    .execute_batch(
                        "CREATE TABLE unrelated(value TEXT); INSERT INTO unrelated VALUES('keep');",
                    )
                    .unwrap();
            }
            drop(connection);
            let before = fs::read(&path).unwrap();
            assert!(Database::open(&location).is_err());
            assert_eq!(fs::read(&path).unwrap(), before);
        }
    }

    #[test]
    fn rejects_symlink_files_and_unsafe_directories() {
        let (directory, location) = location();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), directory.path().join(DATABASE_NAME)).unwrap();
        assert!(Database::open(&location).is_err());
        assert_eq!(outside.as_file().metadata().unwrap().len(), 0);
        fs::remove_file(directory.path().join(DATABASE_NAME)).unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            Database::open(&location),
            Err(Error::UnsafeLocation)
        ));
    }

    #[test]
    fn resolves_parent_aliases_but_rejects_a_symlinked_state_directory() {
        let (directory, _) = location();
        let alias = directory.path().join("parent-alias");
        symlink(directory.path(), &alias).unwrap();
        let state = alias.join("state");
        let location = StateLocation::Directory(state);
        let mut database = Database::open(&location).unwrap();
        let saved = database.create_pair(&values()).unwrap();
        drop(database);
        let database = Database::open(&location).unwrap();
        assert_eq!(database.pair(&saved.id).unwrap().unwrap().id, saved.id);
        drop(database);

        let state_alias = directory.path().join("state-alias");
        symlink(directory.path().join("state"), &state_alias).unwrap();
        assert!(matches!(
            Database::open(&StateLocation::Directory(state_alias)),
            Err(Error::UnsafeLocation)
        ));
    }

    #[test]
    fn refuses_an_unmarked_existing_sqlite_container_even_without_tables() {
        let (directory, location) = location();
        let path = directory.path().join(DATABASE_NAME);
        drop(private_file(&path).unwrap());
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("VACUUM").unwrap();
        drop(connection);
        let before = fs::read(&path).unwrap();
        assert!(!before.is_empty());
        assert!(matches!(
            Database::open(&location),
            Err(Error::ForeignDatabase)
        ));
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn minimum_reader_refusal_preserves_current_schema_data() {
        let (directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        database.create_pair(&values()).unwrap();
        database
            .connection
            .execute(
                "UPDATE state_meta SET minimum_reader=?1",
                [SCHEMA_VERSION + 1],
            )
            .unwrap();
        drop(database);
        let path = directory.path().join(DATABASE_NAME);
        let before = fs::read(&path).unwrap();
        assert!(matches!(
            Database::open(&location),
            Err(Error::UnsupportedReader { .. })
        ));
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn online_backup_includes_committed_wal_rows() {
        let (directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let saved = database.create_pair(&values()).unwrap();
        assert!(
            fs::metadata(directory.path().join("state.db-wal"))
                .unwrap()
                .len()
                > 0
        );
        backup(&database.connection, directory.path(), SCHEMA_VERSION).unwrap();
        let prefix = format!("state-before-v{SCHEMA_VERSION}-");
        let entry = fs::read_dir(directory.path())
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
            .unwrap();
        let copy =
            Connection::open_with_flags(entry.path(), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let name: String = copy
            .query_row("SELECT name FROM pair WHERE id=?1", [saved.id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(name, saved.values.name);
    }

    #[test]
    fn initial_migration_backs_up_existing_container_and_rolls_back_bad_sql() {
        let (directory, location) = location();
        let path = directory.path().join(DATABASE_NAME);
        drop(private_file(&path).unwrap());
        let mut connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "application_id", APPLICATION_ID)
            .unwrap();
        assert!(
            migrate(
                &mut connection,
                0,
                &[SCHEMA_V1, "CREATE TABLE partial(value); INVALID SQL;"]
            )
            .is_err()
        );
        let partial: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='partial'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(partial, 0);
        drop(connection);
        let database = Database::open(&location).unwrap();
        assert!(database.pairs().unwrap().is_empty());
        let backups = fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("state-before-v0-")
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        let original =
            Connection::open_with_flags(backups[0].path(), OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        assert_eq!(
            original
                .pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            0
        );
    }

    fn put<'a>(path: &'a [u8], version: &'a [u8]) -> BaseChange<'a> {
        BaseChange::Put {
            path,
            left_version: version,
            right_version: version,
        }
    }

    #[test]
    fn base_changes_are_atomic_revision_bound_and_ordered() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        database
            .change_base(&pair.id, 1, &[put(b"b", b"2"), put(b"a", b"1")])
            .unwrap();
        let paths: Vec<_> = database
            .base_rows(&pair.id)
            .unwrap()
            .into_iter()
            .map(|row| row.path)
            .collect();
        assert_eq!(paths, [b"a".to_vec(), b"b".to_vec()]);

        database
            .change_base(
                &pair.id,
                1,
                &[put(b"a", b"3"), BaseChange::Remove { path: b"b" }],
            )
            .unwrap();
        assert_eq!(
            database.base_rows(&pair.id).unwrap(),
            [BaseRow {
                path: b"a".to_vec(),
                left_version: b"3".to_vec(),
                right_version: b"3".to_vec(),
            }]
        );

        // An invalid row rolls back the whole change.
        assert!(
            database
                .change_base(&pair.id, 1, &[put(b"c", b"4"), put(b"/absolute", b"5")])
                .is_err()
        );
        assert_eq!(database.base_rows(&pair.id).unwrap().len(), 1);

        assert!(matches!(
            database.change_base(&pair.id, 2, &[put(b"c", b"4")]),
            Err(Error::StaleRevision)
        ));
        assert!(matches!(
            database.change_base(&[7; 16], 1, &[put(b"c", b"4")]),
            Err(Error::PairNotFound)
        ));
        assert_eq!(database.base_rows(&pair.id).unwrap().len(), 1);
    }

    #[test]
    fn root_changes_and_removal_discard_the_base_but_renames_keep_it() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        database
            .change_base(&pair.id, 1, &[put(b"a", b"1")])
            .unwrap();

        let mut renamed = values();
        renamed.name = "renamed".into();
        renamed.mode = "mirror-left".into();
        database.update_pair(&pair.id, 1, &renamed).unwrap();
        assert_eq!(database.base_rows(&pair.id).unwrap().len(), 1);

        // A stale update changes nothing, including the base.
        let mut moved = renamed.clone();
        moved.right_root = b"/elsewhere".to_vec();
        assert!(matches!(
            database.update_pair(&pair.id, 1, &moved),
            Err(Error::StaleRevision)
        ));
        assert_eq!(database.base_rows(&pair.id).unwrap().len(), 1);

        database.update_pair(&pair.id, 2, &moved).unwrap();
        assert!(database.base_rows(&pair.id).unwrap().is_empty());

        database
            .change_base(&pair.id, 3, &[put(b"a", b"1")])
            .unwrap();
        database.remove_pair(&pair.id, 3).unwrap();
        let orphans: i64 = database
            .connection
            .query_row("SELECT count(*) FROM base_entry", [], |row| row.get(0))
            .unwrap();
        assert_eq!(orphans, 0);
    }

    #[test]
    fn version_one_state_migrates_with_a_backup_and_keeps_pairs() {
        let (directory, location) = location();
        let path = directory.path().join(DATABASE_NAME);
        drop(private_file(&path).unwrap());
        let mut connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "application_id", APPLICATION_ID)
            .unwrap();
        let transaction = connection.transaction().unwrap();
        transaction.execute_batch(SCHEMA_V1).unwrap();
        transaction.pragma_update(None, "user_version", 1).unwrap();
        transaction.commit().unwrap();
        connection
            .execute(
                "INSERT INTO pair (id,revision,name,left_root,right_root,mode) VALUES (?1,1,'old',x'2f6c',x'2f72','sync')",
                [[9_u8; 16].as_slice()],
            )
            .unwrap();
        drop(connection);

        let mut database = Database::open(&location).unwrap();
        assert_eq!(database.pairs().unwrap()[0].values.name, "old");
        database
            .change_base(&[9; 16], 1, &[put(b"a", b"1")])
            .unwrap();
        let backups = fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("state-before-v1-")
            })
            .count();
        assert_eq!(backups, 1);
    }

    #[test]
    fn identities_are_revision_bound_and_forgotten_per_changed_root() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        assert_eq!(
            database.identities(&pair.id).unwrap(),
            IdentityRow::default()
        );
        assert!(matches!(
            database.record_identities(&pair.id, 2, &[1; 16], &[2; 16]),
            Err(Error::StaleRevision)
        ));
        assert!(
            database
                .record_identities(&pair.id, 1, &[1; 15], &[2; 16])
                .is_err()
        );
        database
            .record_identities(&pair.id, 1, &[1; 16], &[2; 16])
            .unwrap();

        let mut renamed = values();
        renamed.name = "renamed".into();
        database.update_pair(&pair.id, 1, &renamed).unwrap();
        assert_eq!(
            database.identities(&pair.id).unwrap(),
            IdentityRow {
                left: Some(vec![1; 16]),
                right: Some(vec![2; 16])
            }
        );
        let mut moved = renamed.clone();
        moved.right_root = b"/elsewhere".to_vec();
        database.update_pair(&pair.id, 2, &moved).unwrap();
        assert_eq!(
            database.identities(&pair.id).unwrap(),
            IdentityRow {
                left: Some(vec![1; 16]),
                right: None
            }
        );
        assert!(matches!(
            database.identities(&[9; 16]),
            Err(Error::PairNotFound)
        ));
    }

    #[test]
    fn resolutions_are_revision_bound_and_spent_by_base_changes() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let row = |path: &[u8]| ResolutionRow {
            path: path.to_vec(),
            keep: "left".into(),
            left_version: Some(b"l".to_vec()),
            right_version: None,
        };
        assert!(matches!(
            database.record_resolution(&pair.id, 2, &row(b"a")),
            Err(Error::StaleRevision)
        ));
        database.record_resolution(&pair.id, 1, &row(b"a")).unwrap();
        database.record_resolution(&pair.id, 1, &row(b"b")).unwrap();
        assert_eq!(
            database.resolutions(&pair.id).unwrap(),
            [row(b"a"), row(b"b")]
        );

        database
            .change_base(&pair.id, 1, &[put(b"a", b"1")])
            .unwrap();
        assert_eq!(database.resolutions(&pair.id).unwrap(), [row(b"b")]);

        let mut moved = values();
        moved.left_root = b"/moved".to_vec();
        database.update_pair(&pair.id, 1, &moved).unwrap();
        assert!(database.resolutions(&pair.id).unwrap().is_empty());
    }

    #[test]
    fn snapshots_replace_whole_sides_and_follow_the_base_rules() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let pair = database.create_pair(&values()).unwrap();
        let row = |path: &[u8]| SnapshotRow {
            path: path.to_vec(),
            fingerprint: b"f".to_vec(),
            version: b"v".to_vec(),
        };
        database
            .replace_snapshot(&pair.id, 1, "left", &[row(b"a"), row(b"b")])
            .unwrap();
        database
            .replace_snapshot(&pair.id, 1, "right", &[row(b"c")])
            .unwrap();
        database
            .replace_snapshot(&pair.id, 1, "left", &[row(b"b")])
            .unwrap();
        assert_eq!(database.snapshot(&pair.id, "left").unwrap(), [row(b"b")]);
        assert_eq!(database.snapshot(&pair.id, "right").unwrap(), [row(b"c")]);
        assert!(
            database
                .replace_snapshot(&pair.id, 1, "middle", &[row(b"d")])
                .is_err()
        );
        assert!(matches!(
            database.replace_snapshot(&pair.id, 2, "left", &[]),
            Err(Error::StaleRevision)
        ));

        let mut renamed = values();
        renamed.name = "renamed".into();
        database.update_pair(&pair.id, 1, &renamed).unwrap();
        assert_eq!(database.snapshot(&pair.id, "left").unwrap().len(), 1);
        let mut ignoring = renamed.clone();
        ignoring.modification_times = "ignore".into();
        database.update_pair(&pair.id, 2, &ignoring).unwrap();
        assert!(database.snapshot(&pair.id, "left").unwrap().is_empty());
        assert!(database.snapshot(&pair.id, "right").unwrap().is_empty());
    }
}
