# ADR 0043: Parallel copies

Status: Implemented

## Context

PLAN.md §4.7 runs siblings in parallel, and §4.9 sizes parallelism per
device: solid-state disks 4–8 workers, spinning disks 1–2, network 4–8
connections. Runs made every copy in turn on one thread, so a folder of
small files on a fast disk or a share waited on one file's latency at a
time.

## Decision

- **Workers per folder.** `Endpoint::workers` says how many copies a
  folder takes at once, and a run uses the smaller of its two folders'.
  A local folder asks what holds its volume (`Root::medium`): the I/O
  Registry's "Medium Type" of the disk on macOS, the block queue's
  `rotational` flag on Linux.

  | Folder | Copies at once |
  |---|---|
  | Solid-state disk | 4 |
  | Mounted share (SMB, NFS, AFP, WebDAV) | 4 |
  | Spinning disk, or one that does not say | 1 |
  | SFTP server | 4; observation reads use up to eight sessions from a pool capped at nine (eight readers and the run's thread), plus a separate lease session, so at most ten; 1 when Foldover cannot open more |
  | Two SFTP servers in one pair | 1 copy: a copy holds a session on each, and copies in opposite directions could wait on each other once a server refuses more sessions. Observations stay sequential, one root at a time, each on its own endpoint's pool |
  | Another Mac | 4, one link stream each; a link takes 64 streams, so change reports and lease beats always find one |

  Observation reads on an SFTP folder do not use this copy count: they read
  on up to eight pooled sessions, while copies still take the smaller of the
  two folders' counts.

- **One recorder.** The run's thread keeps planning order, the journal,
  the base, commits, progress and the control (cancel, lease beats). It
  hands copies of files and links to workers and records each as it
  lands. Folders and moves stay on the run's thread, so a copy is
  handed out only after its folder was made. Independent file and link
  archives may use the same bounded workers after every copy has landed.
  Before removing any folder, the run drains all outstanding archives;
  reverse path order still puts children before their parent. The thread
  alone records archive results, including failures and base updates.
  While workers run, the thread asks the control
  every 100 ms; a cancel or a lost lease stops copies in progress and
  those not started, and the run ends cancelled.
- **Unchanged with one worker.** A run whose folders take one copy at a
  time makes every write on its own thread in plan order, as before, so
  simulation seeds over such folders still replay exactly.
- **Per run, not per device.** Two pairs on one disk each use its count.
  A spinning disk still sees one stream per running pair, as before.

## Consequences

- Copies of many small files to a solid-state disk or a share overlap.
- A crash can leave up to one batch of copies done but unrecorded, as
  before: the next run finds them equal on both sides and records them.
- A worker's panic stops the other writes and fails the run as a
  serial panic did. Cancellation and commit failures drain completed
  archives through the same recorder as completed copies.
- In-memory folders can take several workers; a simulation over them
  runs on threads and a failing seed may not replay exactly.
- **SFTP sessions.** A server's sessions are pooled: a borrower takes
  an idle one, opens another below the limit, or waits for one to come
  back, and holds one at a time, so a handle stays on the session that
  opened it. Observation reads use a pool capped at nine (eight readers and
  the run's thread); the lease beats on a separate session, so an endpoint
  opens at most ten. A server that refuses another session is shared by the
  ones it took, and refusals clamp the pool to what the server took.
- Follow-up: a budget shared by pairs on the same device.

## Archive extension qualification

The deletion-heavy LAN diagnostic archived 1,000 files among 10,000 tracked
files in 95.9 seconds of apply time, out of 154.5 seconds overall. This uses
separate synthetic local/NAS folders on a shared host and establishes that
the serial archive phase is material; it is not a paired speedup claim.
The extension needs paired LAN measurements and regressions for overlap,
copy and folder barriers, failure, cancellation, worker panic and undo
before promotion. Single-worker runs and two-SFTP pairs keep their existing
serial behavior. Root-check cadence, commit size and durability stay fixed.
