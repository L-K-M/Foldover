#!/usr/bin/env python3
"""Run the isolated Rust harness and retain timings, memory, CPU and host metadata."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runner", required=True, type=Path)
    parser.add_argument("--state", required=True)
    parser.add_argument("--left", required=True)
    parser.add_argument("--right", required=True)
    parser.add_argument("--iterations", default=3, type=int)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--label", required=True)
    parser.add_argument("--revision", help="source revision of a saved comparison binary")
    args = parser.parse_args()
    args.output.mkdir(parents=True)  # Never overwrite a previous measurement.
    command = [str(args.runner.resolve()), args.state, args.left, args.right, str(args.iterations)]
    timer_flags = ["-l"] if platform.system() == "Darwin" else ["-v"]
    summary = dict(label=args.label, command=command, host=platform.platform(),
                   python=platform.python_version(), load_start=os.getloadavg(),
                   revision=args.revision or subprocess.check_output(["git", "describe", "--always", "--dirty"]).decode().strip(),
                   binary_sha256=hashlib.sha256(args.runner.read_bytes()).hexdigest(),
                   measurements=[], started_utc=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()))
    process_id = None
    started = time.monotonic()
    with (args.output / "runner.tsv").open("x") as log, (args.output / "resources.txt").open("x") as resources:
        child = subprocess.Popen(["/usr/bin/time", *timer_flags, *command], stdout=subprocess.PIPE,
                                 stderr=resources, text=True, bufsize=1)
        try:
            for line in child.stdout:
                log.write(line)
                log.flush()
                print(line, end="", flush=True)
                fields = line.rstrip("\n").split("\t")
                if fields[:1] == ["process"]:
                    process_id = fields[1]
                if fields[:1] in (["phase"], ["metric"]) and len(fields) == 4:
                    point = dict(event=fields[0], iteration=int(fields[1]), phase=fields[2], ns=int(fields[3]))
                    if process_id:
                        sampled = subprocess.run(["ps", "-p", process_id, "-o", "rss=,pcpu="],
                                                 capture_output=True, text=True, check=False)
                        values = sampled.stdout.split()
                        if len(values) == 2:
                            point.update(rss_kib=int(values[0]), cpu_percent=float(values[1]))
                    summary["measurements"].append(point)
            summary["returncode"] = child.wait()
        finally:
            if child.poll() is None:
                child.terminate()
                child.wait()
    summary.update(elapsed_seconds=time.monotonic() - started, load_end=os.getloadavg())
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    if summary["returncode"]:
        raise SystemExit(summary["returncode"])


if __name__ == "__main__":
    main()
