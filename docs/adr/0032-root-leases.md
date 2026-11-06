# ADR 0032: Root leases

Status: Implemented

## Context

PLAN.md §4: several Macs may sync the same folder, such as a NAS share
two Macs pair with their own disks, or a folder another Mac shares. Two
runs writing one folder at once would each stage, archive and commit
against a plan the other is invalidating. The agent's own state lock
(PLAN.md §4.2) covers one Mac only. Shares hold no server process, and
SFTP offers no portable advisory locks, so the lock has to be a file in
the folder that any Mac can read and write.

## Decision

Each run holds a lease on both roots from before its first write until
it ends. A Mirror's or Backup's source is leased only when the run
writes there ([ADR 0047](0047-shared-one-way-sources.md)). The lease is `.foldover/lock`:

```text
foldover-lease 1
holder <agent instance>/<run name>   one run of one agent
label Studio                         the Mac's name, shown to people
beat 17                              moved by the holder as it works
```

```text
take:   create lock exclusively ──ok──▶ held
          │ exists
          ▼
        beat moved within a TTL? ──yes──▶ wait (poll), Busy after `wait`
          │ no: dead
          ▼
        create lock.claim exclusively ──exists──▶ claim still for a TTL?
          │ ok                                      remove it, retry
          ▼
        lock unchanged? ──no──▶ drop claim, retry
          │ yes
          ▼
        move claim over lock, re-read: ours? ──yes──▶ held
```

- **Liveness:** a lease is dead when its bytes stay unchanged across two
  reads one TTL apart, timed on the reader's clock, so Macs need no
  shared time. Holders move the beat every 15 s; the TTL is 120 s, long
  enough that one slow write between beats never looks dead. A run waits
  up to 150 s for a live lease, showing "Waiting for “Studio” to finish
  syncing these folders…" (`waiting_for` in a working job's progress,
  local protocol 10), then fails with "Studio is syncing this folder";
  scheduled runs retry on their cadence.
- **Takeover:** the exclusive claim file makes reclaiming a race with one
  winner; the move and re-read settle any remaining tie. A claim is a
  lease too, so one left by a Mac that died mid-takeover is removed by
  the same rule. Its observation clock starts alongside the lock's, so a
  crash leaving both records recovers within the ordinary wait rather
  than requiring two consecutive TTLs on every attempt. A claim beside a
  missing lock is observed and recovered before granting a free lease;
  otherwise the first beat would immediately lose to that orphaned claim.
- **Fencing:** each beat creates `lock.claim` exclusively with its next
  record, re-reads the exact lock, atomically promotes the claim over the
  live lock and verifies the promoted bytes. Promotion never removes the
  lock first. A competing claim or changed lock loses the lease. The run
  cancels at its next checkpoint, commits what it did, and fails with
  "Another Mac took over this folder".
- **Ordering:** a run takes its two leases in root-identity order, so two
  Macs syncing the same two folders cannot each hold one and wait on the
  other. Undo takes the same leases.
- **Release:** dropping a lease advances through the same claim protocol,
  then removes the promoted lock. A competing claim leaves the lock alone;
  a crash between promotion and removal leaves an ordinary live lease for
  TTL recovery (delaying a waiting takeover by up to one TTL), never a free
  lock beside a stale claim.
- **Transport:** local roots use exclusive create and atomic rename-replace
  on the held directory handle. SFTP opens the final lock or claim with
  `OPEN_CREATE|OPEN_EXCLUSIVE`; promotion uses `posix-rename@openssh.com`
  to atomically replace the live lock without a remove-then-rename gap.
  Both use a second session opened for leases: a transfer holds the main
  session while it calls back into the run, so a beat on it would wait for
  the transfer, or deadlock. A shared folder forwards each step to the Mac
  that shares it
  (`FolderOp::Lease`), so the file operations run there.
- A lock in any other format, such as one a newer version wrote, is
  honoured as live until it stays unchanged for a TTL.

## Consequences

- A run on a Mac that crashed or slept mid-sync holds its folders for up
  to two minutes; the next run waits that out and takes over.
- A Mac whose clock jumps cannot steal a live lease: only the beat and
  the reader's own monotonic clock count.
- Each beat costs three reads, one exclusive claim create and one promotion
  per root. A run on an SFTP folder opens a second SSH connection so a
  transfer holding the main one cannot block its lease traffic.
- An interrupted SFTP exclusive create can leave an incomplete lock. It is
  kept rather than removed by name, since a delayed cleanup could delete a
  successor's lock; the ordinary invalid-record TTL recovery reclaims it.
- Fencing runs at beats, not at every write: after a takeover, the old
  holder may write for up to one beat before it stops. Staged writes
  and archived replacements keep that window recoverable.
