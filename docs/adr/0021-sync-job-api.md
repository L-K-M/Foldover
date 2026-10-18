# ADR 0021: Sync job API

Status: Implemented in the agent and CLI; app pending

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
| `sync.run` | `id`, approved deletions per side | Start an interactive run |
| `sync.status` | `id` | Current status; `idle` if none |
| `sync.cancel` | `id` | Ask the job to stop at the next check |

- Starting a job while one is working fails with `SYNC_BUSY` (-32007).
- Jobs bypass the configuration queue. They lock the engine only to read
  inputs and commit progress, so other requests stay responsive.
- A status carries at most 100 items or failures, plus counts for all of
  them, so maximum-length paths stay below the 1 MiB frame limit.
- Results stay in agent memory until the pair's next job; an agent restart
  forgets them.
- `daemon.status` reports the `sync` capability.

The CLI starts a job, polls every 250 ms and prints the final status as one
JSON line: `foldover sync ID [--preview | --cancel | --approve LEFT RIGHT]`.
A run that needs approval exits 3; failures and cancellations exit 1.

## Consequences

- Polling, not events. A subscription stream can replace it without
  changing job semantics.
- No progress counts yet; status only says which job is working.
- Unattended runs (watchers, schedules) will use the same jobs.
