# Improvement plan, 2026-11-10

Reviewed against `70544e18` (main after PR 276). This plan selects work
from a fresh review and from the standing backlog in
[ANALYSIS.md](../ANALYSIS.md); it changes no behavior by itself.

## How the review was done

- **Gates on main.** The last fifteen `ci`, `nightly`, `devices` and
  `bench` runs on main passed. `cargo test --workspace` on the Mac Studio
  under load failed one test once (R2 below); `shellcheck` and both doc
  checkers reported nothing.
- **The app on screen.** The 110 screenshots of the latest `ui-tour` run
  (light and dark, menus, sheets, stress scenarios).
- **Documents against code.** The README, the guides, the changelog and
  the release scripts, read against what the code and the release
  actually do.
- **Static review by area**, under way when this plan was first
  committed: the Swift app, the agent-side crates (service, engine,
  state, IPC, client, CLI), and the driver crates (endpoint, fs, net, db,
  os, watch). A finding enters the table below only once it is confirmed
  in the code.
- **Backlog audit**, also under way: every `bugs`, `general issues` and
  `performance` entry of ANALYSIS.md, and the small usability entries,
  checked against the current code. Two results so far: MIMO-01 does not
  hold, because the FSEvents header defines `HistoryDone` as a sentinel
  event of its own whose path is ignored, never a flag on a real change;
  and the panic MIMO-06 describes has no trigger, because `SyncPath` has
  no byte limit that a lower-cased name could exceed.

Hardware behavior, VoiceOver, frame times and physical NAS runs were not
reviewed; they stay with the owner's release qualification
(ANALYSIS.md SOL-11).

## Findings

Sizes: S is under about 30 changed lines, M under about 300, L more.

| ID | Area | Finding | Impact | Size |
|---|---|---|---|---|
| R1 | Scheduler | Interval schedules count only time the Mac is awake and start over when the agent restarts | An "Every day" pair on a Mac that sleeps at night runs about every third day, and never on one that restarts daily | M |
| R2 | Tests | `a_gentle_automatic_sync_waits_for_a_manual_one` ticks twice and fails when the first job finishes between the ticks | Spurious red runs under load | S |
| R3 | CI | `checkdocs.py` and `mmdcheck.mjs` always exit 0, and the table check CI names does not exist | The `docs` job cannot fail; broken links pass review | S |
| R4 | Docs | README calls shipped features "Planned highlights" and omits five pair options the CLI accepts; PLAN.md still says "P1 implementation" | Readers get the wrong status and miss options | S |
| R5 | App | Schedule summaries print 24-hour times ("Every day at 18:00") beside a picker that follows the Mac's clock format ("6:00 PM") | Two formats for one time in one sheet | S |
| R6 | Docs | limitations.md and the README say a pair's first run always stops for review; since PR 188 it runs unattended by default, and neither names the two review settings | A reader trusts a safety net that is off by default | S |
| R7 | Release | Nothing moves the changelog's Unreleased entries under a version, so the 0.2.0 release notes read "See CHANGELOG.md." and the changelog names no release | Nobody can tell what a release changed | M |

### R1: interval schedules and sleep

`Scheduler::due` compares `Instant`s for `Schedule::Every`
(`crates/foldover-service/src/schedule.rs`). On macOS `Instant` reads
`CLOCK_UPTIME_RAW`, which stops while the Mac sleeps, and the last start
lives in agent memory. The editor offers "Every day" as an interval of
1440 minutes, so a Mac awake eight hours a day reaches it every third
day, and a restart of the agent (a reboot, a login, an app update)
starts the count again. Daily calendar schedules already avoid this by
comparing wall-clock time with the last recorded run
([ADR 0039](adr/0039-calendar-schedules.md)); limitations.md documents
the restart, not the sleep.

Plan: an interval pair is also due once the wall clock shows a full
interval since its last scheduled start or, after a restart, since the
start of its last recorded run. The awake-time rule stays, so a clock
set back cannot hold a pair, and a clock set forward costs one early
run. All run gates stay as they are.

