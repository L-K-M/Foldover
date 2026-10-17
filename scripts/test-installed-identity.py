#!/usr/bin/env python3
"""Exercise installed-code IPC authentication on macOS with signed process fixtures."""

from contextlib import contextmanager
import json
import os
from pathlib import Path
import plistlib
import selectors
import shutil
import socket
import subprocess
import sys
import tempfile


COMMAND_SECONDS = 20
BUILD_SECONDS = 180
PERMISSION_DENIED = -32002
BUNDLE_ID = "ch.lkmc.foldover"
PROJECT = Path(__file__).resolve().parent.parent


def command(arguments, **options):
    return subprocess.run(arguments, check=True, capture_output=True, text=True,
                          timeout=COMMAND_SECONDS, **options)


def build(variant):
    environment = dict(os.environ, FOLDOVER_IDENTITY_FIXTURE_VARIANT=variant)
    result = subprocess.run(["cargo", "build", "--locked", "-p", "foldover-ipc", "--example",
                             "identity_fixture", "--message-format=json"], cwd=PROJECT,
                            env=environment, capture_output=True, text=True,
                            timeout=BUILD_SECONDS, check=True)
    for line in result.stdout.splitlines():
        item = json.loads(line)
        if item.get("reason") == "compiler-artifact" and item.get("executable"):
            if item["target"]["name"] == "identity_fixture":
                return Path(item["executable"])
    raise RuntimeError("Cargo did not report the identity fixture")


def sign(path, identifier, entitlements=None):
    arguments = ["/usr/bin/codesign", "--force", "--sign", "-", "--timestamp=none",
                 "--options", "runtime", "--identifier", identifier]
    if entitlements is not None:
        arguments += ["--entitlements", str(entitlements)]
    command(arguments + [str(path)])


def exchange(endpoint, method):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(COMMAND_SECONDS)
        connection.connect(str(endpoint))
        connection.sendall((json.dumps({"jsonrpc": "2.0", "id": 1, "method": method}) + "\n").encode())
        with connection.makefile("rb") as stream:
            return json.loads(stream.readline())


def invoke(executable, mode, endpoint):
    return json.loads(command([str(executable), mode, str(endpoint)]).stdout)


def require_allowed(response):
    if response.get("result", {}).get("marker") != "trusted":
        raise RuntimeError(f"Trusted code was not authenticated: {response}")


def require_denied(response):
    if response.get("error", {}).get("code") != PERMISSION_DENIED:
        raise RuntimeError(f"Untrusted code reached a private handler: {response}")


@contextmanager
def server(executable, endpoint, mode="serve"):
    child = subprocess.Popen([str(executable), mode, str(endpoint)], stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, text=True)
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(child.stdout, selectors.EVENT_READ)
            if not selector.select(COMMAND_SECONDS):
                raise RuntimeError("Identity fixture did not become ready")
        if child.stdout.readline().strip() != "ready":
            raise RuntimeError(f"Identity fixture startup failed: {child.stderr.read()}")
        yield child
    finally:
        failed = sys.exc_info()[0] is not None
        if child.poll() is None:
            child.kill()
        child.wait(timeout=COMMAND_SECONDS)
        if failed and not child.stderr.closed:
            print(child.stderr.read(), file=sys.stderr)
        child.stdout.close()
        child.stderr.close()


