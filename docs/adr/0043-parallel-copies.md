# ADR 0043: Parallel copies

Status: Implemented for local disks and shares; SFTP servers and other
Macs copy one file at a time

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
  | SFTP server | 1: its one session takes requests in turn |
  | Another Mac | 1: its link's four streams also carry change reports and lease beats |

- **One recorder.** The run's thread keeps planning order, the journal,
  the base, commits, progress and the control (cancel, lease beats). It
  hands copies of files and links to workers and records each as it
  lands. Folders, moves and archives stay on the run's thread, so a copy
  is handed out only after its folder was made, and archives start once
  every copy has landed. While copies run, the thread asks the control
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
- A worker's panic stops the other copies and fails the run as a
  serial panic did.
- In-memory folders can take several workers; a simulation over them
  runs on threads and a failing seed may not replay exactly.
- Follow-ups: a session pool for SFTP servers, more streams on the
  Mac-to-Mac link, and a budget shared by pairs on the same device.