Acceptance: unit tests on `Scheduler::due` for sleep (wall clock ahead
of awake time), restart (recorded run older than the interval, due
once), a recent recorded run (not due) and a clock set back (awake time
still governs). ADR 0022 and limitations.md say what now happens.

### R2: a test that depends on job timing

`run_due_started` ticks, waits for folder checks, and ticks again. With
gentle effort the second tick rightly starts the other pair when the
first job has already ended, and the test then counts two starts
instead of one. A 500 ms pause between the ticks reproduces it every
time.

Plan: hold the started job at the test's job gate until the count is
checked. Acceptance: the test passes with the pause in place; the whole
service suite passes with it too.

### R3: documentation checks that cannot fail

Both checkers print their problem count and exit 0. The CI step is
named "Check links, anchors, fences and tables", but nothing checks
tables. Run locally, both also walk ignored build output (`dist/`,
`build/`) and report its files.

Plan: exit 1 on any problem; read tracked and new files only; add the
table check (rows whose cell count differs from the header); test the
checker and run those tests in the `docs` job. Acceptance: a broken
link, an unclosed fence and a short table row each fail the command;
the repository reports 0 problems.

### R6: a first-run review the code no longer does

PR 188 made routine review a per-pair setting that is off by default
(PLAN.md §3.6, [ADR 0020](adr/0020-run-execution.md)); the service test
`an_automatic_backup_runs_on_its_first_scheduled_tick` pins it. That
pull request changed neither limitations.md, whose first safety net
still reads "The first run always needs you", nor the changelog. The
README repeats the claim in three places, and no guide mentions
"Require approval before syncing" or "Stop unusually large removals".

Plan: state the actual behavior and both settings in limitations.md,
the README and the changelog. No behavior changes: the default is the
owner's recorded decision.

### R7: releases without notes

`scripts/release-notes.py` prints the changelog section named after the
version and falls back to "See CHANGELOG.md."; the release flow never
creates that section, so 0.2.0 shipped the fallback and the changelog
still lists everything as Unreleased.

Plan: a post-bump step moves the Unreleased entries under
`## [X.Y.Z] - DATE` in the bump commit, and refuses a release with
nothing to say; a pre-tag check covers versions bumped by hand. Cut the
0.2.0 section from the changelog as it stood at the `v0.2.0` tag.
Acceptance: tests roll a changelog and read the result back through
`release-notes.py`; `release-notes.py 0.2.0` prints the 0.2.0 changes.

### R4 and R5

Plan for R4: rename the section, document `daily@HH:MM[:DAYS]`,
`--bandwidth-limit`, `--canary`, `--review` and `--removal-protection`
in the README, and state the current phase in PLAN.md.

Plan for R5: format schedule times with the Mac's locale. Acceptance:
view-model tests for a 12-hour and a 24-hour locale.

## Order of work

1. R2, R3, R4, R6 and R7 in one pull request with this plan: tests,
   checks, documents and the release script, with no change to what the
   app or agent does. R3 guards the documentation the later changes
   touch.
2. R1 in its own pull request: it changes when automatic syncs run and
   needs its ADR text reviewed.
3. Confirmed findings from the area reviews, grouped by area (Swift app,
   Rust), one pull request each, with a regression test per fix.
4. R5 with the Swift changes.

Each pull request goes through CI and the automated review under the
rules in AGENTS.md before the next one starts from the new main.

## Not in this pass

- **Hardware and accessibility qualification** (ANALYSIS.md SOL-11,
  SOL-12, MUSE-05e, SOL26-17): needs devices, displays and VoiceOver.
- **Protocol or schema features** (SOL-04 server-side search, SOL-08
  archive run paging, SOL-09 agent-delivered notifications, MIMO-11
  event streaming, SOL-05 in-flight bytes): each is a design of its own
  and deserves an ADR and the owner's priority call.
- **Durability trade-offs** (MIMO-08 batched directory syncs): changes
  the ADR 0019 contract; not a review fix.
