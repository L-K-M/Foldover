# ADR 0057: Trusting remote stats on SFTP targets

Status: Implemented ([plan](../resume-interrupted-runs.md), W1)

## Context

Every run reads every file in an SFTP folder to prove its version
(ADR 0028): SFTP version 3 reports size, modification time in whole
seconds, mode and owner, but no file identity or change time, so a stat
cannot tell a file rewritten with its old size and time (`touch -r`,
`cp -p`, `rsync -t`) from an untouched one. For a backup of a home
folder to a NAS over SFTP, that is every byte of the backup read over
the network on every run, and again by a run continuing an interrupted
one (ADR 0055).

Local folders reuse a version while the stat, including identity and
change time, stays the same, outside a 3 s racy window, and reread it
about weekly (PLAN.md §4.8).

## Decision

- **Where.** A remote stat may prove a version only on a Backup's or
  Mirror's target that keeps records (owner decision, 2026-11-15).
  Only Foldover writes there, under a lease each run. On a source or a
  two-way folder, an edit that kept size and time would be overwritten
  or stay unseen for up to a week instead of raising a conflict. A
  target whose records stay with the source takes no lease, so another
  Mac could write it unnoticed (ADR 0040); it stays read. The engine
  passes each side's `StatTrust` into the observation.
- **Opt-in elsewhere.** A pair's owner may extend this to every SFTP
  folder of the pair (`remote_stats` = `everywhere`, schema v33, local
  protocol 25; **Skip unchanged SFTP files in every folder** in the
  editor, `--remote-stats everywhere` in the CLI), accepting that an
  edit there which keeps size, time, mode and owner is found within a
  week, and can be overwritten meanwhile instead of raising a conflict.
  Older agents ignore the column and read every remote file.
- **What proves it.** A regular file is reused unread when its listed
  stat carries both size and modification time, equals the stored
  fingerprint's size, time, mode and owner, the file was modified more
  than an hour before it was read, and it was read within its reread
  interval (half a week to a week, as local). Link targets are always
  read: a same-length retarget can keep every listed field.
- **Settling across clocks.** The modification time is the server's
  clock and the read time this Mac's. A fingerprint is settled only
  when the file's server time is more than one hour before this Mac's
  read time (owner decision, 2026-11-15), so a server clock up to an
  hour behind cannot make a same-second edit after the read look
  settled. One hour costs little: nearly every file in a backup is
  older. Skew of an hour or more is outside the guarantee.
- **Confirming reads settle.** On a trusting side, a read that finds
  the stored version records the new read time, so a fingerprint taken
  too soon after an edit, or one due for its weekly reread, can prove
  the file next time. Other sides keep the stored fingerprint, and
  their snapshot is not rewritten.
- **Failed writes forget.** A copy, archive or move that fails on a
  side drops that path's stored observation in the same transaction as
  the batch's base, so the next run reads it. A stale reused version
  fails there, since writes still verify the destination's content
  (ADR 0034), and would otherwise fail the same way until its weekly
  reread. A copy that fails on the content it read, a changed source or
  a digest that is not the planned one, drops the source side's as
  well: where a pair trusts every folder, the reused version may be the
  source's.

## Consequences

- A no-change run of a backup to SFTP lists every folder but opens no
  settled file.
- An edit on the target that keeps size, time, mode and owner, made by
  something other than Foldover, is found at the file's weekly reread.
  Written content is likewise first verified by a read at that
  cadence, never at write time: the transfer digest covers the bytes
  sent.
- The first run after this change reads everything and stores new read
  times for every file it finds unchanged.
- The simulation cannot cover this: it has no SFTP folder, and its
  memory folder's fingerprints never prove anything. Tests run against
  OpenSSH's `sftp-server` and count the requests.
