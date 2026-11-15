# Continuing interrupted runs — implementation plan

Written against `2e31ce29` (main after PR 283), revised after review.
Motivation: a run killed by a reboot or an agent stop loses no committed
work today, but the run itself is gone — a manual pair waits for a
person, and the next run pays a re-observation of both trees that for
SFTP endpoints means re-reading every remote file. For large SFTP
backups "the next run finishes the work" can mean re-downloading the
whole backup first.

## What survives a stop today

- Completed work: base and run-journal commits land in batches of 256
  (`COMMIT_BATCH`, `crates/foldover-engine/src/run.rs:59`), bound to the
  pair revision. A crash loses at most one uncommitted batch of records;
  the next run sees equal content on both sides and only records it.
- Leftover temporaries and displaced originals are recovered by the next
  run's leftover pass (`crates/foldover-endpoint/src/local/apply.rs:409-433`).
- A killed run's `.foldover/lock` lease dies within one TTL (~120–150 s,
  ADR 0032); a successor run waits on the existing lease path, reporting
  a wait on its own previous instance (the agent's instance ID is per
  process).
- **Scheduled pairs already continue.** A killed run writes no history
  row (`history::record` runs only after the run returns,
  `crates/foldover-service/src/sync.rs:596`), so `every` pairs count
  their interval from the previous *recorded* start and `daily` pairs
  from the last recorded run (`schedule.rs:268-273,301-315`): both are
  due at the first tick after a restart. `changes` pairs re-run seconds
  after start-up replay (ADR 0002/0022).
- Convergence under interruption is simulation-tested
  (`random_edits_and_interrupted_runs_lose_nothing_and_converge`).

## What is lost today

- **Manual pairs: nothing restarts them.** Job state is in-memory
  (`sync.rs` `State.jobs`); a killed manual run leaves no history row,
  no status, and waits for a person to sync again.
- **In-flight copies never resume on a kill.** A staged
  `.foldover-tmp-*` left by SIGKILL is archived as a leftover, not
  resumed — on *every* endpoint kind, local and paired Mac included.
  `.foldover/partial` resume (ADR 0034) only covers live-run failures:
  an error path moves the staged file there. A reboot is not an error
  path.
- **The re-observation.** `SftpEndpoint::observe_entry` calls
  `version_on` unconditionally, which streams every remote file through
  BLAKE3 (`sftp.rs:770-846`, `read_range_on` :889-958). The
  stored-snapshot check at :786-791 runs after the read and saves only a
  row rewrite. Destinations also re-hash every file the killed run
  wrote: the stored snapshot is the *pre-run* observation (`sync.rs`
  stores it at :1007-1015, before `execute`).
- **The run's identity.** The resuming run is a new run: new archive
  folder, and its first non-empty journal batch deletes the killed
  run's journal (`database/journal.rs`), so Undo afterwards covers only
  the resumed run's writes.
- **Intent held only in memory:** "skip in the next run" choices
  (`sync.rs:251`), the reviewed preview a presented token binds to, and
  an approval itself.

## Design stance

Keep the ADR 0020 recovery model: no persisted plan, no operation
journal for recovery. A "resumed" run is a new run whose base records
make finished work a no-op; a persisted plan would be stale after hours
of downtime, and convergence is already simulation-tested. The work
below removes the remaining costs — the missing trigger and visibility
for manual pairs, the re-read of the remote tree, the re-hash of the
just-written set — and keeps the killed run's identity so Undo survives.

Two exceptions in ADR 0020's model stay documented gaps, not new work:

- **SFTP replace is two renames** (old into archive, temporary into
  place): a kill between them leaves the path empty and the temporary
  archived. The resumed run plans a `DeletedVersusEdited` conflict and
  stops for a person — acceptable: it surfaces, never silently loses
  (reconcile.rs; ADR 0020 already names this the journal exception).
- **Graceful-stop semantics:** today SIGTERM records a clean stop while
  the job runs on until SIGKILL (`foldoverd/src/main.rs:51-74`). If a
  graceful stop ever cancels jobs, that cancel must *keep* the resume
  marker — unlike a person's `sync.cancel`, which clears it.

## Work items

Ordered by leverage; W1 and W3 each need an owner decision recorded in
an ADR.

### W3 — Continue runs the agent lost (marker, status, run identity)

Scheduled pairs already resume; this item delivers visibility, Undo
continuity, manual-pair handling, and a crash-loop bound.

- **Marker = an open run-history row.** Write a "working" row at job
  admission — inside `Jobs::start` while `engine_guard` is held
  (`sync.rs:566-576`), since `publish_claim` holds the jobs lock and has
  no engine handle (lock order is engine → jobs, `sync.rs:455-457`).
  Finish the row in the `spawn` completion path after `history::record`
  (`:596,881-887`). A killed agent leaves an unfinished row: the marker
  *is* the history, and it carries the run's archive-folder name for
  free. Never write one for a Preview. This replaces both a bespoke
  marker table and a separate "interrupted" outcome — but still needs a
  `run_history` schema change (`CHECK` on the outcome column,
  `database.rs:160,237`) and the protocol surface below.
