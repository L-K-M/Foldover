#!/usr/bin/env python3
"""Exercise the built CLI's streaming paths against a controlled agent.

The fixture is a stdlib-only Unix-socket server speaking just enough of the
local JSON-RPC protocol for `sync --all` and `sync ID --progress`; it runs as
the same user so the client's peer-credential check passes unchanged. Wire
shapes mirror crates/foldover-proto/src/local: Hello, DaemonStatus,
PairRecord/PairPage and SyncStatus. Linux's same-user authentication admits
the fixture; macOS authenticates signed helpers.
"""

import argparse
import json
import os
from pathlib import Path
import selectors
import socket
import subprocess
import sys
import tempfile
import threading
import time

# The CLI waits at most five seconds per call, so a gated job is held by
# repeated Working answers, never by a silent socket.
EARLY_LINE_SECONDS = 10
DRAIN_SECONDS = 15
MAX_CONCURRENT = 4
PAIR_NOT_FOUND = -32003
METHOD_NOT_FOUND = -32601

PAIR_IDS = [f"{number:032x}" for number in range(1, 7)]
# The first listed pair finishes on `sync.run`; the rest stay Working until
# the test releases them.
INSTANT = PAIR_IDS[0]
# A seventh pair, reachable through pair.get but not listed, drives the
# `sync ID --progress` scenario without touching `sync --all`.
PROGRESS_ID = f"{7:032x}"

WORKING = {
    "state": "working", "job": "run", "cancelling": False,
    "progress": {"listed": 4, "checked": 4, "changes": 0, "applied": 0,
                 "bytes": 0, "copied": 0, "rate": 0},
}
FINISHED = {
    "state": "finished",
    "result": {"cancelled": False, "copied": 1, "moved": 0, "updated": 0,
               "archived": 0, "recovered": 0, "conflicts": 0, "failures": [],
               "failure_count": 0, "archive": "2026-10-24T00-00-00.000Z"},
}
# sync.status polls for PROGRESS_ID: an identical repeat of the run's
# status, then changed counters, then Finished forever after.
PROGRESS_STAGES = [
    {"state": "working", "job": "run", "cancelling": False,
     "progress": {"listed": 8, "checked": 5, "changes": 0, "applied": 0,
                  "bytes": 0, "copied": 0, "rate": 0}},
    {"state": "working", "job": "run", "cancelling": False,
     "progress": {"listed": 8, "checked": 8, "changes": 3, "applied": 1,
                  "bytes": 4096, "copied": 2048, "rate": 1024}},
    FINISHED,
]


def pair_record(pair_id, name):
    return {
        "id": pair_id,
        "revision": 1,
        "configuration": {
            "name": name,
            "left_root": list(f"/fixture/{name}-left".encode()),
            "right_root": list(f"/fixture/{name}-right".encode()),
            "mode": {"kind": "sync"},
            "options": {
                "modification_times": "sync",
                "extended_attributes": "sync",
                "permissions": "sync",
                "durability": "standard",
                "schedule": {"kind": "manual"},
                "filters": [],
                "size_limit": None,
                "age_limit": None,
            },
        },
    }


PAIRS = [pair_record(pair_id, f"Pair {index}") for index, pair_id in enumerate(PAIR_IDS, 1)]
RECORDS = {record["id"]: record for record in PAIRS}
RECORDS[PROGRESS_ID] = pair_record(PROGRESS_ID, "Progress")


