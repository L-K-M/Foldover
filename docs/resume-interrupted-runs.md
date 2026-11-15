# Continuing interrupted runs: implementation plan

Written against `2e31ce29` (main after PR 283), revised after two review
rounds (see [Revision notes](#revision-notes)).
Motivation: a run killed by a reboot or an agent stop loses no committed
work today, but the run itself is gone. A manual pair waits for a
person, and the next run pays a re-observation of both trees that for
SFTP endpoints means re-reading every remote file. For large SFTP
backups "the next run finishes the work" can mean re-downloading the
whole backup first.

## What survives a stop today

- Completed work: base and run-journal commits land in batches of 256
  (`COMMIT_BATCH`, `crates/foldover-engine/src/run.rs:59`), bound to the
  pair revision. A crash loses at most one uncommitted batch of records;
  the next run sees equal content on both sides and only records it.
- Leftover temporaries and displaced originals are handled by the next
  run's leftover pass before planning: archived where the folder keeps
  an archive, deleted locally where it keeps none
  (`crates/foldover-endpoint/src/local/apply.rs:409-433`,
  `sftp.rs:1389-1398`). A paired-Mac `records=source` target refuses
  the leftover instead (`archived()` returns `Unsupported` under
  `Archiving::Off`, `remote.rs:548-556,855-859`), so it fails on every
  run: a pre-existing bug, listed under follow-ups.
- A killed run's `.foldover/lock` lease dies within one TTL (~120–150 s,
  ADR 0032). A successor waits on the existing lease path and reports a
  wait on its own previous instance, because the agent's instance ID is
  per process (`run.rs:175-178`); lease holders are `instance/run`.
- **Killed scheduled runs are already due at the first tick.** A killed
  run writes no history row (`history::record` runs only after the run
  returns, `crates/foldover-service/src/sync.rs:596`). After a restart
  the scheduler's in-memory `last` map is empty, so `due()` alone counts
  from the agent's start (`schedule.rs:241-250`), but `interval_passed`
  counts from the last *recorded* start (`:301-315`) and `daily` compares
  the latest moment with it (`:268-273`). The condition that started a
  scheduled run therefore still holds after its kill. A *manual* run
  killed on an `every`/`daily` pair does not make the pair due; it waits
  for the normal cadence. `changes` pairs re-run after start-up replay
  sees the killed run's own writes (ADR 0002/0022).
- Convergence under interruption is simulation-tested
  (`random_edits_and_interrupted_runs_lose_nothing_and_converge`).

## What is lost today

- **Manual runs: nothing restarts them.** Job state is in memory
  (`sync.rs` `State.jobs`). A killed manual run leaves no history row
  and no status; after a restart `status()` reports `Idle`
  (`sync.rs:281-285`).
- **In-flight copies never resume after a kill.** A staged
  `.foldover-tmp-*` left by SIGKILL goes to the leftover pass above, not
  back into a transfer, on every endpoint kind. `.foldover/partial`
  resume (ADR 0034) covers only live-run failures, where an error path
  moves the staged file there (`foldover-fs/src/root/write/partial.rs:114-147`).
  A reboot is not an error path.
- **The re-observation.** `SftpEndpoint::observe_entry` calls
  `version_on` unconditionally, which streams every remote file through
  BLAKE3 (`sftp.rs:770-846`, `read_range_on` :889-958). The
  stored-snapshot check at :786-791 runs after the read and saves only a
  row rewrite. Destinations also re-hash every file the killed run
  wrote: the stored snapshot is the *pre-run* observation, stored at
  `sync.rs:1007-1015` before `execute`.
- **Undo for the killed half.** The next run's first non-empty journal
  batch deletes every other run's journal rows (`REPLACE_OTHER_RUNS`,
  `crates/foldover-db/src/database/journal.rs:55-58`), so Undo
  afterwards covers only the next run's writes.
- **Intent held only in memory:** "skip in the next run" choices (the
  skips map at `sync.rs:251`, spent at :599), the reviewed preview a
  presented token binds to, and an approval itself.

## Design stance

Keep the ADR 0020 recovery model: no persisted plan, no operation
journal for recovery. A continued run is a new run whose base records
make finished work a no-op. A persisted plan would be stale after hours
of downtime, and convergence is already simulation-tested. The work
below removes the remaining costs: the missing trigger and visibility,
the re-read of the remote tree, the re-hash of the just-written set,
and the lost Undo for the killed half.

