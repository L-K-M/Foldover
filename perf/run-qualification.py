#!/usr/bin/env python3
"""Run the isolated Rust harness and retain timings, memory, CPU and host metadata."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import subprocess
import sys
import threading
import time

MAX_SAMPLING_ERRORS = 100


class ParseError(Exception):
    """A known runner record was malformed and cannot be trusted."""


def parse_nonnegative_int(field, line_number, name):
    try:
        value = int(field)
    except ValueError:
        raise ParseError(f"line {line_number}: {name} {field!r} is not an integer") from None
    if value < 0:
        raise ParseError(f"line {line_number}: {name} {value} is negative")
    return value


def parse_record(line, line_number):
    """Parse a process/phase/metric record, or return None for pass-through lines."""
    text = line.rstrip("\n")
    fields = text.split("\t")
    event = fields[0]
    if event not in ("process", "phase", "metric"):
        return None
    if event == "process":
        if len(fields) != 2:
            raise ParseError(f"line {line_number}: malformed process record {text!r}: "
                             "expected 'process\\tPID'")
        try:
            pid = int(fields[1])
        except ValueError:
            raise ParseError(f"line {line_number}: malformed process record {text!r}: "
                             f"PID {fields[1]!r} is not an integer") from None
        if pid <= 0:
            raise ParseError(f"line {line_number}: malformed process record {text!r}: "
                             f"PID {pid} is not a positive process id")
        return ("process", pid)
    if len(fields) != 4:
        raise ParseError(f"line {line_number}: malformed {event} record {text!r}: "
                         f"expected '{event}\\tITERATION\\tNAME\\tNANOS'")
    iteration = parse_nonnegative_int(fields[1], line_number, f"{event} iteration")
    if not fields[2]:
        raise ParseError(f"line {line_number}: malformed {event} record {text!r}: empty name")
    nanos = parse_nonnegative_int(fields[3], line_number, f"{event} nanoseconds")
    return (event, iteration, fields[2], nanos)


def parse_ps_rows(stdout):
    """Parse `ps -axo pid=,ppid=,rss=,pcpu=,comm=` rows, reporting malformed rows."""
    rows = []
    errors = []
    for line in stdout.splitlines():
        fields = line.split(None, 4)
        if len(fields) != 5:
            errors.append(f"ps row {line!r}: expected PID PPID RSS PCPU COMM")
            continue
        try:
            rows.append((int(fields[0]), int(fields[1]), int(fields[2]),
                         float(fields[3]), fields[4]))
        except ValueError as error:
            errors.append(f"ps row {line!r}: {error}")
    return rows, errors


def record_sampling_error(errors, message):
    if len(errors) < MAX_SAMPLING_ERRORS:
        errors.append(message)
    elif len(errors) == MAX_SAMPLING_ERRORS:
        errors.append(f"further sampling errors suppressed after {MAX_SAMPLING_ERRORS}")


def sample_process(pid, stopped, output, samples, errors, started):
    try:
        _sample_process(pid, stopped, output, samples, errors, started)
    except Exception as error:
        record_sampling_error(errors, f"process sampling failed: {error}")


def _sample_process(pid, stopped, output, samples, errors, started):
    """Keep sampling during long phases, including the runner's SSH children."""
    with output.open("x") as log:
        while not stopped.is_set():
            result = subprocess.run(["ps", "-axo", "pid=,ppid=,rss=,pcpu=,comm="],
                                    capture_output=True, text=True, check=False,
                                    env={**os.environ, "LC_ALL": "C"})
            if result.returncode:
                record_sampling_error(errors, result.stderr.strip()
                                      or f"ps exited {result.returncode}")
                return
            processes, row_errors = parse_ps_rows(result.stdout)
            for message in row_errors:
                record_sampling_error(errors, message)
            runner = next((row for row in processes if row[0] == pid), None)
            if runner:
                children = [row for row in processes
                            if row[1] == pid and Path(row[4]).name == "ssh"]
                point = dict(elapsed_seconds=time.monotonic() - started, rss_kib=runner[2],
                             cpu_percent=runner[3], ssh_children=len(children),
                             ssh_rss_kib=sum(row[2] for row in children))
                samples.append(point)
                log.write(json.dumps(point) + "\n")
                log.flush()
            stopped.wait(1)


def sample_point(pid):
    """Return ((rss_kib, cpu_percent), None) for a live PID, or (None, error)."""
    result = subprocess.run(["ps", "-p", str(pid), "-o", "rss=,pcpu="],
                             capture_output=True, text=True, check=False,
                             env={**os.environ, "LC_ALL": "C"})
    if result.returncode:
        return None, result.stderr.strip() or f"ps exited {result.returncode}"
    values = result.stdout.split()
    if len(values) != 2:
        return None, f"ps output {result.stdout!r}: expected RSS and CPU"
    try:
        rss = int(values[0])
        cpu = float(values[1])
    except ValueError:
        return None, f"ps output {result.stdout!r}: expected integer RSS and numeric CPU"
    if rss < 0:
        return None, f"ps reported negative RSS {rss}"
    return (rss, cpu), None