class Fixture:
    """A minimal agent on a private socket; gated pairs answer Working
    until `release` is set. Tracks overlapping jobs to bound fan-out."""

    def __init__(self, endpoint, version):
        self.version = version
        self.release = threading.Event()
        self.saturated = threading.Event()
        self.lock = threading.Lock()
        self.running = set()
        self.completed = set()
        self.peak = 0
        self.polls = {}
        self.handlers = []
        self.closed = False
        self.listener = socket.socket(socket.AF_UNIX)
        self.listener.bind(str(endpoint))
        self.listener.listen(16)
        self.listener.settimeout(0.1)
        self.thread = threading.Thread(target=self.serve, daemon=True)
        self.thread.start()

    def close(self):
        self.closed = True
        self.listener.close()
        self.thread.join(timeout=EARLY_LINE_SECONDS)
        for handler in self.handlers:
            handler.join(timeout=1)

    def serve(self):
        while not self.closed:
            try:
                connection, _ = self.listener.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            handler = threading.Thread(target=self.handle, args=(connection,), daemon=True)
            self.handlers.append(handler)
            handler.start()

    def handle(self, connection):
        try:
            with connection:
                connection.settimeout(DRAIN_SECONDS)
                pending = b""
                while True:
                    chunk = connection.recv(65536)
                    if not chunk:
                        return
                    pending += chunk
                    while b"\n" in pending:
                        frame, pending = pending.split(b"\n", 1)
                        reply = self.answer_one(json.loads(frame))
                        if reply is not None:
                            connection.sendall(json.dumps(reply).encode() + b"\n")
        except (OSError, json.JSONDecodeError):
            return

    def answer_one(self, request):
        if not isinstance(request, dict):
            return None
        params = request.get("params") or {}
        pair = params.get("id")
        method = request.get("method")
        if method == "session.hello":
            result = {"protocol_version": params.get("protocol_version"),
                      "core_version": self.version}
        elif method == "pair.list":
            after = params.get("after")
            pairs = [record for record in PAIRS if after is None or record["id"] > after]
            result = {"pairs": pairs, "next": None}
        elif method == "pair.get":
            result = RECORDS.get(pair)
            if result is None:
                return self.reply(request, error=(PAIR_NOT_FOUND, "Pair not found"))
        elif method == "sync.run":
            if pair not in RECORDS:
                return self.reply(request, error=(PAIR_NOT_FOUND, "Pair not found"))
            result = self.job_status(pair, poll=False)
        elif method == "sync.status":
            if pair not in RECORDS:
                return self.reply(request, error=(PAIR_NOT_FOUND, "Pair not found"))
            result = self.job_status(pair, poll=True)
        else:
            return self.reply(request, error=(METHOD_NOT_FOUND, "Method not found"))
        return self.reply(request, result=result)

    def reply(self, request, result=None, error=None):
        if "id" not in request:
            return None
        response = {"jsonrpc": "2.0", "id": request["id"]}
        if error is None:
            response["result"] = result
        else:
            response["error"] = {"code": error[0], "message": error[1]}
        return response

    def job_status(self, pair, poll):
        if pair == PROGRESS_ID:
            if poll:
                with self.lock:
                    index = self.polls.get(pair, 0)
                    self.polls[pair] = index + 1
                status = PROGRESS_STAGES[min(index, len(PROGRESS_STAGES) - 1)]
            else:
                status = PROGRESS_STAGES[0]
        elif pair == INSTANT or self.release.is_set():
            status = FINISHED
        else:
            status = WORKING
        with self.lock:
            if status["state"] == "working":
                self.running.add(pair)
                self.peak = max(self.peak, len(self.running))
                if len(self.running) >= MAX_CONCURRENT:
                    self.saturated.set()
            else:
                self.running.discard(pair)
                self.completed.add(pair)
        return status


class Pipe:
    """Deadline-bounded line reader on the child's stdout via selectors."""

    def __init__(self, child):
        self.fd = child.stdout.fileno()
        self.buffer = bytearray()
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.fd, selectors.EVENT_READ)

    def line(self, deadline):
        while True:
            newline = self.buffer.find(b"\n")
            if newline >= 0:
                line = bytes(self.buffer[:newline])
                del self.buffer[:newline + 1]
                return line
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not self.selector.select(remaining):
                return None
            chunk = os.read(self.fd, 65536)
            if not chunk:
                return None
            self.buffer += chunk


def spawn(cli, endpoint, arguments):
    return subprocess.Popen([str(cli), *arguments, "--socket", str(endpoint)],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)


def reap(child, seconds=DRAIN_SECONDS):
    try:
        child.wait(timeout=seconds)
    except subprocess.TimeoutExpired:
        child.kill()
        child.wait(timeout=seconds)
    return child.returncode


