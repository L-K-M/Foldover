#!/usr/bin/env python3
"""Create a new, isolated small-document corpus. Never modifies existing files."""

import argparse
import hashlib
import os
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--files", type=int, default=10_000)
    parser.add_argument("--bytes", type=int, default=8192)
    parser.add_argument("--width", type=int, default=1000)
    args = parser.parse_args()
    if not 0 < args.files <= 1_000_000 or not 0 <= args.bytes <= 1024 * 1024 or args.width < 1:
        parser.error("invalid corpus bounds")
    if not any(part.startswith("foldover-perf") for part in args.directory.parts):
        parser.error("use an isolated foldover-perf directory")
    args.directory.mkdir(mode=0o700)
    for index in range(args.files):
        folder = args.directory / f"d-{index // args.width:06}"
        if index % args.width == 0:
            folder.mkdir(mode=0o700)
        data = hashlib.shake_256(str(index).encode()).digest(args.bytes)
        with (folder / f"f-{index % args.width:06}").open("xb") as stream:
            stream.write(data)
            # Inherited NAS ACLs can grant execute permission to new files.
            # These fixtures are documents; keep that cross-platform content
            # property explicit even when permission syncing is disabled.
            os.fchmod(stream.fileno(), 0o600)
        if (index + 1) % 10_000 == 0:
            print(f"created {index + 1}", flush=True)


if __name__ == "__main__":
    main()