Two exceptions in ADR 0020's model stay documented gaps, not new work:

- **SFTP replace is two renames:** the old file into the archive, then
  the temporary into place (`sftp.rs:1211-1253`). A kill between them
  leaves the path empty and the temporary archived, or deleted on a
  `records=source` target. In Sync mode the continued run plans a
  `DeletedVersusEdited` conflict (`reconcile.rs:118-124`) and stops for
  a person. It surfaces and never silently loses; ADR 0020:80-87
  already names remote endpoints as the exception.
- **Stop semantics.** On SIGTERM `main` records a clean stop and
  returns (`foldoverd/src/main.rs:62-73`). The runtime's drop then waits
  for the job's blocking task, so the process exits when the job ends or
  when launchd's SIGKILL arrives after the exit timeout. If a graceful
  stop ever cancels jobs, that cancel must keep the marker below,
  unlike a person's `sync.cancel`.

## Work items

Ordered by leverage. The owner decisions are recorded under
[Owner decisions](#owner-decisions); each lands in its work item's ADR.
Suggested order: W3 core → W3 manual-pair resume (`Work::Resume`) →
W2 local → W1 ADR and W1(b) → W2 SFTP → W4.

### W3: continue runs the agent lost

Killed scheduled runs already re-run; this item adds visibility, keeps
Undo for both halves, handles manual pairs and bounds crash loops.

**Status:** the core is implemented as
[ADR 0055](adr/0055-interrupted-runs.md). It refines this section in two
places: the row has three states (`running`, `interrupted`, and `ended`
for a run that stopped without finishing, which keeps the journal for
its successor but is never resumed), and the chain is two flags on the
row (`wrote`, `keep_journal`) rather than a list of names, since the
journal itself holds exactly the chain. Manual-pair resume
(`Work::Resume`, persisted skips, schema v32) is implemented too.

- **Marker table, not a history row.** A new table in a v31 migration
  (`SCHEMA_VERSION` was 30 when this was planned):
  `interrupted_run (pair_id PRIMARY KEY REFERENCES pair ON DELETE CASCADE,
  started_ms, trigger, revision, resume, unclean, chain)`. Keying by
  pair means a new admission replaces an older marker, so a stale
  marker can never outlive a newer run. `run_history` is unchanged: no
  open rows, no new outcome, no `minimum_reader` bump, and the
  scheduler's history readers (`schedule.rs:745-755,979-986`) keep
  seeing only finished runs. Older agents refuse the newer
  `user_version` (`database.rs:1330-1335`); the migration backup covers
  downgrades.
- **Write at admission.** Capture `started` once in `Jobs::start` and
  write the marker there while `engine_guard` is held
  (`sync.rs:566-576`). `publish_claim` holds the jobs lock and has no
  engine handle, and the lock order is engine before jobs
  (`sync.rs:455-457`). Pass that same `started` into the worker
  (today it captures its own at :589) so the marker and the history row
  agree. Only Sync and Scheduled work write markers. Previews write
  nothing, and Undo writes none: a killed Undo leaves its unspent
  journal rows, and undoing again continues it.
- **Chain instead of name continuation.** Each attempt keeps its own
  run name and archive folder, minted as today inside `execute`
  (`run.rs:1319`). The run's first non-empty journal batch appends its
  name to the marker's `chain` in the same transaction, and
  `REPLACE_OTHER_RUNS` deletes only runs outside the chain. A new
  admission copies the previous marker's chain. An attempt killed before
  its first write adds nothing, which is correct because it wrote
  nothing. This avoids the costs of reusing a name: no name injection
  (`RunScope::named` is `pub(crate)` and drops `pace`,
  `local/apply.rs:84-97`), no `seq` seeding against
  `PRIMARY KEY (pair_id, run, seq)`, and no archive collisions. Archive
  moves never overwrite: locally `rename_exclusive` returns `Exists`
  after the new content is already placed (`foldover-fs/src/root/write.rs:530-545`,
  `local/apply.rs:273-285`), and on SFTP the rename fails before
  placement (`sftp.rs:1255-1278`).
- **Undo spans the chain.** `Database::journal` reads only `max(run)`
  (`journal.rs:122-126`). It becomes "every run left in the pair's
  journal, newest run first, `seq` descending"; `REPLACE_OTHER_RUNS`
  guarantees those runs form one chain. `JournalEntry` carries its own
  run, so archive lookups (`run/undo.rs:321-347`) use the entry's run,
  and spending deletes every run undone. Run names start with their
  start time, so name order is chronological.
- **Retention bound.** Archive folders are pruned by the time in their
  name at the end of every finished run (`run.rs:1495-1513`,
  `local/archive.rs:505-522`). A chain whose oldest run is older than
  `ARCHIVE_RETENTION` minus one day is dropped at admission: the run
  starts a fresh journal, and the status says Undo will not cover the
  earlier attempt.
- **Clear on every exit path.** In `spawn` after `catch_unwind`
  (`sync.rs:848-861`), so a panicking job (published as "The job
  crashed", with no history row) does not leave a resumable marker:
  - `Finished`: delete the marker.
  - A person's cancel: delete it. The journal halves stay until the
    next run replaces them, as for any cancelled run today.
  - `NeedsApproval`, `Failed`, a crash: keep the chain and set
    `resume = off`. The next admitted run continues the chain, and
    scheduled pairs re-run on their cadence as today.
  - Agent killed: the row stays with `resume = auto`.
- **Saved edits at startup.** `Engine::open` activates saved edits
  before anything else runs (`configuration.rs:133-136`,
  `database/pending.rs:97-115`), so "refuse to resume over a saved
  edit" can never fire. Instead, resume always runs under the activated
  revision. When activation changes roots or metadata categories,
  `apply_update` deletes the base, resolutions, snapshot and journal
  (`pending.rs:143-156`); it deletes the marker in the same transaction,
  because nothing is left to continue. The marker's `revision` lets the
  manual-pair branch below tell that settings changed.
- **Crash-loop bound.** `note_start` already reports whether this start
  found the previous agent stopped uncleanly
  (`crates/foldover-db/src/database/starts.rs:24-37`). On an unclean
  start, increment `unclean` on every `auto` marker; at 2, set
  `resume = off` and show why. Clean stops (SIGTERM, reboots) never
  count, because the reason to resume is the reboot case. Safe mode alone
  is not enough: it needs three unclean stops in five minutes
  (`safe_mode.rs:12-13`), and with launchd's 30 s `ThrottleInterval`,
  restarts land at D+30, 2D+60 and 3D+90 seconds. A run that crashes the
  agent more than about two minutes in (2D+60 > 300) would otherwise
  resume and crash forever.
- **"An unfinished marker means the agent died."** The runtime lock
  lives in the IPC `Listener` (`crates/foldover-net/src/local.rs:128-130`),
  which is dropped when `serve` stops at SIGTERM while the job runs on.
  Hold the lock until the process exits, acquired outside the Tokio
  runtime, so no second agent can load markers of runs still working.
- **Status.** Before `start_scheduler` (`foldoverd/src/main.rs:54-55`),
  `Service::open` loads markers into `State.jobs` as a new
  `SyncStatus::Interrupted { since, resume }`. `reserve_claim`,
  `busy()` and `mutation()` treat it as idle (`sync.rs:538-545,708-758`),
  `wait_for` keeps it (`:505-528`), and `history::record` writes no row
  for it. `sync.cancel` on an interrupted pair deletes the marker: a
  database write, run off the async runtime with the engine handle,
  unlike today's in-memory flag (`sync.rs:548-555`,
  `foldover-service/src/lib.rs:316-319`). Surface:
  - One `PROTOCOL_VERSION` bump (`foldover-proto/src/local.rs:57`), FFI,
    `scripts/test-agent.py`, app fixtures and the UI tour.
  - App: `canCancel` (`PairSyncViewModel.swift:226-229`) offers "Don't
    resume" for interrupted pairs.
  - CLI: `status_code` and `changes_exit_code` name the variant
    (`foldover-cli` `main.rs:998-1042`).
- **Resume through the scheduler's holds.** Widen `run_due_at`'s
  candidates and `due()` so a pair with an `auto` marker is due at the
  first tick. Never add a separate start path: safe mode, pauses,
  `automation_hold`, pair pause and environment holds are checked only in
  `run_due`/`run_due_at` (`schedule.rs:700-768`), not in `Jobs::start`.
  The gentle-effort slot rule still applies.
- **Manual pairs resume automatically (owner decision, 2026-11-15).** A
  marker written by manual work gets `resume = auto` like scheduled
  work, and the scheduler starts it as a new `Work::Resume`. The
  rejected alternative was `resume = off` with "Interrupted, sync again
  to finish". Ship this as a second W3 slice, after the core marker,
  status and chain: it needs persisted skips first. `Work::Scheduled`
  is wrong for manual work:
  - It runs `RunContext::Unattended`, whose fuse stops only above 50% of
    a side's paths; an interactive run asks above 50 deletions and 1%
    (`crates/foldover-engine/src/gates.rs:1-4,56-76`). A scheduled-mode
    resume could archive half a folder nobody reviewed.
  - It also uses gentle effort (`sync.rs:54-59`), never remounts a share
    (`:977-982`), records the scheduled trigger
    (`foldover-service/src/history.rs:57-60`) and never lifts an undo
    hold (`:1093-1098`).

  `Work::Resume` therefore runs with:
  - interactive deletion gating;
  - full effort, the share remount and the manual history trigger,
    like a person's sync;
  - persisted skips, so "skip in the next run" survives the restart
    (a new table in the v31 migration, spent as today at `sync.rs:599`);
  - a refusal when the marker's `revision` differs from the active one.
    That pair shows "Interrupted, settings changed, sync again".

  It still stops with `NeedsApproval` above the interactive fuse and for
  `review_policy = required` (`run.rs:1234-1252`): it starts unattended
  and halts at the fuse for a person. A resume never presents a review
  token, because the reviewed preview is gone with the old process, so
  it can never count as the approval the killed run had. The status
  explains both stops. Tests: a manual marker resumes at the first tick;
  a persisted skip is honored once; a changed revision refuses; a resume
  over the fuse stops with `NeedsApproval`.
- **Known cost.** The continued run's history row shows only its own
  counts; the killed attempt's counts were in memory. Accept it, or
  later persist running totals in the marker with each batch.
- **Tests.**
  - Service tests with a seeded marker: restart → `Interrupted` →
    due at the first tick → resumes. `automation_hold` and pair pause
    hold it. Cancel deletes it.
  - A new admission replaces it and continues the chain.
  - An unclean start counts and the second one stops resuming; a clean
    stop does not count.
  - A root edit activated at startup deletes the marker.
  - A panicking job leaves no `auto` marker.
  - Journal: two attempts keep both runs, a third non-chain run
    replaces them, and Undo restores both halves newest first from their
    own archive folders.
  - Extend `cargo xtask agent-smoke` with kill-mid-run-and-restart.

### W2: post-write snapshots (local first, then SFTP)

**Status:** local folders are implemented as
[ADR 0056](adr/0056-post-write-snapshots.md). Moves still report no
observation, and the Undo drop moves to the SFTP part, where it matters:
a local put-back changes the stat key's change time.

Files a run writes have no stored fingerprint, because the snapshot is
taken before the run, so the continuing run re-reads the whole written
set.

- **Endpoint API change.** Every write call returns only a version:
  `copy_from` and `update_metadata_from` return `EntryVersion`, and
  `move_entry` returns `()` (`crates/foldover-endpoint/src/endpoint.rs:374-482`).
  `Fingerprint` is crate-private (`local.rs:305-311`). The write calls
  return the post-placement `ObservedEntry` (opaque outside the endpoint
  crate) beside the version, or `None` where none is available.
- **Commit plumbing.** A per-side list of `(path, ObservedEntry)` rides
  `Commit::Base`. `BaseUpdate` has no side (`foldover-state/src/base.rs:20`),
  so this is a separate list, upserted inside `change_base`'s
  transaction (`database.rs:1136`) and deleted for archived or removed
  paths. Atomic with the base commit, a fingerprint never outlives the
  write it proves. W1's contradiction list (below) uses the same path.
- **Undo drops what it reverts.** Undo commits never touch snapshot rows
  (`run/undo.rs:163,226`). They must drop the row of *every* path
  `restore(entry)` writes on `entry.side`, including `Replace`/`PutBack`
  restores recorded as `BaseUpdate::Record` (`undo.rs:296-306`). A
  put-back keeps size, mtime and mode, so a surviving post-write row
  would stat-match the wrong version under W1.
- **Local:** write versions come from the staged temporary's stat
  *before* the rename (`local/foreign.rs:169-201`, `local/apply.rs:231-237`),
  and the local key holds ctime and inode, which the rename can change.
  Re-stat after placement, as `receive_link`/`receive_directory`
  already do (`foreign.rs:219,239`). `scripts/device-matrix.sh` proves
  the key matches a fresh scan on APFS, HFS+, exFAT and FAT32
  (`:49-50`). It has no SMB target; check SMB manually and record it
  (ADR 0038 keeps network shares manual).
- **Settling.** A post-write fingerprint whose mtime is within the 3 s
  racy window of the re-stat is never trusted (`local.rs:296,348-352`);
  a copy with a fresh source mtime is first reused after a confirming
  read in a later run. Tests use old source mtimes or wait.
- **SFTP:** choose between the staged `lstat` (`staged_version_on`,
  `sftp.rs:1076-1087`) and one extra LSTAT after `place_on` (:1211),
  which returns nothing today. Caveat for the ADR: the SFTP digest
  covers bytes *sent*, never read back (`sftp/transfer.rs:488-512`), so
  under W1 written content is first verified at the weekly re-read.
- **Peer: out of scope.** This Mac's stored snapshot of a peer side is
  not what skips work. The serving Mac keeps a root-local `FolderCache`
  in memory (`remote/serve.rs:95`, filled at :527), and
  `PeerReply::Received` carries only a version (`remote.rs:487`). A peer
  version belongs to the serving side and needs no wire change; take it
  as a separate item.
- **Tests.** A first sync then a no-change run hashes none of the
  written files (old mtimes). Simulated interrupted runs re-verify
  nothing already written; asserting that needs a read counter. Undo
  drops reverted rows, put-backs included.

### W1: stat-trust observation for SFTP targets (needs ADR + owner decision)

**Status:** record-keeping targets are implemented as
[ADR 0057](adr/0057-sftp-stat-trust.md). Two refinements: a failed copy,
archive or move drops its path's row whatever the cause, which costs one
reread, and the per-pair opt-in follows as its own change.

Today the SFTP observe reads every file on every run. ADR 0028's
"contents and link targets are re-read" is a deliberate contract, so
this is a policy change, not a refactor.

- **Scope: (b) targets by default, (c) per-pair opt-in (owner
  decision, 2026-11-15).** Reuse a remote
  stat only on sides Foldover alone writes: destination-role sides of
  Backup and Mirror pairs that keep records, which take a lease every
  run. That covers the motivating case, a backup *to* SFTP. PLAN §4.8's
  "unchanged" needs same size, mtime **and file ID** (`PLAN.md:561-562`).
  SFTP v3 has neither inode nor ctime, the fields that catch
  `touch -r`/`cp -p`/`rsync -t` rewrites (`foldover-fs/src/root.rs:194-225`).
  On an SFTP *source* or two-way side, a stat-preserving edit would be
  silently overwritten or stay stale for up to a week instead of raising
  a conflict. `records=source` targets take no lease, and ADR 0040 notes
  that another Mac writing the same target goes unnoticed, so they stay
  re-read unless opted in. Option (c) is a per-pair opt-in for any SFTP
  side whose owner accepts weekly detection.
- **The scope reaches the endpoint.** `observe_owned` and
  `observe_entry` get no role today (`sftp.rs:486-500,770-778`), so a
  literal implementation would trust sources too. The engine computes a
  per-side reuse permission from `draft.role(side)`,
  `draft.unrecorded()` and the opt-in (`foldover-types/src/pair.rs:132-150`)
  and passes it into the observe.
- **Reuse in the walk, before submit.** In overlapped mode files reach
  `observe_entry` on pool workers (`sftp.rs:612-640`). The reuse test
  runs in the walk callback before `pending.submit`, as local does
  (`local.rs:2006-2011`), and clones the stored entry without
  submitting. Serial mode tests at the same point.
- **The predicate.** Reuse only when all of these hold:
  - size and mtime are both present: `stat_key` substitutes `u64::MAX`
    and `0` for absent fields (`sftp.rs:1976-1977`), and those sentinels
    match each other, so `same_stat` alone would reuse them;
  - `same_stat(&key, size)` (`local/fingerprint_key.rs:80-90`);
  - the fingerprint is settled under the window below;
  - it was read within the re-verification interval (`read_within`,
    `local.rs:344-371`).

  Share local's rule `pub(crate)` (`FingerprintKey` is `pub(super)`)
  rather than copying it.
- **Symlinks are out of scope.** Link targets stay read; a READLINK is
  one small round trip, and a same-length target swap with `touch -h -r`
  would otherwise reuse a stale target.
- **Racy window across two clocks.** `observed_at` comes from this
  Mac's clock (`sftp.rs:780`); `mtime` comes from the server's clock, in
  whole seconds. If the server runs 3 s or more behind, a just-edited
  file looks settled and a same-second, same-size edit reuses a stale
  version. Owner decision (2026-11-15): a fixed SFTP window of one hour.
  A fingerprint is settled only when the file's server mtime is more
  than an hour before this Mac's `observed_at`. Nearly all backup files
  are older, so it costs little and needs no clock measurement. The
  rejected alternative was measuring the offset from files Foldover
  creates. Skew beyond an hour is outside the guarantee, and the ADR
  says so. Tests cover skew in both directions within the hour.
- **Refresh on confirming reads, in scope only.** Today's post-read
  equality check returns `known` with its old `observed_at`
  (`sftp.rs:782-791`), so an aged fingerprint would be re-read forever
  and one taken inside the window would never settle. On in-scope
  sides, a confirming read of a racy or aged fingerprint records the new
  time through the cache's existing path (`observation_cache.rs:295-305`).
  Out-of-scope sides keep `return known`: a new `observed_at` makes
  every entry unequal (`Fingerprint` derives `PartialEq`) and would
  rewrite every snapshot row under the engine lock.
- **Drop a contradicted fingerprint.** Execute still reads content: the
  destination digest check (ADR 0034), and `verified_at_on`
  (`sftp.rs:978-992`) on replace or move (:1295) and archive (:1353).
  A stale reused version fails there as `DestinationChanged`. Today the
  engine records `Failure { path, message }` with no side
  (`run.rs:538-541`), and the only snapshot writer is the whole-side
  store before execute. Failures from an in-scope side carry the side,
  and a per-side forget list rides `Commit::Base` (W2's plumbing), so
  the next observe re-reads the path instead of failing for a week.
- **Weekly re-verification** on local's cadence
  (`Fingerprint::reread_after`/`read_within`, `local.rs:301,357-371`).
  This is the only detection for stat-preserving remote edits, so the
  ADR must say written SFTP content is verified at this cadence, never
  at write time.
- **Acceptance by request counts:** a no-change run issues no OPEN or
  READ for matched files. The 96 µs/file in `per-file-cost-budget.md:55`
  is today's read-everything observe, not a bound. The continuation
  still reads content for every remaining SFTP replace and archive.
- **Tests.**
  - Matching stat → no session request; changed stat → read.
  - Clock skew in both directions.
  - Missing size or mtime → no reuse.
  - A source side is never reused.
  - A confirming read refreshes the age on in-scope sides only.
  - A contradicted fingerprint is dropped.

  These run against the `sftp-server` fixture (`FOLDOVER_SFTP_SERVER`).
  The simulation cannot cover this: it has no SFTP endpoint, and the
  memory endpoint's empty-key fingerprint never reuses (`memory.rs:594`).
  The ADR says so.

### W4: resume copies killed in flight (optional, last)

Killed staged copies go to the leftover pass on every endpoint kind, and
SFTP destinations never keep partials.

- **Mechanism:**
  - Keep pipelined writes: 16 × 32 KiB in flight, with replies matched
    first-in-first-out (`foldover-net/src/sftp.rs:31-35,493-556`).
    Persist the contiguous acknowledged offset.
  - On resume, clamp the offset to the remote file's size and truncate
    beyond it.
  - Keep the partial under `.foldover/partial/<digest>`, keyed as ADR
    0034 keys local partials.
  - Exclude `records=source` targets.
- **Not sequential staging.** That drops the pipeline to one request in
  flight, about 16× slower on exactly the large files this is for.
- **Tell staged copies from displaced originals.** Both share the
  `.foldover-tmp-*` prefix, so mark one, for example with a name suffix
  or the partial marker itself.
- **Trust decision in the ADR.** This reverses ADR 0034's refusal of
  SFTP resume. ADR 0034's local resume hashes the kept destination bytes
  (`partial.rs:74-110`). W4 hashes the source's prefix, which verifies
  the source, not what the server stored, and a WRITE acknowledgement is
  not durability. Owner decision (2026-11-15): trust the server's
  acknowledgements and read nothing back. The rejected alternative was
  reading the kept prefix back and hashing it. Clamping the offset to
  the remote file's size still catches a short file. A prefix the server
  acknowledged but lost or corrupted, for example on a NAS power loss,
  goes undetected until the next full re-read: the weekly
  re-verification under W1, or the next run without it. The ADR states
  this trade and that it applies to SFTP destinations only; local and
  paired-Mac partials keep ADR 0034's hash of the kept bytes.

## Follow-ups outside this plan

- Persisted-plan checkpoint resume: still rejected. After W1 to W3 it
  saves only a stat walk, at the price of the execute loop's
  correctness surface.
- Persisting the reviewed-preview binding, so a resumed manual run
  could keep the approval the killed run had. Out of scope: a resume
  halts at the fuse instead.
- Pre-existing: a local replace whose archive path already exists places
  the new content, then fails `set_aside` with no journal row
  (`local/apply.rs:273-285`), so Undo cannot revert it. The chain design
  never triggers this, but it should fail before placement as SFTP
  does.
- Pre-existing: leftover temporaries on paired-Mac `records=source`
  targets fail on every run (`remote.rs:855-859`).

## Verification

- `cargo xtask ci` on every change.
- Simulation seeds: interrupt mid-execute, then assert:
  - the follow-up run reads nothing already written (new read counter);
  - Undo across a chain of two attempts restores the pre-run state.
- SFTP: `sftp-server` fixture tests for W1.
- `scripts/device-matrix.sh` for local post-write fingerprints on the
  four disk-image file systems; SMB checked manually.
- `cargo xtask agent-smoke` extended: kill the agent mid-run, restart,
  assert `Interrupted` then resume for a scheduled pair.
- Performance: rerun the million-file SFTP pipeline
  (`perf/run-qualification.py`) before and after W1+W2; report observe
  wall time and remote request counts in `docs/performance/`.
- Docs:
  - ADR for W1: stat-trust scope and the clock window.
  - ADR for W3: marker, chain, crash bound and the manual-pair decision.
  - ADR for W4: the trust decision.
  - A changelog entry per work item.

## Owner decisions

Recorded 2026-11-15; each goes into its work item's ADR.

| Question | Decision | Rejected alternatives |
|---|---|---|
| Manual pair killed mid-sync (W3) | Resume automatically as `Work::Resume`, halting at the interactive fuse | Show "Interrupted, sync again" and wait for a person |
| Where SFTP stat-trust applies (W1) | Backup and Mirror targets that keep records by default; any SFTP side by per-pair opt-in | Targets only, with no opt-in; every SFTP side |
| When a remote file counts as settled (W1) | Server mtime more than one hour before this Mac's observation time | Measuring the clock offset with a few-second window |
| Verifying a resumed SFTP upload's kept prefix (W4) | Trust the server's acknowledgements; clamp to the remote size | Reading the prefix back and hashing it; not building W4 |

## Revision notes

- **Round 1** (on `e9f2658`): adopted in full. Corrected the inventory,
  switched W1's scope to (b), added the W1 refresh, skew and
  contradiction rules, and the W2 post-placement stats. Dropped the peer
  part of W2. Made W3's status non-blocking and routed resume through
  the scheduler's holds.
- **Round 2** (on `96e2805`): two independent reviews, both verified
  against the code. Reversed round 1's suggestion to use an open
  `run_history` row as the marker and continue the run's name, because
  round 2 showed what each needs:
  - The open row: a nullable `finished_ms` (`database.rs:149,235`), a
    wider outcome enum through `foldover-state`, FFI, app and CLI, a
    `minimum_reader` bump, a clock shared by two writes (`history.rs:46`
    vs `sync.rs:589`), closing superseded rows, and fallout in the
    scheduler's history readers.
  - Continuing the name: injecting it (it is minted inside `execute`),
    keeping `pace`, seeding `seq`, and an archive collision policy.

  The per-pair marker table and journal chain avoid all of that. Also
  adopted: the startup load and database-backed cancel, the
  unclean-start counter, holding the agent lock until exit, the
  retention bound, the W1 scope plumbing and placement, the complete
  predicate, the W2 API change, and the W4 trust decision. Adjudicated:
  killed *scheduled* runs are due at the first tick after a restart;
  killed *manual* runs on `every`/`daily` pairs are not.
- **Owner decisions** (2026-11-15): manual pairs resume automatically,
  W1 scope (b) with (c) opt-in, a one-hour SFTP settling window, and W4
  trusts acknowledgements. See [Owner decisions](#owner-decisions).