def sync_all_streams(cli, fixture, endpoint):
    """`sync --all` must report pair 1 while the other jobs are still gated."""
    child = spawn(cli, endpoint, ("sync", "--all"))
    output = Pipe(child)
    try:
        first = output.line(time.monotonic() + EARLY_LINE_SECONDS)
        if first is None:
            raise RuntimeError(
                "sync --all produced no result within "
                f"{EARLY_LINE_SECONDS}s while later jobs were still gated")
        record = json.loads(first)
        if record.get("id") != INSTANT or record.get("status", {}).get("state") != "finished":
            raise RuntimeError(f"Unexpected first sync --all result: {first!r}")
        if not fixture.saturated.wait(EARLY_LINE_SECONDS):
            raise RuntimeError("sync --all never filled its concurrent job slots")
        if fixture.completed != {INSTANT}:
            raise RuntimeError(
                f"Other jobs finished before the gate was released: {fixture.completed}")
        fixture.release.set()
        seen = {INSTANT}
        deadline = time.monotonic() + DRAIN_SECONDS
        while len(seen) < len(PAIR_IDS):
            line = output.line(deadline)
            if line is None:
                raise RuntimeError(f"sync --all stopped after results {sorted(seen)}")
            record = json.loads(line)
            if record.get("status", {}).get("state") != "finished":
                raise RuntimeError(f"sync --all result was not a finished run: {line!r}")
            seen.add(record.get("id"))
        if reap(child) != 0:
            raise RuntimeError(
                f"sync --all failed: {child.stderr.read().strip()}")
        if output.line(deadline) is not None:
            raise RuntimeError("sync --all repeated a pair's result")
        if seen != set(PAIR_IDS):
            raise RuntimeError(f"sync --all missed pairs {sorted(set(PAIR_IDS) - seen)}")
        if fixture.peak > MAX_CONCURRENT:
            raise RuntimeError(f"sync --all ran {fixture.peak} pairs at once")
    finally:
        # Bounded cleanup: let gated jobs finish so the CLI can exit.
        fixture.release.set()
        reap(child)
        output.selector.close()
        child.stdout.close()
        child.stderr.close()


def progress_streams(cli, fixture, endpoint):
    """`sync ID --progress` must print each distinct Working status."""
    child = spawn(cli, endpoint, ("sync", PROGRESS_ID, "--progress"))
    output = Pipe(child)
    deadline = time.monotonic() + DRAIN_SECONDS
    lines = []
    try:
        while True:
            line = output.line(deadline)
            if line is None:
                break
            lines.append(json.loads(line))
        if reap(child) != 0:
            raise RuntimeError(
                f"sync --progress failed: {child.stderr.read().strip()}")
        finals = [line for line in lines if line.get("state") != "working"]
        if len(finals) != 1 or finals[0].get("state") != "finished" or lines[-1:] != finals:
            raise RuntimeError(f"Expected exactly one final finished status, got {lines}")
        working = [line for line in lines if line.get("state") == "working"]
        if working != PROGRESS_STAGES[:2]:
            raise RuntimeError(f"Expected each distinct Working status once, got {working}")
    finally:
        reap(child)
        output.selector.close()
        child.stdout.close()
        child.stderr.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("cli", type=Path)
    arguments = parser.parse_args()
    if sys.platform != "linux":
        raise RuntimeError("The controlled agent fixture is Linux-only")
    cli = arguments.cli.resolve()
    version = subprocess.run([str(cli), "--version"], capture_output=True, text=True,
                             timeout=EARLY_LINE_SECONDS, check=True).stdout.split()[-1]
    with tempfile.TemporaryDirectory(prefix="foldover-streaming-") as directory:
        os.chmod(directory, 0o700)
        endpoint = Path(directory) / "s"
        fixture = Fixture(endpoint, version)
        failures = []
        try:
            for check in (sync_all_streams, progress_streams):
                try:
                    check(cli, fixture, endpoint)
                except Exception as error:
                    failures.append(f"{check.__name__}: {error}")
        finally:
            fixture.release.set()
            fixture.close()
    if failures:
        raise RuntimeError("; ".join(failures))
    print("sync --all early results and sync --progress distinct lines passed")


if __name__ == "__main__":
    main()
