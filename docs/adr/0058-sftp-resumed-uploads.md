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

- **Staged by content.** A file of at least 16 MiB copied from this Mac
  to an SFTP folder that keeps records is written to
  `.foldover/partial/<content digest>`, as ADR 0034 names local
  partials, and renamed into place when whole. A stop at any point, a
  kill included, leaves it there; the leftover pass never looks inside
  `.foldover`. A folder that keeps no records stages beside the file and
  keeps nothing. Two copies of one content in a run never share the
  file: the second stages beside its destination.
- **Resume after what the server acknowledged.** The writer sends a new
  write only once the oldest of 16 in flight was answered, so every byte
  more than 16 × 32 KiB before the kept file's end was acknowledged,
  in whatever order the server applied the writes. The next copy of the
  content starts there and sends the rest, overwriting the unanswered
  window. A kept file longer than the file it would become is not this
  content's and is replaced.
- **Trust the acknowledgements** (owner decision, 2026-11-15). Nothing
  is read back. A local source still reads the whole file, the kept part
  too, so the stored version's digest is the source's whole digest and
  the source cannot have changed unnoticed. What the server did with the
  acknowledged part is not verified: an acknowledgement is not
  durability, and a prefix the server lost or corrupted, for example in
  a NAS power loss, goes unnoticed until the file is next read in full,
  at its weekly reread under ADR 0057. The rejected alternative read the
  kept part back and hashed it. Local and paired-Mac partials keep
  ADR 0034's hash of the kept bytes.
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
- An abandoned partial occupies server space until the next copy of the
  same content or the archive's retention.
