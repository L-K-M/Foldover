# ADR 0039: Calendar schedules

Status: Implemented

## Context

PLAN.md §3.5: a pair should also sync at a time of day on chosen
weekdays — "Weekdays at 18:00", "Every day at 02:30" — in the Mac's
local time zone. The existing schedules ("Every N minutes" since the
last run, "When files change") are measured on a monotonic clock; a
calendar time is not: 18:00 means the wall clock shows 18:00, which a
monotonic interval cannot express across sleeps, restarts and daylight
saving changes.

## Decision

`Schedule::Daily(DailySchedule)` holds a `TimeOfDay` (minutes after
midnight, 0–1439) and a `Weekdays` set (Monday first, stored as a bit
mask). `DailySchedule::latest_at_or_before(local)` is a pure function:
it walks calendar days back from the given local date-time to the most
recent selected weekday whose time is not after it.

A daily pair is due when the latest scheduled moment at or before the
current local date-time is later than the last run's start, read from
run history. A pair with no recorded run is due at once: routine
review is off by default since
[ADR 0020](0020-run-execution.md)'s review policy, so no first-sync
gate holds it. A scheduled moment is therefore covered
exactly once: waking at 18:05 for an 18:00 schedule runs one sync, and
the recorded run start then covers it for every later tick. Interval
schedules compare awake time and, across sleep and restarts, the wall
clock ([ADR 0022](0022-scheduled-runs.md)).

Local wall-clock time comes from `foldover-os::local_time`, a small
audited `localtime_r` binding that works on macOS and Linux, re-exposed
through `foldover-host` and the engine like `computer_name`.

The pair table gains `schedule_time` (0–1439) and `schedule_days`
(1–127), both checked in the schema, in migration 21, which also admits
`schedule = 'daily'` and raises `minimum_reader`: older agents would
misread a daily pair. Local protocol 14 serializes the schedule as
`{"kind": "daily", "at_minutes": 1080, "days": ["monday", ...]}`.

The CLI parses `--schedule daily@HH:MM` with an optional `:mon-fri` or
`:mon,wed,sun` day list; no list means every day. The app's pair editor
offers Daily with an hour/minute picker and weekday toggles, defaulting
to 02:00 every day, and describes it as "Every day at 02:30", "Weekdays
at 18:00" or a day list.

## Consequences

- A scheduled moment missed while the Mac slept or the agent was off
  runs once at the next tick, never repeatedly.
- Daily schedules follow the local clock: a daylight-saving shift moves
  the wall-clock time, which is what "at 18:00" means; an ambiguous or
  skipped local time under a DST transition is resolved by
  `localtime_r`'s reading of the system zone.
- A new daily pair's first sync starts at the first check after it is
  saved, not at its first scheduled moment, unless the pair requires
  approval: with no recorded run every past moment counts as missed.
  The pair editor says that a scheduled pair can start by itself.
- Older agents cannot open a database that stores a daily pair; readers
  at schema 21 or newer can.
