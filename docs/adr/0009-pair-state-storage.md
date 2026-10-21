# ADR 0009: Pair configuration and state writer ownership

Status: Implemented initial store and agent ownership

## Scope

Version 1 stores local pair configuration: opaque identity, revision,
name, selected root bytes, mode and source direction. Version 2 adds the
per-path base. Version 3 adds per-pair options: three metadata categories
and durability; existing pairs get the defaults. Version 4 adds each side's
endpoint identity, recorded on the first sync and forgotten when that side's
root changes ([ADR 0020](0020-run-execution.md)). Version 5 adds the
schedule ([ADR 0022](0022-scheduled-runs.md)). Version 6 adds conflict
choices ([ADR 0023](0023-conflict-resolution.md)). Version 7 adds each
side's last observation ([ADR 0018](0018-local-entry-versions.md)), a cache
discarded with the base. Version 8 adds filter rules
([ADR 0024](0024-filters.md)). Version 9 adds run history
([ADR 0026](0026-run-history.md)). Version 10 adds the file size limit
([ADR 0024](0024-filters.md)). Version 11 adds the automation pause
([ADR 0022](0022-scheduled-runs.md)). Version 12 adds each side's network
mount source ([ADR 0027](0027-share-reconnect.md)). Version 13 adds the
age limit ([ADR 0024](0024-filters.md)). Version 14 permits SFTP locator bytes
alongside absolute local roots ([ADR 0028](0028-sftp-endpoints.md)). Secrets are not stored yet.
Configuration does not establish endpoint identity, overlap safety,
permissions or readiness to sync.

`foldover-types` validates configuration values. `foldover-state` translates
storage records into those values and rejects invalid records. Raw SQLite,
file operations, migrations and backups stay in `foldover-db`.

## Storage and compatibility

- Default paths are macOS Application Support and Linux XDG data storage.
  The directory must be owned by the user with mode 0700; database and writer
  lock files require 0600 and are opened without following symlinks.
  After rejecting a symlinked state directory, resolve parent aliases such
  as macOS `/var` before SQLite's all-components no-symlink open.
- An exclusive state-directory lock precedes database access. It is separate
  from the health agent's runtime/socket lock.
- Agent startup opens the store through service/engine/state and retains the
  writer guard for the server lifetime. Another socket cannot acquire the
same store. Startup failures are explicit. Configuration dispatch is covered by
[ADR 0011](0011-pair-configuration-api.md).
- On macOS the state directory carries Time Machine's sticky exclusion
  (PLAN.md §4.7), set when the store opens if missing: a restored base
  older than the folders could carry stale deletions over. Setting it is
  best effort; the store works without it.
- WAL, FULL synchronous mode and foreign-key enforcement are required.
  macOS requires a local-volume flag. Linux currently supports ext-family,
  btrfs, XFS, overlay and tmpfs; unknown, network and FUSE storage is refused.
- A read-only compatibility check precedes writable SQLite opens and journal
  changes. Foreign databases and unsupported schema/minimum-reader versions
  are refused. They are not relabeled or migrated.
- Existing supported containers get a unique SQLite online backup before
  migration. Backup file and directory metadata are flushed. Migration runs
  in one transaction; failure rolls back while retaining the backup.
- Migrations are append-only steps; opening an older database runs every
  later step in one transaction. There is no destructive contraction or
  automatic downgrade. Restore decisions require stopping the writer and
  selecting a verified backup; this slice does not add a restore command.
- Version 14 rebuilds the pair table without changing IDs, revisions or child
  records. Foreign-key cascades are disabled before that transaction, references
  are checked before commit, and enforcement is restored before serving requests.
  Older readers refuse schema 14; its minimum reader is also 14.
- Version 15 adds the run journal ([ADR 0029](0029-undo.md)) and rebuilds
  `run_history` to allow undo rows; its minimum reader is 15.

## Pair updates

IDs are generated in the driver. Names and roots are bound SQL parameters;
raw root bytes are BLOBs, preserving non-UTF-8 paths. Schema constraints
reject invalid IDs, root syntax, modes and revisions. Domain validation
adds Unicode-aware name checks and preserves all five mode/direction forms.

Updates require the expected revision. A stale request leaves the new
configuration unchanged. Missing pairs and invalid stored values are errors,
never silently omitted rows. Removal requires the current revision and leaves
endpoint data in place; its API scope is in [ADR 0011](0011-pair-configuration-api.md).

## Base

`base_entry` holds one row per pair and path: raw path bytes and each
side's 65-byte version (kind, content digest, metadata digest). Removing a
pair cascades to its base. Changing either root or any metadata category
deletes the base in the same transaction; renames, mode and durability
changes keep it. Base updates are
atomic and require the pair's current revision, so a run planned for other
roots or another mode cannot commit. Stored rows whose sides disagree on
content, or whose kind or length is invalid, are errors.

## Observation cache

Snapshot versions and fingerprints only avoid rereading unchanged files.
A malformed cached version is a cache miss, so the next full observation
can replace it. Invalid snapshot paths and database failures still stop the
read. Authoritative base versions, identities, pair configuration and
conflict choices retain their strict validation.

## Verification

Driver tests cover reopen durability, single-writer ownership, SQL/schema
constraints, stale updates, foreign/future refusal with unchanged database
bytes, minimum-reader refusal, symlink/permission rejection, migration
rollback, committed-WAL backup contents, v1→v2 migration with a backup,
base ordering, atomic rollback, revision binding and cascades. State tests cover every mode,
raw paths, stale revisions and invalid domain records.

Linux checks pass locally. macOS filesystem/backup behavior is checked by CI.
Complete schema, journal crash recovery and release reliability gates remain
required.
