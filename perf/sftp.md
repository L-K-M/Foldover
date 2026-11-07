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

## Cached spelling lookups

A folder's cached listing used to be scanned linearly for every
respellable name, composing each cached name until a match. The cache now
indexes exact bytes and composed forms, so one lookup is a hash probe per
map and a mutation touches the name it changes. The index keeps the
server's listing order for aliases that compose alike, leaves invalid
UTF-8 alone, and is bounded by folder count (512) and a conservative byte
estimate (~32 MiB) that charges key and table overhead; past either bound
the cache starts over rather than grow. Folder keys and Dirty views count too,
and writes evict views when they cross the limit. A fetched folder index that
exceeds the limit can serve its current lookup but is not cached; later
lookups refetch it. The limit bounds retained hints, not a transient directory
listing returned by the server.

`cached_spelling_lookup_bench` (ignored) measures folders of 1k, 10k and
100k mostly-ASCII names with one composed Unicode name, three samples of
200 lookups each, asking for an absent decomposed name so the whole cache
is walked. On this host, before the index (revision `d89d474`) and after:

| cached names | before, per lookup | after, per lookup |
|---|---|---|
| 1k | 184, 192, 194 µs | 359, 372, 369 ns |
| 10k | 1.90, 1.87, 1.89 ms | 308, 343, 346 ns |
| 100k | 18.2, 18.1, 17.7 ms | 343, 351, 352 ns |

The after column is flat in folder size. These are local subprocess
timings against `sftp-server`, not a LAN or NAS result: they measure the
cache's CPU, not wire behaviour, and say nothing about SSH overhead or
throughput. Reproduce with

```sh
cargo test --locked --release -p foldover-endpoint --lib cached_spelling_lookup_bench -- --ignored --nocapture
```
