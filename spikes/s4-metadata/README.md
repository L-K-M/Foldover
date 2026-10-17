# S4: Metadata fidelity

Run on macOS 27:

```sh
python3 -m unittest discover -s spikes/s4-metadata -v
mkdir -p target
python3 spikes/s4-metadata/spike.py target/s4-results.json
```

The harness creates a case-sensitive APFS source image and APFS/exFAT
target images. It probes each target before copying a corpus through
`ditto --rsrc --extattr --acl` and back. The corpus covers content, mode,
fractional mtime, Finder tags, resource forks, FinderInfo, ACLs, flags,
symlinks, hard links, sparse files, NFC/NFD names, colons, case collisions
and bundle trees. Directory-root metadata is included.

Each difference is retained in JSON. Unsupported metadata predicts its
absence, never corrupted content or attribute values. Unavailable probes
do not excuse losses. Known name collisions and unrepresentable entry kinds
are blocked before copying. AppleDouble sidecars are identified by a
bounded v2 header/table check and recorded separately from logical entries.

To add an existing Samba mount, use a unique label and its mount point:

```sh
python3 spikes/s4-metadata/spike.py target/s4-results.json \
  --mounted-target samba-fruit=/Volumes/S4Fruit \
  --mounted-target samba-no-fruit=/Volumes/S4Plain
```

Writes use fresh temporary directories inside those mounts. Existing data
is not copied, replaced or deleted. Only the harness's disk images are
detached, with a forced retry if an owned image is busy. Final cleanup
failure fails the harness.

If a fixture directory inside an owned image cannot be removed, its error
and remaining name bytes are recorded as a failed operation. The harness
continues only because detaching and deleting that image removes the whole
owned filesystem. Cleanup failure on an existing mount still aborts.

The command's success means measurements completed. Inspect
`harness_completed`, `tested_targets_passed`, per-case differences and
`unavailable_targets`. Failures retain completed measurements with a failed
harness verdict.
`full_s4_passed` remains false: SFTP extension/operation probes and physical
target validation are outstanding. `ditto` is a baseline, not a claim about
the future Rust endpoint. Rename-over success does not prove crash atomicity.
