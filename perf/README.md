# Performance budgets

`cargo xtask bench-check` runs the criterion benches, compares each
median against `budgets.toml`, and fails when a median is missing or
over budget. PLAN.md §5.2 lists these benches as the stored baselines
for the scan, hash, plan and DB hot paths.

## What is measured

One bench per hot path, in the crate that owns the code. Criterion
ids are the bench function names; estimates land in
`target/criterion/<id>/new/estimates.json`.

| Id | Crate | Hot path |
|---|---|---|
| `scan_10k_files` | `foldover-endpoint` | `LocalEndpoint::observe` over 10_000 small files in nested directories |
| `hash_64_mib` | `foldover-fs` | `Root::hash_file`, the bounded BLAKE3 read behind `LocalEndpoint::hash_file`, over a 64 MiB file |
| `plan_100k_entries` | `foldover-engine` | `plan_candidates` over two 100_000-entry sides with a synthetic base mixing unchanged, copied, conflicting, new and deleted files |
| `plan_skipped_100k_entries` | `foldover-engine` | `plan_resolved` over the same 100_000-row pair with all 1_000 top-level folders skipped, the focused prefix-index workload |
| `prefix_index_100k_entries_1000_prefixes` | `foldover-engine` | `PathPrefixIndex` ancestor lookups: 100_000 paths against 1_000 roots, the shared helper behind unavailable roots, failed writes and skipped branches |
| `archive_pages_5k` | `foldover-endpoint` | `LocalEndpoint::archived_items` listing 5,000 items in 50 folders, 100 per page |
| `db_commit_100k` | `foldover-state` | `PairStore::update_base` committing 100_000 base entries |
| `db_commit_batches_100k` | `foldover-state` | `PairStore::update_base` committing one run's 100_000 base entries and journal entries in the engine's 256-row batches |
| `db_read_100k` | `foldover-state` | `PairStore::base` reading the 100_000-entry base back |
| `db_snapshot_unchanged_100k` | `foldover-state` | `PairStore::replace_snapshot` storing the same 100_000-entry observation again |
| `db_snapshot_rewrite_100k` | `foldover-state` | `PairStore::replace_snapshot` with every one of the 100_000 fingerprints changed |

Fixtures are built in temp directories on every run; nothing outside
`target/` and the system temp directory is touched.

Unicode normalization has a separate preparation regression budget. Run
it explicitly on an idle host in release mode:

```sh
cargo test --release -p foldover-engine unicode_respelling_prepare_budget -- --ignored --nocapture
```

It aligns 50,000 respelled names within five seconds, the entire planning
budget for 500,000 paths in PLAN.md section 5.3. Setup is not timed. It is
excluded from ordinary unit tests to avoid wall-clock assertions on busy
runners; the normal suite checks ambiguous aliases and unreadable paths.

The journal batch cleanup has a scaling check in the same style, covering
100_000 to 800_000 writes in 256-row batches:

```sh
cargo test --release -p foldover-state journal_batches_commit_linearly -- --ignored --nocapture
```

Each count must stay within 3x of the 100_000-write time per write, so a
per-batch journal cleanup that scans the run's own entries (quadratic in
the run's size) fails from 400_000 writes on. It is ignored for the same
reason as the preparation budget; the criterion bench is the gate.

## How budgets were set

Each budget is a maximum median in milliseconds, set at roughly 3x
the median measured on the development machine they were calibrated
on (2026-10-20, Linux, release profile, short criterion windows). The
headroom absorbs slower and noisier CI runners, which is where the
gate runs weekly and on dispatch (`bench.yml`). A budget that fails
on runners but passes locally was set too tight, not too loose.

## Updating

1. Run `cargo xtask bench-check` on an idle machine and read the
   medians from its table.
2. Set each entry in `budgets.toml` to about 3x the local median,
   rounded up.
3. Keep every id identical to a criterion bench function name: a
   budget without matching criterion estimates fails the check, and
   a bench without a budget is silently unchecked.
