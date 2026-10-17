#!/usr/bin/env python3
"""Exercise the built CLI and a real agent over a private Unix socket."""

import argparse
from contextlib import closing
import json
import os
from pathlib import Path
import socket
import sqlite3
import stat
import subprocess
import sys
import tempfile
import time


STARTUP_SECONDS = 10
COMMAND_SECONDS = 10
RETRY_SECONDS = 0.05
METHOD_NOT_FOUND = -32601
PERMISSION_DENIED = -32002
PARSE_ERROR = -32700


def invoke(cli, endpoint):
    return subprocess.run([str(cli), "status", "--socket", str(endpoint)],
                          capture_output=True, text=True, timeout=COMMAND_SECONDS)


def start(agent, cli, endpoint, environment):
    child = subprocess.Popen([str(agent), "--socket", str(endpoint)],
                             env=environment, stdout=subprocess.DEVNULL,
                             stderr=subprocess.PIPE, text=True)
    try:
        deadline = time.monotonic() + STARTUP_SECONDS
        while time.monotonic() < deadline:
            status = invoke(cli, endpoint)
            if status.returncode == 0:
                if "health API only (sync unavailable)" not in status.stdout:
                    raise RuntimeError(f"Unexpected CLI status: {status.stdout}")
                return child
            if child.poll() is not None:
                raise RuntimeError(f"Agent exited before startup: {child.stderr.read()}")
            time.sleep(RETRY_SECONDS)
        raise RuntimeError("Agent did not become reachable before the startup deadline")
    except BaseException:
        if child.poll() is None:
            child.kill()
        child.wait(timeout=COMMAND_SECONDS)
        if child.stderr is not None:
            child.stderr.close()
        raise


def exchange(endpoint, frames):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(COMMAND_SECONDS)
        connection.connect(str(endpoint))
        connection.sendall(frames)
        response = bytearray()
        while not response.endswith(b"\n"):
            chunk = connection.recv(4096)
            if not chunk:
                raise RuntimeError("Agent closed before a complete reply")
            response.extend(chunk)
        return json.loads(response)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("agent", type=Path)
    parser.add_argument("cli", type=Path)
    arguments = parser.parse_args()
    agent, cli = arguments.agent.resolve(), arguments.cli.resolve()
    with tempfile.TemporaryDirectory(prefix="foldover-agent-") as directory:
        home = Path(directory)
        environment = dict(os.environ, HOME=str(home), XDG_DATA_HOME=str(home / "data"))
        state = (home / "Library/Application Support/Foldover" if sys.platform == "darwin"
                 else home / "data/foldover")
        endpoint = Path(directory) / "s"
        child = start(agent, cli, endpoint, environment)
        try:
            duplicate = subprocess.run([str(agent), "--socket", str(endpoint)],
                                       env=environment, capture_output=True,
                                       text=True, timeout=COMMAND_SECONDS)
            if duplicate.returncode == 0 or "runtime lock" not in duplicate.stderr:
                raise RuntimeError(f"Second agent was not rejected: {duplicate.stderr}")

            other = Path(directory) / "other" / "s"
            duplicate = subprocess.run([str(agent), "--socket", str(other)],
                                       env=environment, capture_output=True,
                                       text=True, timeout=COMMAND_SECONDS)
            if duplicate.returncode == 0 or "state writer lock" not in duplicate.stderr:
                raise RuntimeError(f"A second socket bypassed state ownership: {duplicate.stderr}")
            for path, mode in [(state, 0o700), (state / "state.db", 0o600),
                               (state / "writer.lock", 0o600)]:
                if stat.S_IMODE(path.stat().st_mode) != mode:
                    raise RuntimeError(f"Unsafe state permissions: {path}")

            unknown = exchange(endpoint, b'{"jsonrpc":"2.0","method":"pair.list","id":"private"}\n')
            expected = PERMISSION_DENIED if sys.platform == "darwin" else METHOD_NOT_FOUND
            if unknown.get("error", {}).get("code") != expected:
                raise RuntimeError(f"Unimplemented privileged route was not closed: {unknown}")
            malformed = exchange(endpoint, b'{\n')
            if malformed.get("error", {}).get("code") != PARSE_ERROR:
                raise RuntimeError(f"Malformed request was not rejected: {malformed}")
            notification = b'{"jsonrpc":"2.0","method":"daemon.status"}\n'
            request = b'{"jsonrpc":"2.0","method":"daemon.status","id":null}\n'
            response = exchange(endpoint, notification + request)
            if "result" not in response or "id" not in response or response["id"] is not None:
                raise RuntimeError(f"Notification/null ID behavior is wrong: {response}")

            child.kill()
            child.wait(timeout=COMMAND_SECONDS)
            if invoke(cli, endpoint).returncode == 0:
                raise RuntimeError("CLI reported an unavailable agent as connected")
            child.stderr.close()
            child = start(agent, cli, endpoint, environment)
            child.terminate()
            if child.wait(timeout=COMMAND_SECONDS) != 0:
                raise RuntimeError("Agent did not shut down cleanly on SIGTERM")

            database = state / "state.db"
            with closing(sqlite3.connect(database)) as connection:
                version = connection.execute("PRAGMA user_version").fetchone()[0]
                connection.execute(f"PRAGMA user_version={version + 1}")
                connection.commit()
            before = database.read_bytes()
            incompatible = subprocess.run([str(agent), "--socket", str(endpoint)],
                                          env=environment, capture_output=True,
                                          text=True, timeout=COMMAND_SECONDS)
            if incompatible.returncode == 0 or "schema reader" not in incompatible.stderr:
                raise RuntimeError(f"Incompatible state was not refused: {incompatible.stderr}")
            if database.read_bytes() != before:
                raise RuntimeError("Incompatible-state refusal changed the database")
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=COMMAND_SECONDS)
            if child.stderr is not None:
                child.stderr.close()
    print("Agent/CLI startup, state ownership, compatibility refusal, framing, crash restart and shutdown passed")


if __name__ == "__main__":
    main()