- **Status.** A persisted non-blocking `Interrupted` state the claim,
  `busy()` and edit paths treat as idle, and `sync.cancel` clears to
  mean "do not resume" — distinct from `Working`, which would block
  claims, gentle-effort slot counting and edits, and read a cancel flag
  no job checks (`sync.rs:538-545,708-738,755-758`). Protocol, FFI, app
  and CLI updates ride one `PROTOCOL_VERSION` bump with tests
  (`scripts/test-agent.py`, app fixtures, UI tour).
- **Resume through the scheduler's holds.** Widen `run_due_at`'s
  candidates and `due()` to treat a marked pair as due at the first
  tick — never a separate start path, which would skip safe mode,
  pauses, `automation_hold`, pair pause and environment holds, all
  checked only in `run_due`/`run_due_at` (`schedule.rs:700-768`). A
  marked Undo or Preview resumes nothing: Undo already refuses without
  its journal's run; previews are throwaway.
- **Attempt count.** Cap resume at one retry after an unclean stop of a
  resumed run: safe mode needs three unclean stops inside five minutes,
  and launchd restarts the agent after ~30 s, so a run that crashes
  later than ~2.5 minutes in would resume-crash forever otherwise.
- **Continue the run's name.** The resumed run reuses the killed run's
  recorded name (kept on the open history row): same archive folder,
  journal rows append under `seq` continuing from the stored maximum,
  and the first-batch journal delete keys on `run` so nothing is
  replaced. Undo then covers the whole logical run — most of the "run
  identity" benefit without a persisted plan.
- **Manual pairs — owner decision.** Options: (a) default — status
  `Interrupted`, "sync again to finish", no auto-write (recommended);
  (b) auto-resume via a new `Work::Resume` that uses *interactive*
  deletion gating — `RunContext::Unattended` stops only above 50% of a
  side's paths while interactive review asks above 50 deletions and 1%
  (`gates.rs:56-76`), so a scheduled-mode resume could archive up to
  half a folder nobody saw — plus persisted skips and a refusal to
  resume over saved-not-yet-activated edits (`sync.rs:251,566-576` for
  where those live today). `Work::Scheduled` is wrong for manual pairs
  on four more counts: gentle effort, no share remount (`:977`),
  scheduled history trigger, and it never lifts an undo hold
  (`:1093-1097`). The ADR records the choice.
- **Tests.** Service tests with a pre-seeded open row: restart → due at
  first tick → resumes; `automation_hold` and pair pause hold it;
  cancel clears the marker; second unclean stop stops resuming; journal
  appends under one run name and Undo spans both halves. Extend
  `cargo xtask agent-smoke` with kill-mid-run-and-restart.

### W2 — Post-write snapshots (local first, then SFTP)

Files a run writes have no stored fingerprint (the snapshot is
pre-run), so the continuing run re-reads the whole written set.

- **Mechanism.** A per-side list of `(path, ObservedEntry)` post-write
  fingerprints rides `Commit::Base` (`BaseUpdate` has no side today —
  new plumbing), upserted inside `change_base`'s transaction
  (`database.rs:1136`) and dropped for archived/removed paths. Atomic
  with the base commit: a fingerprint never outlives the write it
  proves. Undo commits must drop the snapshot rows they revert — undo
  never touches them today, and a put-back keeps size+mtime+mode, which
  would trick stat-reuse into the wrong version.
- **Local:** write versions come from the staged temporary's stat
  *before* rename (`local/foreign.rs:169-201`, `local/apply.rs:231-237`),
  and the local key holds ctime+inode, which the rename changes.
  Re-stat after placement, as `receive_link`/`receive_directory`
  already do (`foreign.rs:219,239`). The device matrix proves the key
  matches a fresh scan on APFS, exFAT and SMB.
- **SFTP:** choose between the staged `lstat`
  (`staged_version_on`, `sftp.rs:1076-1087`, *not* the post-placement
  path) and one extra LSTAT after `place_on` (:1211), which returns
  nothing today. Caveat for the ADR: the SFTP digest covers bytes
  *sent*, never read back, so under W1 written content is first
  verified at the weekly re-read.
- **Peer: out of scope.** This Mac's stored snapshot of a peer side is
  not what skips work — peers keep a root-local cache on the serving
  Mac (`endpoint.rs:265-274`), in memory, updated after an observe
  (`remote/serve.rs`). A peer version belongs to the serving side and
  needs no wire change; take it as a separate item or drop it.
- **Tests.** First sync then a no-change run shows an observe with no
  hashing of written files; simulated interrupted runs re-verify
  nothing already written (needs a read counter to assert); undo
  invalidates reverted rows.

### W1 — Stat-trust observation for SFTP targets (needs ADR + owner decision)

Today the SFTP observe reads every file on every run — ADR 0028's
"contents and link targets are re-read" is a deliberate contract, so
this is a policy change, not a refactor.

