# Continuing interrupted runs — implementation plan

Written against `2e31ce29` (main after PR 283). Motivation: a run killed
by a reboot or an agent stop loses no completed work today, but the run
itself is gone — a manual pair waits for a person, a scheduled pair waits
for its cadence, and the next run pays a full re-observation of both
trees. For pairs whose far side is an SFTP server that observation reads
and hashes every remote file, so "the next run finishes the work" can
mean re-downloading the whole backup before anything resumes.

This plan makes a stopped run continue unattended, and makes the
continuing observation cost a walk instead of a re-read.

## What survives a stop today

- Completed work: base and run-journal commits land in batches of 256
  (`COMMIT_BATCH`, `crates/foldover-engine/src/run.rs:59`), bound to the
  pair revision. A crash loses at most one uncommitted batch of records;
  the next run sees equal content on both sides and only records it.
- Partial copies ≥ 16 MiB to local and paired-Mac destinations resume at
  their offset via `.foldover/partial/<digest>` (ADR 0034). Copies to
  SFTP destinations start over; unrecorded (`records=source`) targets
  keep nothing (ADR 0040).
- Leftover temporaries and displaced originals are recovered by the next
  run's leftover pass (`run.rs` `recover_leftover`).
- A killed run's `.foldover/lock` lease dies within one TTL (~120–150 s,
  ADR 0032); a successor run waits on the existing lease path.
- Convergence under interruption is simulation-tested
  (`random_edits_and_interrupted_runs_lose_nothing_and_converge`).

## What is lost today

- **The trigger.** Job state is in-memory (`crates/foldover-service/src/
  sync.rs`, `State.jobs`). A killed run leaves no history row and nothing
  restarts it: manual pairs wait for a person; `every`/`daily` pairs wait
  for the next due moment; `changes` pairs re-run seconds after start-up
  replay (ADR 0002/0022).
- **The re-observation.** `SftpEndpoint::observe_entry` calls
  `version_on` unconditionally, which streams every remote file through
  BLAKE3 (`crates/foldover-endpoint/src/sftp.rs:770-846`,
  `read_range_on` :897-958). The stored-snapshot check at :786-791 runs
  after the read and saves only a row rewrite. Local destinations re-hash
  every file the killed run wrote, because the stored snapshot is the
  *pre-run* observation (`sync.rs` stores it at :1007-1015, before
  `execute`).
- **In-flight copies.** Files under 16 MiB, and all files to SFTP
  destinations, restart from zero.

## Design stance

Keep the ADR 0020 recovery model: no persisted plan, no operation
journal for recovery. A "resumed" run is a *new* run whose base records
make finished work a no-op. The work below removes the three remaining
costs — the missing trigger, the re-read of the remote tree, and the
re-hash of the just-written set — plus one optional per-file bound. A
checkpoint-resume (persisted plan + cursor) stays out of scope; its
remaining value after this plan is run-identity semantics (one history
entry, one journal for Undo, approval carry-over), listed under
follow-ups.

## Work items

Each item ships independently; order is by leverage for the motivating
case (large SFTP backup), not dependency. W1 and W2 together reduce the
continuation cost to a stat walk; W3 makes it unattended.

### W1 — Stat-trust observation for SFTP

