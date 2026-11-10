#!/usr/bin/env python3
"""Move the changelog's Unreleased entries under the version being released.

Post-bump hook for the release engine (RELEASE_POST_BUMP in
scripts/release.sh, after sync-cargo-version.sh): the release commit
then carries a `## [X.Y.Z] - DATE` section, which release-notes.py
publishes as the GitHub release's changes. Without it a release says
only "See CHANGELOG.md." and the changelog names no version.

With --check it changes nothing and fails unless the section exists:
the pre-tag hook (RELEASE_PRE_TAG), which also runs when a release tags
a version that was bumped by hand.

Usage: RELEASE_NEW_VERSION=X.Y.Z scripts/roll-changelog.py [--check]
"""

import datetime
import os
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
UNRELEASED = re.compile(r"^## \[Unreleased\][^\n]*\n", re.M)
NEXT_SECTION = re.compile(r"^## \[", re.M)


def main() -> None:
    if sys.argv[1:] not in ([], ["--check"]):
        sys.exit("usage: RELEASE_NEW_VERSION=X.Y.Z roll-changelog.py [--check]")
    check_only = sys.argv[1:] == ["--check"]
    version = os.environ.get("RELEASE_NEW_VERSION", "")
    # Keep the heading aligned with the version the pipeline accepts.
    shape = subprocess.run(
        ["bash", str(ROOT / "scripts/version-shape.sh")],
        check=True, capture_output=True, text=True,
    ).stdout
    if not shape or not re.fullmatch(shape, version):
        sys.exit("error: RELEASE_NEW_VERSION must look like X.Y.Z or X.Y.Z-suffix")

    path = ROOT / "CHANGELOG.md"
    text = path.read_text(encoding="utf-8")
    # A rerun after a failed later step finds its section already cut.
    if re.search(rf"^## \[{re.escape(version)}\]", text, re.M):
        print(f"-- CHANGELOG.md has a [{version}] section")
        return
    if check_only:
        sys.exit(
            f"error: CHANGELOG.md has no [{version}] section; run "
            f"RELEASE_NEW_VERSION={version} scripts/roll-changelog.py and commit the result"
        )

    heading = UNRELEASED.search(text)
    if not heading:
        sys.exit("error: CHANGELOG.md has no '## [Unreleased]' section")
    following = NEXT_SECTION.search(text, heading.end())
    end = following.start() if following else len(text)
    entries = text[heading.end():end]
    if not entries.strip():
        sys.exit(
            "error: CHANGELOG.md lists nothing under [Unreleased]; "
            f"describe what {version} changes before releasing it"
        )

    released = f"## [{version}] - {datetime.date.today().isoformat()}\n"
    path.write_text(
        text[:heading.end()] + "\n" + released + entries + text[end:],
        encoding="utf-8",
    )
    print(f"-- moved the Unreleased entries of CHANGELOG.md under [{version}]")


if __name__ == "__main__":
    main()
