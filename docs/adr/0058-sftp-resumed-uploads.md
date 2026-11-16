# ADR 0058: Resumed copies to SFTP folders

Status: Implemented ([plan](../resume-interrupted-runs.md), W4)

## Context

ADR 0034 resumes a broken-off copy into a local folder, here or on a
paired Mac, and starts copies to SFTP servers over: pipelined writes may
land out of order, and nothing on the server hashes what it kept. A
copy the agent stopped during, at a restart or a crash, kept nothing
anywhere: the staged file sat beside the destination as a temporary,
and the next run's leftover pass deleted it. A backup of a large file to
a NAS over SFTP that the Mac restarts during therefore sends the whole
file again.

## Decision

- **Staged by content and source.** A file of at least 16 MiB copied
  from this Mac to an SFTP folder that keeps records is written to
  `.foldover/partial/<content digest>-<source state>` and renamed into
  place when whole. A stop at any point, a kill included, leaves it
  there; the leftover pass never looks inside `.foldover`. A folder that
  keeps no records stages beside the file and keeps nothing. Two copies
  of one content in a run never stage there at once: the second stages
  beside its destination.
- **Continued only from the same file, unwritten.** ADR 0034 names
  local partials by content alone, since their folder hashes what it
  kept. Nothing hashes what a server kept, and a copy that stops before
  its read ends never reaches the check that follows the read: had its
  source been written to meanwhile, the staged file would hold bytes of
  neither version under the old content's name. The source state is a
  digest of the file's size, modification time, change time and inode,
  as the copy found them on the file it opened. Every write to the file
  moves its change time, so only a copy of that file, not written
  since, finds the staged file: a duplicate, or the file put back as it
  was, stages anew. A file whose modification or change time lies within
  the last 3 s, where a coarse timestamp could hide another write,
  stages beside its destination and keeps nothing. The change time
  counts because a write that puts the modification time back shows in
  it alone. A copy whose source fails while it is read, changed for
  one, removes what it staged: nothing would continue it.
- **Resume after what the server acknowledged.** The writer sends a new
  write only once the oldest of 16 in flight was answered, so every byte
  more than 16 × 32 KiB before the kept file's end was acknowledged,
  in whatever order the server applied the writes. The copy continuing
  it starts there and sends the rest, overwriting the unanswered
  window. A kept file longer than the file it would become is not this
  content's and is replaced.
- **Trust the acknowledgements** (owner decision, 2026-11-15). Nothing
  is read back. A local source still reads the whole file, the kept part
  too, so the stored version's digest is the source's whole digest, and
  the source state says the kept part was read from these bytes. What
  the server did with the acknowledged part is not verified: an
  acknowledgement is not durability, and a prefix the server lost or
  corrupted, for example in a NAS power loss, goes unnoticed until the
  file is next read in full, at its weekly reread under ADR 0057. The
  rejected alternative read the kept part back and hashed it. Local and
  paired-Mac partials keep ADR 0034's hash of the kept bytes.
- **Only from this Mac.** An SFTP or paired-Mac source reading from an
  offset proves nothing about the skipped part, so their copies stage
  beside the file and start over, as before.
- **Retention.** Partials not written to for longer than the archive's
  retention are pruned with it, as local ones are.

## Consequences

- A restart during a large upload costs at most the unanswered window
  plus a local reread of the kept part.
- A whole upload that fails while being given its metadata or placed
  is discarded, as before; the checks before it catch the usual cause.
- An abandoned partial occupies server space until the same file,
  unwritten, is copied again, or until the archive's retention.
- A file whose change time moved between the stop and the next copy,
  by a permission change for example, starts over, as does the same
  content copied from another file.
- A file written, renamed or given other permissions in the 3 s before
  its copy begins stages beside its destination: that copy starts over
  if it is stopped, and the next one stages where it can continue.
- Where a volume numbers its files anew each time it is mounted, a
  staged upload is not found after a remount, and the copy starts over.
- The source state is as exact as the file system's stat. Where a file
  system keeps no change time of its own, a rewrite that restores size
  and modification time is not seen, as in a local folder's observation
  (PLAN.md §4.8). On a share, the server's clock sets both times and
  this Mac's measures the 3 s, so that margin shrinks by however far the
  server runs behind.
