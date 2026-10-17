# ADR 0001: UniFFI event bridge under Swift 6

Status: Accepted with a macOS 27 memory caveat

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
- a SwiftPM package (swift-tools-version 6.0, macOS 27) over an
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
the macOS 27 `xcode-27` image and uploads the JSON. The job fails only when
the harness itself breaks; a measured miss is recorded here instead.

The follow-up soak repeats 100,000-event streams at batch size 1,024
for one hour. It samples `phys_footprint` every minute after teardown,
checks every event's exact sequence, and checks weak references to the
collector, sink and handle after each stream. Rust signals completion
before its callback closure is dropped, so teardown gets up to one second.
Finally, `/usr/bin/leaks` scans the still-live process; its full output
and exit status are retained in the JSON.

```sh
S1_SOAK_SECONDS=3600 spikes/s1-uniffi-events/run.sh results.json
```

Manual CI defaults to this full soak. PRs run a ten-second smoke probe,
which deliberately cannot pass the one-hour criterion. Missing memory
samples or an unavailable heap scan fail their measurement checks.

## Pass criteria

From PLAN.md §8 S1, with the short memory probe and hour gate separate:

| Criterion | Target |
|---|---|
| Events/s at some batch size | ≥ 50,000 |
| Memory growth over 20 streams | < 5 % `phys_footprint` |
| Soak duration | ≥ 3,600 s of repeated streams |
| Memory growth over the soak | < 5 % `phys_footprint`, after warmup |
| Soak object lifetime | no collector, sink or handle retained after teardown |
| Heap scan after the soak | 0 leaked allocations and bytes; scanner succeeds |
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

### One-hour follow-up

Full soak [run 39002938713](https://github.com/L-K-M/Foldover/actions/runs/39002938713),
commit `39f0252`, on a virtual Apple M1, macOS 26.6.2,
Xcode 26.6 and Swift 6.3.3. Every measurement check passed.

| Metric | Result |
|---|---|
| Soak elapsed | 3,600.11 s |
| Completed streams | 24,633 × 100,000 events, batch 1,024 |
| Events checked during the soak | 2,463,300,000 |
| Failed delivery or ordering checks | 0 |
| Retained collectors, sinks or handles after teardown | 0 |
| `phys_footprint`, start → end | 2,819,136 → 2,901,056 bytes (+2.91 %) |
| Largest sampled footprint | 2,917,440 bytes |
| Heap scan | 0 leaked allocations, 0 leaked bytes, exit 0 |
| Best throughput before the soak | 582,667 events/s, batch 1,024 |
| Swift regression and policy tests | 7 passed |

The footprint settled after the first two minutes, with occasional
16 KiB fluctuations. No leak was observed over the hour. This closes
the short probe's duration gap for the in-process UniFFI boundary;
agent IPC and physical-Mac performance are not measured here.

The follow-up also closes an ordering blind spot: the old collector
only rejected decreasing sequence numbers. It accepted duplicates and
gaps, including `0, 1, 1, 3`. Regression tests now require contiguous
sequence numbers from zero, including across callback batches.

The preceding [smoke run](https://github.com/L-K-M/Foldover/actions/runs/39001933830)
completed 63 streams in 10.14 seconds with zero retained objects and
zero leaks. Its one-hour check correctly failed. XCTest runs these
tests because the macOS Swift Testing runner re-entered the CLI.

### macOS 27 follow-up

The owner moved the app to macOS 27 on Apple silicon. Full soak
[run 39019690780](https://github.com/L-K-M/Foldover/actions/runs/39019690780),
commit `3a26989`, used macOS 27.0.1 (26A434), Xcode 27.0 and a virtual
Apple M1. The hour checks passed, but the aggregate verdict was false:

| Metric | Result |
|---|---|
| Soak elapsed | 3,600.01 s |
| Completed streams | 25,356 × 100,000 events, batch 1,024 |
| Events checked during the soak | 2,535,600,000 |
| Failed delivery or ordering checks | 0 |
| Retained collectors, sinks or handles | 0 |
| Soak footprint | 3,048,832 → 3,147,136 bytes (+3.22 %) |
| Heap scan | 0 leaked allocations, 0 leaked bytes, exit 0 |
| Best throughput | 574,983 events/s, batch 1,024 |
| Separate 20-stream footprint | 2,835,840 → 2,999,680 bytes (+5.78 %): fail |

The short probe exceeded its 5 % limit; its cause is not established.
Keep the actor/batching design while carrying this memory miss into
production verification. The hour checks cover in-process lifetimes and a
live heap scan on this VM. Agent IPC and physical-Mac performance remain
unmeasured.

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
