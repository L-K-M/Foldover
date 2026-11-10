# ADR 0046: Cheaper snapshots, polls, archive pages and SFTP observations

Status: Implemented

## Context

A performance audit found four costs that grow with a pair's size:

- Storing a snapshot deleted and reinserted every row of a side, under
  the engine lock: 3.42 s for 100,000 entries per side.
- A poll looked up each listed directory with a scan of the previous
  listing, which is quadratic in a folder's directories.
- Every archive page walked the whole run folder: 2.00 s for 5,000 items
  over 50 pages, against 56 ms for one complete listing.
- An SFTP observation read every file on one session, one at a time.

## Decision

- **Snapshots:** `replace_snapshot` merges the given rows with the stored
  ones in path order and writes only the difference, in the same
  transaction and revision check. SFTP observations keep a file's stored
  entry when its version and stat are unchanged; a fresh observation time
  in every entry would otherwise have changed every row.
- **Polls:** a listing is sorted by name, so a lookup is a binary search
  by name followed by a full comparison of that name's entries.
- **Archive walk:** a folder's key is `name/`, so a depth-first walk over
  sorted listings yields whole-path byte order. It stops once a page and
  one more item are found, and skips subtrees before the cursor. Local
  and SFTP folders share it.

  ```text
  a-b   a.txt   a/x   a0        '-' < '.' < '/' < '0'
  ```

- **Archive read-ahead:** the walk does not help one flat folder, or
  SFTP's new connection per page. The service therefore reads 1,000 items
  and answers the next pages from memory, from one run's window at a time,
  for at most 30 seconds. The first page always reads the folder. A job's start
  and end and the end of any mutation drop the pair's window, and a walk
  overlapping one of those keeps nothing. The window never answers with a
  short page while more follow: a walk would not, and an empty page ends
  the CLI's listing.
- **SFTP reads:** the walk lists one folder at a time, observes its folders
  on the caller and reads its files on up to eight pooled sessions. As in ADR 0043, only the run's thread
  records results and asks the run's control. The handoff holds at most
  twice the worker count of queued, running and completed-but-unrecorded
  owned jobs. Each folder's listing is released as its entries are handed
  off; the walk does not retain the whole tree's file metadata. A single
  wide flat folder is still materialized by `Session::read_dir`. Late
  directory failures suppress any already read descendants. Content
  verification is unchanged. Endpoints
  without a session pool stay serial. A read waiting for a session gives
  up when the run cancels, so a server that went away cannot hold up
  cancelling.

## Measurements

Synthetic data and release builds throughout the Linux and Mac tables. The
LAN no-change runs below are real SSH-over-LAN measurements against a NAS.

### Linux host

Local Linux host with heavy, varying unrelated load. SFTP latency is
simulated at 5 ms per server message.

| What | Before | After |
|---|---|---|
| Unchanged snapshot, 100,000 rows | 1.64 s | 46 ms |
| Every row of a snapshot changed, 100,000 rows | 1.59 s, 1.80 s | 1.50 s, 1.26 s |
| Poll lookups, 20,000 directories | 1.32 s | 6.38 ms |
| Archive pages at the endpoint, 5,000 items in 50 folders | 913 ms | 52 ms |
| Archive pages through the service, 5,000 items in one folder | 905 ms | 92 ms |
| 300 × 1 KiB SFTP files | 55.0 s, 58.4 s | 14.5 s, 14.2 s |
| 32 × 4 MiB SFTP files | 29.9 s, 30.7 s | 8.0 s, 6.3 s |

Two values are two runs. A third round of the full rewrite hit a host stall
on both old and new code and is left out. A GitHub runner's medians, which
the budgets allow for: 37 ms for the endpoint's archive pages, 45 ms for an
unchanged snapshot and 275 ms for a full rewrite.

### Mac

A Mac Studio (M4 Max, 14 cores, 36 GB, macOS 27.0.1), on its internal
APFS disk, shared with other builds at load averages between 40 and 185.
Snapshot and endpoint archive rows run the same benchmarks on `ccc4982`
and `d081589`. The other rows run on `d081589`, with the old behavior
restored for Before: polls search the listing linearly, the service walks
for every page and SFTP reads use one session. SFTP reads go
to the system's `/usr/libexec/sftp-server` through a relay that adds 5 ms
per server message, or nothing.

| What | Before | After |
|---|---|---|
| Unchanged snapshot, 100,000 rows | 470 ms, 629 ms | 42 ms, 103 ms |
| Every row of a snapshot changed, 100,000 rows | 701 ms, 653 ms | 544 ms, 549 ms |
| Poll lookups, 20,000 directories | 674 ms, 535 ms | 1.91 ms, 1.91 ms |
| Archive pages at the endpoint, 5,000 items in 50 folders | 2.12 s, 1.59 s | 107 ms, 75 ms |
| Archive pages through the service, 5,000 items in one folder | 644 ms | 175 ms |
| 300 × 1 KiB SFTP files, 5 ms per message | 66.6 s, 66.1 s | 16.9 s, 16.4 s |
| 32 × 4 MiB SFTP files, 5 ms per message | 29.3 s, 30.0 s | 7.8 s, 7.4 s |
| 300 × 1 KiB SFTP files, no added latency | 166 ms, 271 ms | 90 ms, 54 ms |
| 32 × 4 MiB SFTP files, no added latency | 503 ms, 763 ms | 106 ms, 60 ms |

Two values are two runs. Service pages are the median of 15 walks over
the run: 514 ms to 1.55 s before, 64 to 306 ms after.

### LAN no-change runs

On a shared macOS host (M4 Max, 14 cores, macOS 27.0.1) against a NAS over
SSH, three paired no-change runs of a 10k-file corpus (8 KiB files, 1,000 per
folder) measured the remote observation at 130.501-134.597 s with four readers
(median 133.013 s) and 69.623-76.648 s with eight (median 70.548 s). The host
was busy with other agents, so this is not an idle qualification. See
[perf/sftp.md](../../perf/sftp.md) for provenance, raw ranges and the
one-write read-completion batching runs.

## Consequences

- Another computer's change to an archive can take up to 30 seconds to
  show in pages after the first, as can a change a job makes before it
  ends. Put Back on an item that is gone fails with a clear error.
- An archive page answered from memory does not touch the folder, so a
  root that went away shows at the next walk.
- A single folder holding very many items is still listed once per 1,000
  items, so such a run lists slowly.
- A preview or sync on an SFTP folder can open up to eight SSH connections
  for reading from a pool capped at nine, plus a separate lease connection,
  for at most ten.
- Budgets `db_snapshot_unchanged_100k`, `db_snapshot_rewrite_100k` and
  `archive_pages_5k` guard the snapshot and archive costs.
