# ADR 0034: Resumed transfers

Status: Implemented for copies that land in a local folder, on this Mac or
on a paired one; copies to SFTP servers start over

## Context

PLAN.md §4.9 and P5: a large copy over the network that breaks off, when
a Mac sleeps, changes networks or a person cancels, should not start from
zero next time. PLAN.md sketches block lists for files over 16 MiB.

## Decision

The receiving folder keeps what arrived and the next copy sends only the
rest:

```text
copy breaks off after ≥ 16 MiB      .foldover/partial/<content digest>
next run: destination reports N     source sends bytes N.. ; destination
                                    hashes kept + new, places on a match
```

- **Kept by content:** a staged file that fails after at least 16 MiB is
  moved to `.foldover/partial/`, named by the planned content digest: when
  the data breaks off, when giving it its metadata fails, and when it
  arrived whole but the file it would replace changed meanwhile. The
  next copy of that content to the folder takes it up; copies of other
  content ignore it. Partials follow the archive's retention.
- **Verified whole:** the destination hashes the kept bytes and the new
  ones together; the plan's digest must match before the file is placed.
  A kept part that no longer matches fails that copy once and is gone,
  so the next copy starts over. No block list is exchanged: one digest
  over the file proves the same thing, and the kept bytes are read
  locally.
- **Sources read from an offset:** SFTP reads start at the offset. A
  local source, including one on the other Mac, still reads the skipped
  bytes to prove the whole file is the observed one, but sends only the
  rest. Another Mac's folder reports its kept length (`FolderOp::Partial`)
  and takes `from` with the incoming file; a read from it takes `from`.
- **SFTP sources are read once:** every destination checks a file's
  digest as it lands, so a copy from SFTP no longer hashes the source
  beforehand, which downloaded each file twice. The read still fails if
  the file's stat changes, and a change that keeps the stat fails the
  digest check.
- **SFTP destinations start over:** pipelined writes may land out of
  order and leave holes, and nothing on the server would hash them.

## Consequences

- A copy resumes only if the same content is planned again; an edited
  source starts over, and its stale partial is pruned with the archive.
- Resuming a local source rereads it from its start; on the other Mac
  that costs disk time, not network.
- A whole copy that failed while being placed, after the destination
  check, keeps nothing; the check comes first and catches the usual
  cause, an edit to the file being replaced.
