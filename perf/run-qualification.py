#!/usr/bin/env python3
"""Run the isolated Rust harness and retain timings, memory, CPU and host metadata."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import threading
import time


def sample_process(pid, stopped, output, samples, errors, started):
    try:
        _sample_process(pid, stopped, output, samples, errors, started)
    except Exception as error:
        errors.append(f"process sampling failed: {error}")


def _sample_process(pid, stopped, output, samples, errors, started):
    """Keep sampling during long phases, including the runner's SSH children."""
    with output.open("x") as log:
        while not stopped.is_set():
            result = subprocess.run(["ps", "-axo", "pid=,ppid=,rss=,pcpu=,comm="],
                                    capture_output=True, text=True, check=False,
                                    env={**os.environ, "LC_ALL": "C"})
            if result.returncode:
                errors.append(result.stderr.strip() or f"ps exited {result.returncode}")
                return
            processes = []
            for line in result.stdout.splitlines():
                fields = line.split(None, 4)
                if len(fields) == 5:
                    processes.append((int(fields[0]), int(fields[1]), int(fields[2]),
                                      float(fields[3]), fields[4]))
            runner = next((row for row in processes if row[0] == pid), None)
            if runner:
                children = [row for row in processes if row[1] == pid and Path(row[4]).name == "ssh"]
                point = dict(elapsed_seconds=time.monotonic() - started, rss_kib=runner[2],
                             cpu_percent=runner[3], ssh_children=len(children),
                             ssh_rss_kib=sum(row[2] for row in children))
                samples.append(point)
                log.write(json.dumps(point) + "\n")
                log.flush()
            stopped.wait(1)


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
    parser.add_argument("--prepare-only", action="store_true", help="observe, plan and persist snapshots without applying the plan")
    args = parser.parse_args()
    args.output.mkdir(parents=True)  # Never overwrite a previous measurement.
    command = [str(args.runner.resolve()), args.state, args.left, args.right, str(args.iterations)]
    if args.prepare_only:
        command.append("prepare")
    timer_flags = ["-l"] if platform.system() == "Darwin" else ["-v"]
    summary = dict(label=args.label, command=command, host=platform.platform(),
                   python=platform.python_version(), load_start=os.getloadavg(),
                   revision=args.revision or subprocess.check_output(["git", "describe", "--always", "--dirty"]).decode().strip(),
                   binary_sha256=hashlib.sha256(args.runner.read_bytes()).hexdigest(),
                   measurements=[], started_utc=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()))
    process_id = None
    sampler = None
    stopped = threading.Event()
    samples, sampling_errors = [], []
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
                    sampler = threading.Thread(target=sample_process,
                                               args=(int(process_id), stopped, args.output / "samples.jsonl",
                                                     samples, sampling_errors, started))
                    sampler.start()
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
            stopped.set()
            if sampler:
                sampler.join()
    summary.update(sampled_peak_rss_kib=max((row["rss_kib"] for row in samples), default=None),
                   sampled_peak_ssh_children=max((row["ssh_children"] for row in samples), default=None),
                   sampling_errors=sampling_errors)
    summary.update(elapsed_seconds=time.monotonic() - started, load_end=os.getloadavg())
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    if summary["returncode"]:
        raise SystemExit(summary["returncode"])


if __name__ == "__main__":
    main()
