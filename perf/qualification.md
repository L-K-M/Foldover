# End-to-end qualification harness

Build the engine example in release mode, then generate a new fixture:

```sh
cargo build --release --locked -p foldover-engine --example performance
mkdir -p /private/tmp/foldover-perf-example
python3 perf/make-corpus.py /private/tmp/foldover-perf-example/source --files 10000
mkdir /private/tmp/foldover-perf-example/target
python3 perf/run-qualification.py --runner target/release/examples/performance \
  --state /private/tmp/foldover-perf-example/state \
  --left /private/tmp/foldover-perf-example/source \
  --right /private/tmp/foldover-perf-example/target \
  --iterations 3 --label development --output /private/tmp/foldover-perf-example/results
```

Use an absolute `sftp://user@host/path` root for real LAN runs. Authentication
uses the user's ordinary noninteractive OpenSSH configuration. Create a
dedicated remote fixture directory first. The runner accepts only roots and
state paths containing `foldover-perf`, refuses an existing mismatched catalog,
and never opens the installed agent's state. It mirrors left to right, keeps
normal archives/leases and approves its freshly computed synthetic plan. Never
point it at user data. Failed/conflicting/incomplete runs exit unsuccessfully.

The tool measures actual endpoint open, readiness, each observation, alignment,
planning, input load, preview, snapshot export/store, apply, cumulative commit
time and total duration. `RunProgress::measured` is a default diagnostic hook:
normal UI consumers ignore it, and callbacks stay on the run thread. The example
uses the same public engine/store APIs as the agent; it measures storage time,
not contention on the service's mutex. Test concurrent service activity separately.

Results retain a TSV trace, `/usr/bin/time` resource report including peak RSS,
and JSON host/revision/load/phase samples with RSS and CPU readings. Sampling and
reporting add overhead to total time. Kernel peak RSS covers transient peaks
between samples. Phases can overlap only when the implementation supports it;
do not add overlapping phase times to predict total duration.

Use 10k files for development, 100k for comparisons and 1M for qualification.
Vary `--bytes` (1/8/32 KiB and a separate large-file set) and `--width` for flat
and nested trees. Corpus bytes are deterministic, unique, incompressible SHAKE
output so matching fixtures can be generated independently on each host. First
copy uses an empty target. No-change runs reuse the same state and roots; edits,
deletions and permission changes must affect only synthetic entries. Record all
fixture mutations, NAS hardware/filesystem, RTT and cache conditions separately.

Run three paired before/after samples with matching fixtures and source revisions.
Keep initial population and warm no-change runs separate. In particular, a local
`sftp-server` run omits SSH encryption, authentication and LAN latency. Do not
describe it as NAS qualification. Keep existing performance budgets unchanged.

Generate only bounded corpora, monitor free space, and remove only the exact
uniquely named fixture directories created for the experiment after preserving
results. Neither corpus generator nor qualification runner cleans arbitrary paths.
