#!/usr/bin/env python3
"""Give each signed agent build its own Service Management code constraint."""

import argparse
from pathlib import Path
import plistlib
import re
import subprocess


LABEL_PREFIX = "ch.lkmc.foldover.agent"


def signed_label(helper):
    result = subprocess.run(
        ["codesign", "--display", "--verbose=4", str(helper)],
        capture_output=True, text=True, check=True, timeout=30,
    )
    hashes = re.findall(r"^CDHash=([0-9a-f]{40})$", result.stderr, re.MULTILINE)
    if len(hashes) != 1:
        raise RuntimeError("Expected exactly one Apple-silicon agent code hash")
    return f"{LABEL_PREFIX}.{hashes[0]}"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("stamp", "check"))
    parser.add_argument("helper", type=Path)
    parser.add_argument("plist", type=Path)
    args = parser.parse_args()
    label = signed_label(args.helper)
    contents = plistlib.loads(args.plist.read_bytes())
    if args.mode == "check":
        if contents.get("Label") != label:
            raise RuntimeError("Agent launchd label does not match its signed code hash")
        return
    contents["Label"] = label
    args.plist.write_bytes(plistlib.dumps(contents))


if __name__ == "__main__":
    main()
