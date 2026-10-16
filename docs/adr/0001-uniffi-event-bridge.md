# ADR 0001: UniFFI event bridge under Swift 6

Status: Accepted

## Context

Foldover's macOS app talks to the Rust sync engine over UniFFI
([PLAN.md](../../PLAN.md) §4.5). PLAN.md §8 rows S1 flags the risk:
UniFFI's Swift 6 strict-concurrency support is partial, and the
client API wants high-rate event streams out of Rust plus async calls
in. If the bridge cannot carry that shape, the whole API design
(event streams rather than many small calls) needs rethinking before
P1 builds on it.

## Question

Can a UniFFI 0.32.2 boundary carry Foldover's intended API shape
(async calls in, high-rate event streams out through a
Swift-implemented callback) under Swift 6 strict concurrency with
warnings as errors, fast enough and without leaks?

## Method

Spike `spikes/s1-uniffi-events` builds:

- a Rust crate (`cdylib` + `staticlib`, `uniffi =0.32.2` with the
  tokio feature, `tokio =1.53.x`) exporting an async
  `echo_after(delay_ms, text)`, records `Event`/`StreamReport`, enum
  `EventKind`, a foreign trait `EventSink` with
  `on_batch(events: Vec<Event>)`, and
  `start_stream(sink, total, batch_size)` returning a `StreamHandle`
  with `cancel()` and an async `wait()`;
- a SwiftPM package (swift-tools-version 6.0, macOS 26) over an
  XCFramework binary target, where `Bench` compiles in Swift 6
  language mode with `-warnings-as-errors` and
  `-strict-concurrency=complete`, implements `EventSink` in a final
  class that forwards batches into one actor, and measures:

  - events/s for 1,000,000 events at batch sizes 1, 64 and 1024;
  - `echo_after(0)` round-trip latency, p50 and p95 over 1,000 calls;
  - `phys_footprint` growth in percent after 20 repeated streams;
  - every concurrency escape (`nonisolated(unsafe)`,
    `@unchecked Sendable`, `@preconcurrency`) the bench needed and
    where;
  - that `cancel` stops a stream early and every event arrives in
    `seq` order.

Run it locally on a Mac:

```sh
spikes/s1-uniffi-events/run.sh results.json
```

In CI, `spike-s1` (`.github/workflows/spike-s1.yml`) runs it on
`macos-26` and uploads the JSON. The job fails only when the harness
itself breaks; a measured miss is recorded here instead.

## Pass criteria

From PLAN.md §8 S1, made precise for a 20-stream memory probe rather
than a 1-hour soak:

| Criterion | Target |
|---|---|
| Events/s at some batch size | ≥ 50,000 |
| Memory growth over 20 streams | < 5 % `phys_footprint` |
| Concurrency escapes | confined to one actor wrapper |
| Ordering | every event delivered, `seq` increasing |
| Cancel | stream stops before `total` |

## Results

First CI run of spike-s1
([run 38943571515](https://github.com/L-K-M/Foldover/actions/runs/38943571515)):
GitHub `macos-26` runner, Apple M1 (virtual), macOS 26.6.2, Xcode 26.6,
Swift 6.3.3. A virtual runner is slower than real hardware, so treat
the numbers as a floor.

| Metric | Result | Pass |
|---|---|---|
| Events/s, batch 1 | 155,551 | yes |
| Events/s, batch 64 | 434,973 | yes |
| Events/s, batch 1024 | 538,071 | yes |
| Echo p50 (µs) | 1.71 | n/a |
| Echo p95 (µs) | 1.83 | n/a |
| Memory growth (%) | 3.89 (20 streams of 100,000 events) | yes |
| Cancel stops early | 54,272 of 1,000,000,000 delivered | yes |
| Order complete | every run, 0 out of order | yes |
| Concurrency escapes needed | none | yes |

The generated bindings also compiled under complete strict
concurrency without a warning; the `-warnings-as-errors` bench
compiled with no `nonisolated(unsafe)`, `@unchecked Sendable` or
`@preconcurrency`.

## Decision

Accepted: UniFFI event streams are the client API's push channel.
Rust delivers batches through a foreign `EventSink.on_batch`, and the
Swift side forwards each batch into one actor. Batches hold up to
1,024 events: unbatched delivery already clears the target three
times over, so batching is headroom for richer events, not a
requirement.

## Consequences

**On pass:** PLAN.md §4.5 stands as written; §5.3 gets a real
events/s baseline; the callback-via-one-actor pattern becomes a
documented house rule for every push channel. The spike proves the
generated `EventSink` protocol compiles clean under Swift 6, so no
per-module language-mode exception is needed.

**On throughput miss:** apply the §8 fallback order: batch events
per 100 ms (larger `batch_size` from Rust), then Swift 5 language
mode for the bridge module only, then a C ABI for the event channel.
Each step trades API cleanliness for headroom; the spike's per-batch
numbers say how far down to go.

**On leak or escape miss:** the bridge cannot ship strict-clean; the
fallback is the C-ABI event channel, and PLAN.md §8 S1's risk
stays open into P2 planning.

**On cancel or ordering miss:** the stream primitive itself is
broken; retest after restructuring `wait`/the stream thread before
any P1 work depends on it.
