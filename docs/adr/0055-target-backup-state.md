# ADR 0055: Keep one-way backup state on its target

Status: accepted

## Context

A production pair accumulated 7.8 million snapshot rows, 6.1 million baseline
rows and 4.7 million undo entries. Its local database occupied about 11 GiB,
with another 11 GiB in the retained upgrade backup. Backup retention alone
cannot fix this: per-file state grows with the backed-up tree.

The source Mac should keep configuration and small operational records.
The backup target should keep the retained catalog and undo information.
SQLite WAL requires shared memory on one host and cannot be placed on a NAS
share or accessed through SFTP. See [SQLite's WAL documentation](https://www.sqlite.org/wal.html).

## Decision

Backup and Mirror pairs with records enabled use the target's private
`.foldover/state/<pair-id>/` directory. Two-way Sync retains local storage
until it has a symmetric ownership policy. The explicit Source Only records
option remains an exception: it forbids Foldover metadata on a Backup target.

The target format has four immutable tables: baseline, left and right scan
caches, and the newest writing run's undo journal. Content-addressed blocks
are at most 1 MiB and carry a BLAKE3 digest, byte length and row count in their
table descriptors. Entries are length-delimited byte keys and values, with
an explicit tombstone. Paths retain their original bytes. A block reader
checks its checksum, lengths, row count and strict key order. Missing or
corrupt data is an error, never an empty baseline.

Baseline and journal updates use binary merge levels, at most 32. Each
batch is at most 4,096 entries; normal execution commits 256 changes at a
time. A merge streams one bounded block per occupied level. A bulk import
starts at a level sized for its row count, so the first tiny update does
not rewrite a multi-million-row baseline. Snapshot caches are streamed
replacement tables. No target SQLite file or downloaded working database
is needed, and no per-file persistent cache is kept on the source Mac.

## Publication and recovery

A checkpoint binds all four table roots, pair and target identities, a
migration epoch, generation, predecessor digest, journal run name and spent
conflict choices. Its filename includes its epoch, generation and checksum.
Table roots are bounded to 8 MiB each; a whole checkpoint is bounded to
16 MiB. The target directory is protected by a separate heartbeat lease,
using the existing takeover and fencing protocol. Ordinary content-root
leases still protect file changes.

Publication stages a complete private file, flushes it and publishes it by
rename to a new immutable name. A checkpoint is published only after
its referenced blocks. Local disks and mounted NAS shares use the filesystem
driver's file and directory flushes. SFTP uses exclusive create and rename,
plus `fsync@openssh.com` when supported. The extension flushes a file handle;
it does not provide a directory flush. Standard durability therefore still
depends on the server, and Full durability remains unavailable for SFTP.
See [OpenSSH's SFTP extensions](https://github.com/openssh/openssh-portable/blob/master/PROTOCOL).

Mac-to-Mac targets use peer protocol 5, with bounded binary data frames and
paged directory listings. Both Macs must be updated; older peers refuse the
connection before a catalog operation starts.

After exact checkpoint readback and another lease and identity check, the
agent commits its generation and digest locally. Spending conflict choices
shares that local transaction. Base and undo changes share the target
checkpoint. A lost rename response is accepted only after exact readback.
An ambiguous publication stops further commits in that session.

On restart, the acknowledged checkpoint must still exist and match its
checksum. Exactly one successor can be recovered if it names that checkpoint
as its predecessor; this covers a crash between target publication and local
acknowledgement. An older, missing, conflicting or unexpectedly advanced
catalog stops the run. There is no automatic fallback to an empty local
database. This also detects acknowledged data lost by a server after a crash.

After a verified job, garbage collection retains the current and previous
checkpoints and their blocks. It removes only recognized obsolete checkpoint,
block and staging filenames, under the catalog lease. Foreign files are
untouched. Cleanup failures are retried by later jobs.

## Migration and local disk space

Schema 31 adds a small target routing marker and raises the minimum reader
version. Older agents must not interpret removed local rows as an empty
baseline. An epoch is reserved locally before target writes. Changes to
roots, metadata policy, mode or record placement invalidate both the old
baseline and its marker; name and schedule edits preserve them.

The next preview or run streams a pinned, consistent legacy database view
directly to target blocks, including the retained undo journal. It reads
back the complete checkpoint and verifies all referenced blocks before
acknowledging migration. Until then, the legacy rows remain authoritative.
Interrupted imports leave reusable immutable blocks and retain local data.
The first migration can take time proportional to the existing catalog and
the target connection speed.

After acknowledgement and release of the pinned reader, the service purges
legacy rows in bounded background transactions. Once the live local database
is at most 16 MiB and has no pinned readers, transactional VACUUM and a WAL
checkpoint reclaim its free pages. A durably published compact recovery copy
then replaces large recognized pre-upgrade backups. Large remaining local
catalogs, including two-way pairs, defer this bounded compaction. Configuration,
endpoint identities, schedules, automation holds, watch cursors and the newest
100 history summaries per pair stay local. Local size no longer grows with
the file count of migrated one-way pairs; it is not a hard quota on every
possible configuration.

Target I/O and tree decoding run outside the engine mutex. The target catalog
must be reachable for preview, sync and undo. Leases are renewed during reads,
writes and long scans. Missing targets produce an explicit failure without
recreating local per-file state.

## Validation

Regression coverage checks merge tables against a reference map, tombstones,
multiple blocks, malformed input, missing and corrupt objects, failed writes,
migration of baseline and undo records, interrupted acknowledgement, epoch
changes and reclamation of the local database and upgrade backup. Endpoint
tests cover bounded private files, publication and lease isolation. Service
tests exercise the normal preview, sync and undo routes.

Power-loss behavior ultimately depends on the target filesystem or server
honoring its durability operations. Checksums and the local checkpoint detect
loss; they cannot restore target data that the storage device has lost.