def main():
    if sys.platform != "darwin":
        raise RuntimeError("Installed-code fixtures require macOS")
    with tempfile.TemporaryDirectory(prefix="foldover-auth-") as directory:
        root = Path(directory)
        trusted = root / "trusted"
        shutil.copy2(build("trusted"), trusted)
        changed = root / "changed"
        shutil.copy2(build("different-code"), changed)
        bundle = root / "Foldover.app"
        main = bundle / "Contents/MacOS/Foldover"
        cli = bundle / "Contents/Library/LaunchServices/foldover"
        agent = bundle / "Contents/Library/LaunchServices/foldoverd"
        for path, identifier in [(main, BUNDLE_ID), (cli, BUNDLE_ID + ".cli"),
                                 (agent, BUNDLE_ID + ".agent")]:
            path.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(trusted, path)
            sign(path, identifier)
        with (bundle / "Contents/Info.plist").open("wb") as stream:
            plistlib.dump({"CFBundleIdentifier": BUNDLE_ID, "CFBundleExecutable": "Foldover",
                          "CFBundlePackageType": "APPL", "CFBundleVersion": "1"}, stream)
        sign(bundle, BUNDLE_ID)
        command(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(bundle)])
        lookalike = root / "lookalike"
        shutil.copy2(changed, lookalike)
        sign(lookalike, BUNDLE_ID + ".cli")
        relocated = root / "relocated-cli"
        shutil.copy2(cli, relocated)
        endpoint = root / "runtime/s"

        with server(agent, endpoint):
            if exchange(endpoint, "daemon.status").get("result") != "public":
                raise RuntimeError("Public health access requires code identity")
            require_allowed(invoke(main, "call", endpoint))
            require_allowed(invoke(cli, "call", endpoint))
            require_allowed(invoke(relocated, "raw", endpoint))
            require_denied(invoke(lookalike, "raw", endpoint))
            require_denied(exchange(endpoint, "fixture.private"))

            # A valid re-signed replacement must not change a running agent's trust.
            shutil.copy2(changed, cli)
            sign(cli, BUNDLE_ID + ".cli")
            sign(bundle, BUNDLE_ID)
            command(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(bundle)])
            require_denied(invoke(relocated, "raw", endpoint))
            response = invoke(cli, "call", endpoint)
            if "error" not in response:
                raise RuntimeError(f"A bundle change silently re-enrolled code: {response}")

        with server(agent, endpoint):
            response = invoke(cli, "call", endpoint)
            if response.get("result", {}).get("marker") != "trusted":
                raise RuntimeError(f"Restart did not use the verified current bundle: {response}")
            require_denied(invoke(relocated, "raw", endpoint))

        fake = root / "fake-agent"
        shutil.copy2(changed, fake)
        sign(fake, BUNDLE_ID + ".agent")
        with server(fake, endpoint, "capture") as child:
            response = invoke(main, "call", endpoint)
            if "error" not in response:
                raise RuntimeError(f"A fake server was trusted: {response}")
            stdout, stderr = child.communicate(timeout=COMMAND_SECONDS)
            if child.returncode != 0 or json.loads(stdout).get("bytes_received") != 0:
                raise RuntimeError(f"Private payload reached a fake server: {stdout} {stderr}")

        entitlements = root / "debug.plist"
        for forbidden in ["com.apple.security.get-task-allow",
                          "com.apple.security.cs.disable-library-validation"]:
            with entitlements.open("wb") as stream:
                plistlib.dump({forbidden: True}, stream)
            sign(cli, BUNDLE_ID + ".cli", entitlements)
            sign(bundle, BUNDLE_ID)
            with server(agent, endpoint):
                require_denied(invoke(main, "raw", endpoint))
                if exchange(endpoint, "daemon.status").get("result") != "public":
                    raise RuntimeError("Rejected installation lost public health access")

        with entitlements.open("wb") as stream:
            plistlib.dump({}, stream)
        command(["/usr/bin/codesign", "--force", "--sign", "-", "--timestamp=none",
                 "--options", "0", "--entitlements", str(entitlements),
                 "--identifier", BUNDLE_ID + ".cli", str(cli)])
        sign(bundle, BUNDLE_ID)
        with server(agent, endpoint):
            require_denied(invoke(main, "raw", endpoint))

        print("Installed app/CLI, relocated identity, lookalike/interpreter rejection, "
              "update pinning, restart rotation, debug/runtime policy and fake-server "
              "payload protection passed")


if __name__ == "__main__":
    main()