Today the SFTP observe reads every file on every run (ADR 0028's
"Unchanged-stat proof: unavailable; contents and link targets are
re-read"). Give it the reuse the local endpoint already has.

- **Change.** In `observe_entry` (`sftp.rs:770`), test the stored
  fingerprint *before* `self.version()`: when
  `known.fingerprint().same_stat(&stat_key(attributes), size)` holds,
  reuse `known` outright — no session, no read. Keep the existing
  post-read equality check for the read path. Directory and symlink
  handling is unchanged (directories already cost no request; symlink
  targets are content and stay re-read, or reuse under the same rule —
  decide in the ADR).
- **Racy window.** Reuse only when the file was already ≥ `RACY_WINDOW`
  old when its fingerprint was taken (mirror `local.rs`
  `proves_unchanged_by`: `observed_at − mtime ≥ RACY_WINDOW`). SFTP
  mtimes are whole seconds (ADR 0028); the window logic must account for
  second-granularity, e.g. require `observed_at − mtime ≥ RACY_WINDOW +
  1 s`.
- **Periodic re-verification.** Reuse expires on the same cadence as
  local (`Fingerprint::reread_after`/`read_within`: 3.5–7 days, spread
  per stat, `local.rs:301,357-371`). An aged-out fingerprint re-reads
  once and re-fingerprints, so a stat-preserving remote edit surfaces
  within a week — the same guarantee ADR 0018 gives local roots.
- **Trust scope — owner decision, needs an ADR.** Reusing remote stat
  trusts the server not to change content while preserving
  size+mtime+mode. ADR 0028 already accepts that residual risk in words
  ("only noticed by the next full read"); this item makes it the default
  path instead of the exception. Options: (a) all SFTP sides, matching
  PLAN §4.8's "same size, same mtime, same file ID means unchanged"
  model, with the weekly re-read as backstop — recommended, and needed
  for the motivating backup case; (b) only sides Foldover alone writes
  (Backup/Mirror targets), which is near-risk-free but leaves SFTP
  *sources* paying full reads; (c) a per-pair option. Decide in the ADR,
  which must also amend ADR 0028's differences table.
- **Tests.** Unit: matching stat → no session opened; changed
  size/mtime/mode → read; racy-window boundary; aged fingerprint → one
  re-read then reuse. Integration against the `sftp-server` fixture
  (`FOLDOVER_SFTP_SERVER`). Simulation seeds unchanged.
- **Acceptance.** A no-change run over a million-file SFTP tree issues
  no content reads for fingerprint-matched files; observe time drops to
  the walk+stat bound measured in
  `docs/performance/per-file-cost-budget.md` (~96 µs/file baseline).

### W2 — Post-write snapshots

Files a run writes have no stored fingerprint (the snapshot is
pre-run), so even under W1 the continuing run re-reads the whole written
set. The write path already fetches post-write attributes for
verification (`staged_version_on`, `sftp.rs:921-935`; local
`receive_file`; peer `PeerReply::Received`, `remote.rs:487`).

- **Change.** Have each endpoint write op surface the post-write stat
  key it already read, assembled into `(path, ObservedEntry)` pairs per
  side. Attach them to `Commit::Base` so the next snapshot write —
  inside the same transaction as the base rows — records the post-write
  fingerprint for every committed write, and drops fingerprints for
  archived/removed paths. Atomic with the base commit: a fingerprint can
  never outlive the write it proves.
- **Scope.** All endpoints (local, SFTP, peer). For local it also
  removes the cold re-hash of just-written files on the next run — the
  same gap `docs/performance/million-file-engine-state-hot-paths.md`
  proposal 2 identifies for steady-state runs.
- **Tests.** First sync then a no-change run shows an observe with no
  hashing/reading of written files; simulated runs interrupted mid-way
  re-verify nothing already written. Snapshot rows agree with the base
  they committed beside.

### W3 — Continue runs the agent lost

Persist just enough to know a pair was working when the agent stopped,
then resume it as a fresh run.

- **Marker.** Next state schema version: a per-pair "job working"
  record (pair id, work kind, started), written at claim publication
  (`Jobs::publish_claim`, `sync.rs:789`) and cleared in the `spawn`
  completion path before the status publishes (`sync.rs:881-887`).
  Best-effort like history: a lost clear leaves a stale marker that
  resumes once, harmlessly.
- **On start.** `Service::open` (before `start_scheduler`): pairs with a
  marker get a working-state placeholder so `sync.status` reports the
  pair as continuing rather than idle; the scheduler starts them at the
  first tick as `Work::Scheduled` runs — unattended gates apply, so a
  plan that still needs review stops `NeedsApproval` for a person.
  Resume is skipped by every existing hold: crash-loop safe mode
  (`run_due` already refuses), automation pause, per-pair pause,
  `automation_hold` after undo, environment holds, and a pair whose
  marker says the interrupted job was an Undo or a Preview.
- **Policy decision.** Manual pairs: resume unattended too (recommended
  — the run re-gates everything; worst case it stops for approval), or
  surface an "interrupted — sync again" status and wait for a person.
  Record the choice; the mechanism is the same either way.
- **Status and history.** Optional but recommended: a new
  `SyncStatus::Interrupted`-style outcome in history (alongside
  completed/cancelled/needs-approval/failed), so the app can show "the
  sync stopped when the agent did" instead of nothing. Wire, FFI, app
  and CLI updates ride the same protocol bump.
- **Edge cases.** An interrupted first sync resumes through the normal
  first-run gates (identities still uncommitted). The stale lease of the
  killed run waits out its TTL inside the resumed run's existing lease
  acquisition. A pair edited or removed while down behaves as if
  configured today (revision checks in commit already stop stale work).
- **Tests.** Service tests with a pre-seeded marker: restart → resume
  starts; `NeedsApproval`/`Undone` holds stay held; safe mode blocks.
  Extend `cargo xtask agent-smoke` with a kill-mid-run-and-restart case.

### W4 — Optional: resume copies to SFTP destinations

ADR 0034 leaves SFTP destinations restarting interrupted copies because
pipelined writes can land out of order and nothing server-side hashes
the staged bytes. Bounds: at most one file per worker. Options: stage
SFTP copies sequentially so `.foldover/partial` applies (throughput cost
inside a file, none across files), or keep per-range bookkeeping on the
staged file. Decide with the perf numbers in
`docs/performance/sftp-million-file-run-costs.md`; independent of W1–W3.

## Non-goals and follow-ups

- **Checkpoint resume (persisted plan + cursor):** rejected for now —
  after W1–W3 it saves only a stat walk, and its complexity sits on the
  correctness-critical execute loop. Revisit if large-tree resumes still
  hurt.
- **Semantics W3 does not give:** the resuming run is a new run — its
  first journal batch replaces the killed run's journal, so Undo covers
  only the resuming run's writes; an over-fuse plan re-stops for a
  person; history shows two runs. A later "run identity" item could
  chain resumed runs into one logical unit if users miss these.
- **`Durability::Full` on SFTP** stays refused (ADR 0028).

## Verification

- `cargo xtask ci` on every change; `cargo deny check` untouched.
- Simulation (`foldover-engine/src/run/tests/simulation.rs`): add seeds
  that interrupt mid-execute and assert the follow-up run reads nothing
  already written — the convergence property already exists.
- SFTP: the `sftp-server` fixture tests for W1; `scripts/device-matrix.sh`
  for local W2 paths on APFS/exFAT.
- Performance qualification: rerun the million-file SFTP pipeline
  (`docs/performance/sftp-lan-million-doc-pipeline.md`,
  `perf/run-qualification.py`) before and after W1+W2; record observe
  wall-time in the perf docs.
- Docs: ADRs for W1 (stat-trust scope) and W3 (resume policy);
  changelog entry per work item.
