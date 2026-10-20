# ADR 0033: Canary file

Status: Implemented

## Context

PLAN.md §3.6 gates a run on proofs that the expected folders are the ones
mounted. The emptied-side gate catches a wiped or swapped folder only
when it is empty; a stale copy full of old files passes it. A disk that
never mounted looks like an empty folder too, until something writes
into it.

## Decision

A pair may name one canary: the relative path of a file the person keeps,
unchanged, in both folders. Before a run writes anything, and before it
records folder identities or takes leases, each side's observation must
hold a regular file at that path. When the pair's base already records
the canary, the observed content must equal that side's recorded content.
A missing or non-file canary fails the run with `CanaryMissing`; changed
content fails it with `CanaryChanged`. Pairs without a canary are
unaffected, and `.foldover` paths are rejected so the guard never points
at the pair's own state.

## Consequences

- A swapped or unmounted disk stops the run before its first write,
  including the identity files a first run would create. A preview hits
  the same gate, so it fails rather than plan against the wrong folder.
- The check compares content only, not dates or attributes, so a file
  re-touched without an edit still passes.
- The base learns the canary's content on the first sync that records it;
  until then presence is checked but change is not. Two copies whose
  metadata differs conflict like any file and record once resolved.
- A canary edited on one side blocks every run until it is put back or
  the pair's canary is changed: the intended effect. Inside a run the
  canary is otherwise an ordinary entry and can itself sync or conflict.
