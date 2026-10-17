#!/usr/bin/env python3
"""Exercise the built CLI and a real agent over a private Unix socket."""

import argparse
from contextlib import closing
import json
import os
from pathlib import Path
import plistlib
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
PROTOCOL_VERSION = 2
VERSION_MISMATCH = -32001
ROOT_OVERLAP = -32006
STALE_REVISION = -32004
FORBIDDEN_ENTITLEMENTS = (
    "com.apple.security.get-task-allow",
    "com.apple.security.cs.debugger",
    "com.apple.security.cs.allow-jit",
    "com.apple.security.cs.allow-unsigned-executable-memory",
    "com.apple.security.cs.allow-dyld-environment-variables",
    "com.apple.security.cs.disable-library-validation",
    "com.apple.security.cs.disable-executable-page-protection",
)


def invoke(cli, endpoint, arguments=("status",)):
    return subprocess.run([str(cli), *arguments, "--socket", str(endpoint)],
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
                if "pair configuration available (sync unavailable)" not in status.stdout:
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


def private_exchange(endpoint, method, params=None):
    frames = [{"jsonrpc": "2.0", "method": "session.hello", "id": "hello",
               "params": {"protocol_version": PROTOCOL_VERSION}},
              {"jsonrpc": "2.0", "method": method, "id": "private"}]
    if params is not None:
        frames[1]["params"] = params
    response = exchange(endpoint, json.dumps(frames).encode() + b"\n")
    return next(reply for reply in response if reply.get("id") == "private")


def configuration(cli, endpoint, directory, policy):
    listing = invoke(cli, endpoint, ("pairs",))
    if policy == "denied":
        if listing.returncode == 0 or "Installed peer identity is unavailable or no longer valid" not in listing.stderr:
            raise RuntimeError(f"Development client accessed private configuration: {listing}")
        return None
    if listing.returncode != 0 or listing.stdout.strip():
        raise RuntimeError(f"Initial catalog is not empty: {listing}")
    left, right = directory / "left", directory / "right"
    left.mkdir()
    right.mkdir()
    # Include undecodable bytes: neither the CLI nor the wire may replace them.
    raw = os.path.join(os.fsencode(left), b"raw-\xff")
    os.mkdir(raw)
    arguments = ("pairs", "add", "Photos", raw, str(right), "backup-left")
    created = invoke(cli, endpoint, arguments)
    if created.returncode != 0:
        raise RuntimeError(f"Pair creation failed: {created.stderr}")
    pair = json.loads(created.stdout)
    if pair["configuration"]["left_root"] != list(raw):
        raise RuntimeError("Pair creation changed raw root bytes")
    duplicate = invoke(cli, endpoint, arguments)
    if duplicate.returncode == 0 or str(ROOT_OVERLAP) not in duplicate.stderr:
        raise RuntimeError(f"Overlapping pair was not rejected: {duplicate}")
    update = ("pairs", "update", pair["id"], str(pair["revision"]), "Renamed",
              raw, str(right), "mirror-right")
    saved = invoke(cli, endpoint, update)
    if saved.returncode != 0 or json.loads(saved.stdout)["revision"] != pair["revision"] + 1:
        raise RuntimeError(f"Pair update failed: {saved}")
    stale = invoke(cli, endpoint, update)
    if stale.returncode == 0 or str(STALE_REVISION) not in stale.stderr:
        raise RuntimeError(f"Stale update was not rejected: {stale}")
    return json.loads(saved.stdout)


def verify_release_entitlements(agent, cli):
    bundle = agent.parents[3]
    if bundle.suffix != ".app" or cli.parents[3] != bundle:
        raise RuntimeError("Private macOS smoke requires helpers in one signed app bundle")
    for path in (bundle, agent, cli):
        signed = subprocess.run(["/usr/bin/codesign", "--display", "--xml", "--entitlements", "-", str(path)],
                                capture_output=True, timeout=COMMAND_SECONDS, check=True)
        entitlements = plistlib.loads(signed.stdout) if signed.stdout.strip() else {}
        if not isinstance(entitlements, dict):
            raise RuntimeError(f"Invalid signed entitlements: {path}")
        forbidden = sorted(set(entitlements).intersection(FORBIDDEN_ENTITLEMENTS))
        if forbidden:
            raise RuntimeError(f"Release code contains forbidden entitlements: {path}: {forbidden}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("agent", type=Path)
    parser.add_argument("cli", type=Path)
    parser.add_argument("--configuration", choices=("required", "denied"), default="required")
    arguments = parser.parse_args()
    agent, cli = arguments.agent.resolve(), arguments.cli.resolve()
    if sys.platform == "darwin" and arguments.configuration == "required":
        verify_release_entitlements(agent, cli)
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

            unknown = private_exchange(endpoint, "not.implemented")
            expected = PERMISSION_DENIED if sys.platform == "darwin" else METHOD_NOT_FOUND
            if unknown.get("error", {}).get("code") != expected:
                raise RuntimeError(f"Unimplemented privileged route was not closed: {unknown}")
            unnegotiated = exchange(endpoint, b'{"jsonrpc":"2.0","method":"pair.list","id":1}\n')
            expected = PERMISSION_DENIED if sys.platform == "darwin" else VERSION_MISMATCH
            if unnegotiated.get("error", {}).get("code") != expected:
                raise RuntimeError(f"Private route accepted an unnegotiated session: {unnegotiated}")
            pair = configuration(cli, endpoint, home, arguments.configuration)
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
            if pair is not None:
                saved = invoke(cli, endpoint, ("pairs", "show", pair["id"]))
                if saved.returncode != 0 or json.loads(saved.stdout) != pair:
                    raise RuntimeError(f"Configuration did not survive agent crash: {saved}")
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
    print(f"Agent/CLI lifecycle, state, framing and configuration ({arguments.configuration}) passed")


if __name__ == "__main__":
    main()
