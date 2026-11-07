# Performance implementation status

This tracks the [action plan](action-plan.md). Pending work is not a completed
optimization or qualified release result.

| Work | Current evidence and next step |
|---|---|
| A0 baselines and harnesses | Permanent packet counter and framed relay added with A1/A3. Loaded-host local cold scan: 5.40–7.00 s for 10k files at `2301115`, above the unchanged 1 s budget. Prepare/DB/memory and real LAN qualification remain. |
| A1 bounded SFTP reads | PR 258; failing-before/passing-after packet budget, full net/endpoint tests and clippy. [ADR 0049](../adr/0049-bounded-sftp-reads.md). CI/review pending. |
| A2 journal and resolutions | Isolated helper implementation in progress. |
| A3 write chains | Attribute reuse and archive-parent stat-first checks have failing-before/passing-after request budgets; retains final-state and symlink checks. Wide-folder spelling cache now indexes exact and composed names, bounded by folder count and a conservative byte estimate; local 1k/10k/100k lookups are flat at ~0.3–0.4 µs against 0.19–18 ms before. LAN/NAS qualification remains. |
| A4 metadata capture | Isolated helper implementing policy-aware capture; descriptor fusion follows. |
| A5 path/name algorithms | Isolated helper implementing prefix indexes; name-key/merge work follows. |
| A6 copies and locks | Pending implementation and measurement. |
| X1–X4 experiments | Pending post-fix phase measurements. No defaults changed. |

The real LAN benchmark target has been requested. Local subprocess tests
cannot establish NAS throughput, SSH overhead or million-file qualification.
See [protocol measurements](../../perf/sftp.md) for reproducible request counts.