- **Scope — recommend (b), targets only, plus (c) opt-in.** Reuse
  remote stat only on sides Foldover alone writes: Backup and Mirror
  targets that keep records and are leased every run. That covers the
  motivating case (backup *to* SFTP) without the (a) risk the original
  plan's rationale overstated: PLAN §4.8's "unchanged" needs same size,
  mtime **and file ID**, and SFTP v3 has neither inode nor ctime — the
  fields that catch `touch -r`/`cp -p`/`rsync -t` rewrites. On an SFTP
  *source* or two-way side, a stat-preserving edit would be silently
  synced or stale for up to a week instead of raising a conflict.
  `records=source` targets cannot satisfy (b) — no lease, and ADR 0040
  notes another Mac writing the same target goes unnoticed — so they
  stay re-read. Option (c) is a per-pair opt-in for sources and Sync
  sides whose owners accept the weekly-detection risk.
- **Reuse rule.** In `observe_entry` (`sftp.rs:770`), test
  `known.fingerprint().same_stat(&stat_key(attributes), size)` *before*
  `self.version()` — on the walk thread, like local (`local.rs:2010`),
  not on a pool worker. Require complete attributes: `stat_key`
  substitutes `u64::MAX`/`0` for absent size/mtime (`sftp.rs:1974-1987`),
  and an epoch-0 mtime would look ancient, so missing fields refuse
  reuse (the `from_key` contract at `local.rs:334` agrees). Share the
  reuse predicate `pub(crate)` rather than copying local's private
  helpers.
- **Racy window across two clocks.** `observed_at` is this Mac's clock;
  `mtime` is the server's, in whole seconds. If the server runs ≥3 s
  behind, a just-edited file looks settled and a same-second same-size
  edit reuses a stale version. Either measure the offset from files
  Foldover itself creates (identity file, lease, staged temporaries) or
  use an SFTP window of minutes–hours — nearly all backup files are
  old, so a large window costs little. Decide in the ADR; test skew in
  both directions.
- **Refresh on confirming reads.** Today's post-read equality check
  returns `known` with its old `observed_at` (`sftp.rs:786-791`,
  justified only by "never reused unread"), so an aged fingerprint
  would re-read forever and one taken inside the window would never
  settle. A confirming re-read must record the new observation time —
  the observation-cache path for that exists
  (`observation_cache.rs:286-305`).
- **Drop a contradicted fingerprint.** Execute still reads content:
  destination digest checks (ADR 0034), `verified_at_on` on replace and
  archive (`sftp.rs:978`, :1352). If a reused version was stale, those
  fail once — and every run thereafter for a week. On contradiction,
  delete the path's fingerprint so the next observe re-reads and
  re-fingerprints.
- **Weekly re-verification** on local's cadence
  (`Fingerprint::reread_after`/`read_within`, `local.rs:301,357-371`) —
  the only detection for stat-preserving remote edits, so the ADR must
  say written SFTP content is verified at this cadence, never at write
  time.
- **Acceptance by request counts:** a no-change run issues no OPEN/READ
  for fingerprint-matched files. (The 96 µs/file figure in
  `per-file-cost-budget.md` is today's read-everything observe, not a
  bound.)
- **Tests.** Matching stat → no session request; changed stat → read;
  skewed clocks both ways; missing size/mtime → no reuse; confirming
  read refreshes age; contradicted fingerprint dropped. `sftp-server`
  fixture (`FOLDOVER_SFTP_SERVER`). Simulation cannot cover this (no
  SFTP endpoint; the memory endpoint's empty-key fingerprint never
  reuses) — say so in the ADR.

### W4 — Resume copies killed in flight (optional, last)

Killed staged copies are archived on every endpoint kind, and SFTP
destinations never keep partials. For the staged file: keep pipelined
writes but persist the contiguous acknowledged offset; on resume,
truncate beyond it and hash the source's prefix locally (the SFTP
digest is computed over bytes sent anyway); keep the partial under
`.foldover/partial`; exclude `records=source` targets (ADR 0040: no
resume there). Sequential staging is **not** acceptable — it drops 16×
32 KiB pipelining to one request on exactly the large files this is
for. Also requires telling staged copies from displaced originals,
which share the `.foldover-tmp-*` prefix — e.g. a name suffix or the
partial marker itself.

## Follow-ups outside this plan

- Persisted-plan checkpoint resume: still rejected — after W1–W3 it
  saves only a stat walk, at the price of the execute loop's
  correctness surface.
- If manual auto-resume (W3(b)) is chosen: persisting skips, the
  reviewed-preview binding, and pending-edit activation semantics each
  deserve their own ADR section.

## Verification

- `cargo xtask ci` on every change.
- Simulation seeds: interrupt mid-execute, assert the follow-up run
  reads nothing already written (new read counter), undo across a
  continued run name.
- SFTP: `sftp-server` fixture tests for W1; `scripts/device-matrix.sh`
  for local post-write fingerprints on APFS/exFAT/SMB.
- `cargo xtask agent-smoke` extended: kill agent mid-run, restart,
  assert resume.
- Performance: rerun the million-file SFTP pipeline
  (`perf/run-qualification.py`) before and after W1+W2; report observe
  wall-time and remote request counts in `docs/performance/`.
- Docs: ADRs for W1 (stat-trust scope and clock handling) and W3
  (resume policy, manual-pair decision); changelog per work item.
