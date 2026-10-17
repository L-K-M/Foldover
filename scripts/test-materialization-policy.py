#!/usr/bin/env python3
"""Check launchd materialization policy without opening cloud file data."""

import argparse
from enum import Enum
import json
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import time
import uuid


PROJECT = Path(__file__).resolve().parent.parent
TEMPLATE = PROJECT / "packaging/macos/ch.lkmc.foldover.agent.plist"
COMMAND_SECONDS = 30
BUILD_SECONDS = 120
REPORT_SECONDS = 20
RETRY_SECONDS = 0.05
EXPECTED_WORKER_SAMPLES = 5


class Materialization(Enum):
    DISABLED = False
    ENABLED = True


def command(arguments, timeout=COMMAND_SECONDS):
    result = subprocess.run(arguments, capture_output=True, text=True, timeout=timeout)
    if result.returncode != 0:
        raise RuntimeError(f"{arguments!r} failed: {result.stdout} {result.stderr}")
    return result


def check_template():
    with TEMPLATE.open("rb") as stream:
        template = plistlib.load(stream)
    if template.get("MaterializeDatalessFiles") is not False:
        raise RuntimeError("Packaged agent must explicitly disable dataless materialization")


def run_case(root, executable, policy):
    label = "ch.lkmc.foldover.tests.materialization." + uuid.uuid4().hex
    domain = f"gui/{os.getuid()}"
    service = f"{domain}/{label}"
    report = root / f"{policy.name}.json"
    definition = {
        "Label": label,
        "ProgramArguments": [str(executable), str(report)],
        "RunAtLoad": True,
        "MaterializeDatalessFiles": policy.value,
        "StandardErrorPath": str(root / f"{policy.name}.stderr"),
    }
    plist = root / f"{policy.name}.plist"
    with plist.open("wb") as stream:
        plistlib.dump(definition, stream)
    try:
        command(["/bin/launchctl", "bootstrap", domain, str(plist)])
        deadline = time.monotonic() + REPORT_SECONDS
        while not report.exists():
            if time.monotonic() >= deadline:
                status = command(["/bin/launchctl", "print", service]).stdout
                error = root / f"{policy.name}.stderr"
                raise RuntimeError(f"Policy probe timed out: {status}\n{error.read_text() if error.exists() else ''}")
            time.sleep(RETRY_SECONDS)
        result = json.loads(report.read_text())
        expected = result["off"] if policy is Materialization.DISABLED else result["on"]
        samples = result["samples"]
        if len(samples) != EXPECTED_WORKER_SAMPLES:
            raise RuntimeError(f"Missing worker policy observations: {result}")
        if sum(observed["isMainThread"] for observed in samples) != 1 or not result["child"]["isMainThread"]:
            raise RuntimeError(f"Worker/child threads were not exercised: {result}")
        for observed in [*samples, result["child"]]:
            if observed["process"] != expected or observed["thread"] not in (result["systemDefault"], expected):
                raise RuntimeError(f"Unexpected {policy.name} process/thread policy: {result}")
        print(f"{policy.name}: {json.dumps(result, sort_keys=True)}")
    finally:
        # Also clean up an uncertain bootstrap; never leave a test job loaded.
        cleanup = subprocess.run(["/bin/launchctl", "bootout", service],
                                 capture_output=True, text=True, timeout=COMMAND_SECONDS)
        remaining = subprocess.run(["/bin/launchctl", "print", service],
                                   capture_output=True, text=True, timeout=COMMAND_SECONDS)
        if cleanup.returncode != 0 or remaining.returncode == 0:
            raise RuntimeError(f"Policy fixture cleanup was not verified: {cleanup.stderr} {remaining.stdout} {remaining.stderr}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check-template", action="store_true")
    options = parser.parse_args()
    check_template()
    if options.check_template:
        print("Agent materialization policy template passed")
        return
    if sys.platform != "darwin":
        raise RuntimeError("Live materialization policy fixtures require macOS")
    with tempfile.TemporaryDirectory(prefix="foldover-policy-") as directory:
        root = Path(directory)
        executable = root / "policy-probe"
        command(["/usr/bin/xcrun", "swiftc", "-swift-version", "6", "-warnings-as-errors",
                 "-target", "arm64-apple-macosx27.0",
                 str(PROJECT / "scripts/materialization-probe.swift"), "-o", str(executable)],
                timeout=BUILD_SECONDS)
        for policy in Materialization:
            run_case(root, executable, policy)
    print("Launchd process, worker and child materialization policy passed")


if __name__ == "__main__":
    main()
