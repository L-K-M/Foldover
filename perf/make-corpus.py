#!/usr/bin/env python3
"""Create a new, isolated small-document corpus. Never modifies existing files."""

import argparse
import hashlib
from pathlib import Path

FIXTURE_MARKER = "foldover-perf"


def resolve_fixture_directory(directory: Path) -> Path:
    """Return the physical path whose components carry the fixture marker.

    The marker is a naming convention, not a sandbox: it does not prove
    filesystem ownership and does not defend against a concurrent rename.
    """
    try:
        return directory.resolve()
    except (OSError, RuntimeError) as error:
        raise ValueError(f"cannot resolve fixture directory {directory}: {error}") from error


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--files", type=int, default=10_000)
    parser.add_argument("--bytes", type=int, default=8192)
    parser.add_argument("--width", type=int, default=1000)
    args = parser.parse_args()
    if not 0 < args.files <= 1_000_000 or not 0 <= args.bytes <= 1024 * 1024 or args.width < 1:
        parser.error("invalid corpus bounds")
    try:
        directory = resolve_fixture_directory(args.directory)
    except ValueError as error:
        parser.error(str(error))
    if not any(part.startswith(FIXTURE_MARKER) for part in directory.parts):
        parser.error(f"use an isolated {FIXTURE_MARKER} directory")
    try:
        directory.mkdir(mode=0o700)
    except FileExistsError:
        parser.error(f"fixture directory already exists: {directory}")
    except OSError as error:
        parser.error(f"cannot create fixture directory {directory}: {error}")
    for index in range(args.files):
        folder = directory / f"d-{index // args.width:06}"
        if index % args.width == 0:
            folder.mkdir()
        data = hashlib.shake_256(str(index).encode()).digest(args.bytes)
        with (folder / f"f-{index % args.width:06}").open("xb") as stream:
            stream.write(data)
        if (index + 1) % 10_000 == 0:
            print(f"created {index + 1}", flush=True)


if __name__ == "__main__":
    main()