def stop_child_group(child, errors):
    """Stop the child's own process group and reap the child.

    The child was started with ``start_new_session=True``, so its pid is also
    its process group id.  A group that disappeared between ``poll`` and the
    signal is a normal exit race; any other OS error is recorded so the
    invocation can fail instead of silently reporting success.
    """
    if child is None or child.poll() is not None:
        return
    # time is a wrapper around the Rust runner and its SSH children.  Stop all
    # of this invocation's group, never another process group.
    try:
        os.killpg(child.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    except OSError as error:
        errors.append(f"SIGTERM to process group {child.pid} failed: "
                      f"{type(error).__name__}: {error}")
    try:
        child.wait(timeout=5)
        return
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(child.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    except OSError as error:
        errors.append(f"SIGKILL to process group {child.pid} failed: "
                      f"{type(error).__name__}: {error}")
    try:
        child.wait(timeout=5)
    except subprocess.TimeoutExpired:
        errors.append(f"process group {child.pid} did not exit after SIGKILL")


def resolve_revision(args):
    if args.revision:
        return args.revision
    try:
        return subprocess.check_output(["git", "describe", "--always", "--dirty"],
                                       stderr=subprocess.DEVNULL, text=True).strip()
    except (OSError, subprocess.CalledProcessError) as error:
        raise SystemExit("source revision unavailable: pass --revision with the measured "
                         f"binary's commit, or run inside a git checkout ({error})") from None


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
    parser.add_argument("--prepare-only", action="store_true",
                        help="observe, plan and persist snapshots without applying the plan")
    args = parser.parse_args()
    args.output.mkdir(parents=True)  # Never overwrite a previous measurement.
    command = [str(args.runner.resolve()), args.state, args.left, args.right, str(args.iterations)]
    if args.prepare_only:
        command.append("prepare")
    timer_flags = ["-l"] if platform.system() == "Darwin" else ["-v"]
    summary = dict(label=args.label, command=command, host=platform.platform(),
                   python=platform.python_version(), load_start=os.getloadavg(),
                   revision=args.revision, binary_sha256=None, measurements=[],
                   parsing_errors=[], sampling_errors=[], returncode=None, error=None,
                   started_utc=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()))
    process_id = None
    sampler = None
    stopped = threading.Event()
    samples = []
    child = None
    returncode = None
    started = time.monotonic()
    try:
        if summary["revision"] is None:
            try:
                summary["revision"] = resolve_revision(args)
            except SystemExit as error:
                summary["error"] = str(error)
                raise
        with (args.output / "runner.tsv").open("x") as log, \
                (args.output / "resources.txt").open("x") as resources:
            try:
                summary["binary_sha256"] = hashlib.sha256(args.runner.read_bytes()).hexdigest()
            except OSError as error:
                summary["error"] = f"runner unreadable: {error}"
                raise SystemExit(1) from None
            try:
                child = subprocess.Popen(["/usr/bin/time", *timer_flags, *command],
                                         stdout=subprocess.PIPE, stderr=resources,
                                         text=True, bufsize=1, start_new_session=True)
            except OSError as error:
                summary["error"] = f"runner launch failed: {error}"
                raise SystemExit(1) from None
            for line_number, line in enumerate(child.stdout, start=1):
                log.write(line)
                log.flush()
                print(line, end="", flush=True)
                try:
                    record = parse_record(line, line_number)
                except ParseError as error:
                    summary["parsing_errors"].append(str(error))
                    continue
                if record is None:
                    continue
                if record[0] == "process":
                    if process_id is not None:
                        summary["parsing_errors"].append(
                            f"line {line_number}: duplicate process record "
                            f"{line.rstrip()!r}: already sampling pid {process_id}")
                        continue
                    process_id = record[1]
                    sampler = threading.Thread(
                        target=sample_process,
                        args=(process_id, stopped, args.output / "samples.jsonl",
                              samples, summary["sampling_errors"], started))
                    sampler.start()
                    continue
                point = dict(event=record[0], iteration=record[1],
                             phase=record[2], ns=record[3])
                if process_id is not None:
                    values, error = sample_point(process_id)
                    if error is None:
                        point.update(rss_kib=values[0], cpu_percent=values[1])
                    else:
                        record_sampling_error(summary["sampling_errors"],
                                              f"line {line_number}: {error}")
                summary["measurements"].append(point)
            returncode = child.wait()
    except KeyboardInterrupt:
        summary["error"] = "cancelled"
        raise
    except Exception as error:
        summary["error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        cleanup_errors = []
        try:
            stop_child_group(child, cleanup_errors)
            if child is not None and child.stdout is not None:
                try:
                    child.stdout.close()
                except OSError as error:
                    cleanup_errors.append(f"closing runner stdout for pid {child.pid} "
                                          f"failed: {type(error).__name__}: {error}")
            if child is not None and returncode is None:
                returncode = child.poll()
        except Exception as error:
            cleanup_errors.append(f"cleanup failed: {type(error).__name__}: {error}")
        # The sampler is non-daemon: always signal and join it before writing
        # the summary or propagating any cleanup error.
        stopped.set()
        if sampler is not None:
            sampler.join()
        summary["returncode"] = returncode
        summary["cleanup_errors"] = cleanup_errors
        if cleanup_errors and not summary.get("error"):
            summary["error"] = "; ".join(cleanup_errors)
        summary.update(sampled_peak_rss_kib=max((row["rss_kib"] for row in samples), default=None),
                       sampled_peak_ssh_children=max((row["ssh_children"] for row in samples),
                                                     default=None),
                       elapsed_seconds=time.monotonic() - started,
                       load_end=os.getloadavg())
        (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        # A failed cleanup must fail the invocation, but it must not replace an
        # exception already propagating from the measured run.
        if cleanup_errors and sys.exc_info()[0] is None:
            raise SystemExit(1)
    if summary["parsing_errors"]:
        print(f"runner emitted {len(summary['parsing_errors'])} malformed "
              "process/phase/metric record(s); see summary.json parsing_errors",
              file=sys.stderr)
    if returncode:
        raise SystemExit(128 - returncode if returncode < 0 else returncode)
    if summary["parsing_errors"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
