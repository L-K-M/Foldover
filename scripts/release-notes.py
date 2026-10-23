#!/usr/bin/env python3
"""Print GitHub release notes: install steps plus the version's changelog."""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent

INSTALL = """\
## Install

Foldover is signed ad hoc, not notarized. macOS 27 on Apple silicon.

1. Download `Foldover-{version}.zip` below and unzip it.
2. Move `Foldover.app` to `/Applications`.
3. Allow it to open by removing the download quarantine:

   ```sh
   xattr -dr com.apple.quarantine /Applications/Foldover.app
   ```

4. Open Foldover and choose **Turn On** to start the background agent.

Preview a new pair before its first sync. Replaced and removed files are kept
in each folder's `.foldover/archive` for 30 days.
"""


def changelog(version: str) -> str:
    text = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
    match = re.search(
        rf"^## \[{re.escape(version)}\][^\n]*\n(.*?)(?=^## \[|\Z)", text, re.M | re.S
    )
    return match.group(1).strip() if match else "See CHANGELOG.md."


def main() -> None:
    if len(sys.argv) != 2 or not re.fullmatch(r"\d+\.\d+\.\d+", sys.argv[1]):
        sys.exit("usage: release-notes.py X.Y.Z")
    version = sys.argv[1]
    print(INSTALL.format(version=version))
    print("## Changes\n")
    print(changelog(version))


if __name__ == "__main__":
    main()
