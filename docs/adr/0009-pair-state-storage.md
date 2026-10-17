# ADR 0009: Pair configuration and state writer ownership

Status: Implemented library; agent/API integration pending

## Scope

The first state format stores local pair configuration only: opaque
identity, revision, name, selected root bytes, mode and source direction.
It does not store file snapshots, bases, journal entries or secrets yet.
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
- WAL, FULL synchronous mode and foreign-key enforcement are required.
  macOS requires a local-volume flag. Linux currently supports ext-family,
  btrfs, XFS, overlay and tmpfs; unknown, network and FUSE storage is refused.
- A read-only compatibility check precedes writable SQLite opens and journal
  changes. Foreign databases and unsupported schema/minimum-reader versions
  are refused. They are not relabeled or migrated.
- Existing supported containers get a unique SQLite online backup before
  migration. Backup file and directory metadata are flushed. Migration runs
  in one transaction; failure rolls back while retaining the backup.
- The initial format is version 1. There is no destructive contraction or
  automatic downgrade. Restore decisions require stopping the writer and
  selecting a verified backup; this slice does not add a restore command.

## Pair updates

IDs are generated in the driver. Names and roots are bound SQL parameters;
raw root bytes are BLOBs, preserving non-UTF-8 paths. Schema constraints
reject invalid IDs, root syntax, modes and revisions. Domain validation
adds Unicode-aware name checks and preserves all five mode/direction forms.

Updates require the expected revision. A stale request leaves the new
configuration unchanged. Missing pairs and invalid stored values are errors,
never silently omitted rows. No pair configuration deletes are added yet.

## Verification

Driver tests cover reopen durability, single-writer ownership, SQL/schema
constraints, stale updates, foreign/future refusal with unchanged database
bytes, minimum-reader refusal, symlink/permission rejection, migration
rollback and committed-WAL backup contents. State tests cover every mode,
raw paths, stale revisions and invalid domain records.

Linux checks pass locally. macOS filesystem/backup behavior is checked by CI.
Agent integration, complete schema, crash recovery and release reliability
gates remain required.
