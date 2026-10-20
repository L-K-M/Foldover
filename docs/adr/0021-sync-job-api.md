# ADR 0021: Sync job API

Status: Implemented in the agent, CLI and app

## Context

Previews hash whole trees and runs copy data, so both outlast the client's
five-second request timeout. The configuration queue serializes engine
access, and a run must not block it ([ADR 0020](0020-run-execution.md)).

## Decision

Previews and runs are background jobs in the agent, at most one per pair.

```text
Idle ─start─▶ Working ─▶ Previewed | NeedsApproval | Finished | Failed
                 └─cancel─▶ Idle (before writing) or Finished (cancelled)
```

| Method | Parameters | Effect |
|---|---|---|
| `sync.preview` | `id` | Start a preview |
| `sync.run` | `id`, approved deletions per side, optional `review_token` | Start an interactive run |
| `sync.status` | `id` | Current status; `idle` if none |
| `sync.cancel` | `id` | Ask the job to stop at the next check |
| `sync.items` | `id`, `review_token`, `after` path, optional `kind` | The next 100 items of the latest preview, in path order |

- Starting a job while one is working fails with `SYNC_BUSY` (-32007).
- A `working` status carries live `progress` (protocol 6): `listed` and
  `checked` count entries found in both folders and those whose version
  is known; once the plan is ready, `changes` and `bytes` give its size
  and `applied` and `copied` how much is done. Counters are atomics the
  run updates and status requests read, so polling never waits on the
  run. Copies count their bytes when each file finishes.
- Jobs bypass the configuration queue. They lock the engine only to read
  inputs and commit progress, so other requests stay responsive.
- A status carries at most 100 items or failures, plus counts for all of
  them, so maximum-length paths stay below the 1 MiB frame limit.
  `sync.items` pages through all of a preview's items, optionally of one
  kind (`copy`, `update`, `archive`, `conflict`, `blocked`, `keep`); it
  fails with `PREVIEW_REQUIRED` when the pair has no preview.
- Every preview carries a `review_token` and the `pair_revision` it was
  planned for. The token is a deterministic fingerprint of the whole
  plan — pair, revision, counts and every item — not just the listed
  page. `sync.run` re-stops with a fresh `NeedsApproval` when a nonzero
  approval or a presented token does not name the held preview; a
  matching token with an unchanged plan marks the run reviewed.
  `sync.resolve` and `sync.items` need the token too and fail with
  `PREVIEW_REQUIRED` when it is missing or stale, or when the pair's
  revision moved since the preview.
- Results stay in agent memory until the pair's next job; an agent restart
  forgets them.
- `daemon.status` reports the `sync` capability.

The CLI starts a job, polls every 250 ms and prints the final status as one
JSON line: `foldover sync ID [--preview | --cancel | --approve LEFT RIGHT
--review TOKEN]`. `--review` is the preview's `review_token`, echoed by a
stopped run's hint; it is never looked up implicitly. A run that needs
approval exits 3; failures and cancellations exit 1.

The app's `SyncStore` keeps generated values inside one actor, starts jobs
and polls every 500 ms while one works. A run that needs approval shows the
deletion counts; confirming retries with those counts and the `reviewToken`
captured with the presented preview — never a token fetched after the user
confirms. The retry executes only while the plan still equals the preview
being confirmed — a changed or superseded plan stops for review again.
Declining keeps the preview visible.

While the app runs, with or without its window, it checks every pair's
status every 15 seconds. Outcomes of automatic runs that need a person
become notifications: a run stopped for approval, a failed run, conflicts,
or items that failed. Previews, syncs started in the app, and states seen
at launch never notify. The menu bar lists each pair's status with Sync and
Sync All.

## Consequences

- Polling, not events. A subscription stream can replace it without
  changing job semantics.
- Progress is coarse within a file: a single large copy shows no movement
  until it finishes. Throughput and ETA are left to clients.
- Unattended runs (watchers, schedules) will use the same jobs.
