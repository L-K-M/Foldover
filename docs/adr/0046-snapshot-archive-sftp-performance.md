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
- **SFTP reads:** folders settle first, then files are read on up to four
  sessions of the existing pool. As in ADR 0043, only the run's thread
  records results and asks the run's control. Content verification is
  unchanged. Endpoints without a session pool stay serial. A read waiting
  for a session gives up when the run cancels, so a server that went away
  cannot hold up cancelling.

## Measurements

Local Linux host with heavy, varying unrelated load, synthetic data,
release builds. SFTP latency is simulated at 5 ms per server message. No
Mac, NAS or real WAN was measured.

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

## Consequences

- Another computer's change to an archive can take up to 30 seconds to
  show in pages after the first, as can a change a job makes before it
  ends. Put Back on an item that is gone fails with a clear error.
- An archive page answered from memory does not touch the folder, so a
  root that went away shows at the next walk.
- A single folder holding very many items is still listed once per 1,000
  items, so such a run lists slowly.
- A preview or sync on an SFTP folder can open up to four SSH connections
  for reading, plus the run's lease connection.
- Budgets `db_snapshot_unchanged_100k`, `db_snapshot_rewrite_100k` and
  `archive_pages_5k` guard the snapshot and archive costs.
