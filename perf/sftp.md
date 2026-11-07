# SFTP protocol measurements

`sftp-relay.py` forwards framed requests and responses without changing their
contents. It records JSON lines containing packet type, bytes, request ID,
outstanding count, process ID, elapsed time and DATA payload bytes. Use an
isolated fixture: the log describes the workload, and the server command runs
with your ordinary privileges.

```sh
python3 perf/sftp-relay.py --log events.jsonl -- /usr/libexec/sftp-server
```

The client must speak SFTP on the relay's standard streams. The endpoint
tests' `counted` fixture demonstrates spawning it through `Session::spawn`.
For a real network measurement, replace the server command with
`ssh -o BatchMode=yes -s HOST sftp`. An optional `--delay-ms` delays every
response frame; that measures per-message sensitivity, not actual network RTT.

Run the deterministic request budgets with:

```sh
cargo test --locked -p foldover-net known_length_read_packet_budget -- --nocapture
cargo test --locked -p foldover-endpoint unchanged_staged_metadata_uses_one_stat -- --nocapture
cargo test --locked -p foldover-endpoint archive_parent_reuse -- --nocapture
```

The unchanged staged-metadata regression failed before the fix with two
LSTATs instead of one. The final version now reuses the attributes read by
metadata handling only when no mutation followed them. A SETSTAT still needs
a final read-back; `changed_staged_metadata_reads_back_the_final_state` checks
both that request and equality with the server's final stored version.

The archive-parent regression failed because each repeated sibling issued
four refused MKDIRs for `.foldover/archive/run/parent`, followed by four
LSTATs. Stat-first reuse reduces that to four LSTATs. Initial missing parents
cost an extra stat per component, amortized across siblings. Each component
is still checked, including an external directory-to-symlink substitution;
there is no cache claiming a directory stayed unchanged without checking it.

These are request-count measurements against a local OpenSSH server, not
complete LAN speedups. Keep target, corpus, source revision, host load and
cache conditions with timing results, and compare at least three paired runs.
