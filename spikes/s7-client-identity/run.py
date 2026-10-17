#!/usr/bin/env python3
"""Build and exercise the ad-hoc client-identity spike on macOS 27."""

import argparse
import json
import os
from pathlib import Path
import platform
import select
import shutil
import socket
import stat
import subprocess
import sys
import tempfile


IDENTIFIER = "ch.lkmc.foldover.spike.s7.client"
COMMAND_TIMEOUT_SECONDS = 90
STARTUP_TIMEOUT_SECONDS = 15


def run(*arguments):
    result = subprocess.run([str(value) for value in arguments], check=False,
                            capture_output=True, timeout=COMMAND_TIMEOUT_SECONDS)
    output = (result.stdout + result.stderr).decode(errors="replace")
    if result.returncode:
        raise RuntimeError(f"{arguments[0]} exited {result.returncode}: {output}")
    return output


def signing(path):
    return dict(line.split("=", 1) for line in run(
        "codesign", "-d", "--verbose=4", path,
    ).splitlines() if "=" in line)


def raw_client(path, payload):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(STARTUP_TIMEOUT_SECONDS)
        connection.connect(str(path))
        connection.sendall(payload + b"\n")
        reply = bytearray()
        while not reply.endswith(b"\n"):
            chunk = connection.recv(1_024)
            if not chunk:
                raise RuntimeError("Server closed before replying")
            reply.extend(chunk)
        return json.loads(reply)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    arguments = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("This probe requires macOS 27 and Apple silicon")
    source = Path(__file__).resolve().parent
    report = {"system": platform.platform(), "machine": platform.machine(), "cases": [],
              "limitations": ["No TCC permission or Full Disk Access interaction is exercised",
                              "Installed app updates and SMAppService registration are not exercised",
                              "A different-user connection is not exercised",
                              "This is a throwaway authorization probe, not production IPC"]}
    with tempfile.TemporaryDirectory(prefix="f7.") as directory:
        directory = Path(directory)
        server, trusted, lookalike = [directory / name for name in ("server", "trusted", "lookalike")]
        common = ["xcrun", "swiftc", "-swift-version", "6", "-strict-concurrency=complete",
                  "-warnings-as-errors", "-parse-as-library", "-target", "arm64-apple-macos27.0",
                  source / "Socket.swift"]
        run(*common, source / "Server.swift", "-o", server)
        run(*common, source / "Client.swift", "-o", trusted)
        run(*common, "-D", "LOOKALIKE", source / "Client.swift", "-o", lookalike)
        for path in (trusted, lookalike):
            run("codesign", "--force", "--sign", "-", "--identifier", IDENTIFIER, path)
            run("codesign", "--verify", "--strict", path)
        report["signing"] = {"trusted": signing(trusted), "lookalike": signing(lookalike)}
        if report["signing"]["trusted"].get("Identifier") != IDENTIFIER or \
                report["signing"]["lookalike"].get("Identifier") != IDENTIFIER:
            raise RuntimeError("Both fixtures must claim the same identifier")
        if report["signing"]["trusted"].get("CDHash") == report["signing"]["lookalike"].get("CDHash"):
            raise RuntimeError("The lookalike must have a different code identity")

        relocated = directory / "relocated"
        shutil.copy2(trusted, relocated)
        endpoint = directory / "s"
        scenarios = [
            ("trusted-privileged", trusted, "preview.list", "permitted"),
            ("same-identifier-lookalike", lookalike, "preview.list", "denied"),
            ("interpreter-privileged", None, "preview.list", "denied"),
            ("lookalike-public-status", lookalike, "daemon.status", "permitted"),
            ("same-code-relocated", relocated, "preview.list", "permitted"),
            ("unknown-method", trusted, "not-a-method", "denied"),
            ("malformed-request", None, None, "denied"),
        ]
        process = subprocess.Popen([str(server), str(endpoint), str(trusted), str(len(scenarios))],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            ready, _, _ = select.select([process.stdout], [], [], STARTUP_TIMEOUT_SECONDS)
            if not ready or json.loads(process.stdout.readline()) != {"ready": True}:
                raise RuntimeError("The identity server did not become ready")
            report["socket"] = {"path_utf8_bytes": len(os.fsencode(endpoint)),
                                "socket_mode": oct(stat.S_IMODE(endpoint.stat().st_mode)),
                                "parent_mode": oct(stat.S_IMODE(directory.stat().st_mode))}
            for name, executable, method, expected in scenarios:
                if executable is None:
                    payload = b"{" if method is None else json.dumps({"method": method}).encode()
                    reply = raw_client(endpoint, payload)
                else:
                    reply = json.loads(run(executable, endpoint, method))
                report["cases"].append({"name": name, "expected": expected, "reply": reply,
                                        "passed": reply["access"] == expected})
            output, errors = process.communicate(timeout=STARTUP_TIMEOUT_SECONDS)
            report["server_observations"] = [json.loads(line) for line in output.splitlines()]
            report["server_stderr"] = errors
            if process.returncode:
                raise RuntimeError(f"Server exited {process.returncode}: {errors}")
        except Exception:
            if process.poll() is None:
                process.kill()
            output, errors = process.communicate(timeout=STARTUP_TIMEOUT_SECONDS)
            sys.stderr.write(f"Server observations before harness failure:\n{output}\n{errors}\n")
            raise
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate(timeout=STARTUP_TIMEOUT_SECONDS)

    report["authorization_checks_passed"] = all(case["passed"] for case in report["cases"]) and \
        report["socket"]["socket_mode"] == "0o600" and report["socket"]["parent_mode"] == "0o700"
    report["full_s7_passed"] = False
    arguments.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"authorization_checks_passed": report["authorization_checks_passed"],
                      "full_s7_passed": report["full_s7_passed"]}, indent=2))


if __name__ == "__main__":
    main()
