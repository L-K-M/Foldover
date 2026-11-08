# Performance implementation status

The worthwhile changes from the [action plan](action-plan.md) are implemented.
[Qualification results](../../perf/qualification-results.md) record source
revisions, raw evidence, rejected experiments and remaining validation gaps.
The benchmark budgets and sync safety policies are unchanged.

| Work | Implementation and evidence |
|---|---|
| A0 measurement | Isolated synthetic corpus, engine qualification runner, protocol relay, phase/RSS/CPU artifacts and failure-safe teardown. PR 261 is merged after five review rounds and green CI. Failed runs retain their diagnostics. |
| A1 SFTP reads | Exact-size reads with a bounded EOF probe, bounded control reads and a single READDIR access probe. PR 258, [ADR 0049](../adr/0049-bounded-sftp-reads.md). |
| A2 database costs | Indexed journal cleanup ranges, one resolution-presence query per transaction and a real 256-row commit benchmark. PR 263. Full durability and commit size stay fixed. |
| A3 protocol and names | Reuse attributes only across nonmutating operations, stat-first archive parents, and bounded exact/NFC name indexes. PRs 259 and 267. Missing-path compatibility and external symlink substitution have regressions. |
| A4 local observation | Policy-aware xattr/ACL capture and fused descriptor-based metadata/hash observation. Resource forks remain content. PRs 262 and 265; all sixteen metadata policy combinations are covered. |
| A5 path algorithms | Component prefix indexes, borrowed/ASCII name keys, sorted spelling alignment, and avoidance of planning indexes with no matches. PR 264 and the final integration. |
| A6 memory and locks | Dense shared paths and baseline versions, consumed snapshot storage, exact compact fingerprints, bounded snapshot persistence, lazy previews and decoding outside the engine mutex. PRs 260 and 266 plus [ADR 0052](../adr/0052-compact-observation-storage.md). |
| X1 concurrency | Eight SFTP observation readers and four local readers on SSD/network volumes; one local reader on HDD/unknown media. Recording and control stay on the caller. Copies stay at four, and two-SFTP copies stay serial. Separate left/right overlap was not promoted: the local phase is small relative to the measured remote phase, with additional contention and memory cost. |
| X2 pipelining | Successful read CLOSE and trailing LSTAT share one write, with reply draining/reordering and transport poisoning preserved. [ADR 0051](../adr/0051-batched-sftp-read-completion.md). The two-write variant was rejected for inconsistent results. Further bulk-window and READDIR changes were not justified by the measured small-document workload. |
| X3 release profile | Three paired 10k runs found no repeatable runtime gain from thin LTO/one codegen unit. Keep the release defaults. Build timings used different cache states and do not establish a clean-build cost ratio. |
| X4 archives | Independent file/link archives use the bounded write workers, after copies and before folder removals. Three LAN pairs improved median apply time from 93.014 to 29.644 s for 1,000 deletions. Failure, cancel, panic, single-worker and native undo regressions pass. [ADR 0043](../adr/0043-parallel-copies.md). |

All seventeen local benchmark medians pass their existing budgets. Specific
real million-file local engine cases peak below 500 MB: 473.9 MB for two
populated folders establishing a baseline and 498.3 MB for periodic rehashes.
The final Service preview over expired caches peaks at 489.9 MB while holding
the real preview and polling status. These are isolated qualification processes
on a shared host with uncontrolled OS caches. They do not establish cold-cache or installed-agent release limits.

The case-sensitive APFS, HFS+, exFAT and FAT32 device matrix, FFI drift/type
resolution, layer check and agent lifecycle smoke passed locally. The full
local CI command encounters three native filesystem failures also reproduced
on unchanged main; they are documented with the qualification gaps. Final
integration CI and review are recorded on the PR.

One 100k LAN no-change run completed cleanly. The attempted paired 100k
baseline and the million-file LAN diagnostic lost SSH and failed. No
million-file LAN qualification, complete overnight/device release matrix,
or beta/release readiness is claimed. Re-run those on a stable target;
stat-only SFTP trust, weaker durability, larger commit batches, less frequent
root checks and the other excluded proposals remain outside this work.
