#!/usr/bin/env python3
"""Exercise real SMAppService updates in an isolated, ad-hoc-signed app."""

import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
import time
import uuid


PROJECT = Path(__file__).resolve().parent.parent


def command(arguments, *, check=True):
    return subprocess.run(arguments, capture_output=True, text=True, check=check, timeout=60)


def sign(path, identifier=None):
    arguments = ["codesign", "--force", "--sign", "-", "--timestamp=none", "--options", "runtime"]
    if identifier:
        arguments += ["--identifier", identifier]
    command(arguments + [str(path)])


def run():
    if sys.platform != "darwin":
        raise RuntimeError("The registration lifecycle check requires macOS")
    prefix = "ch.lkmc.foldover.tests.registration." + uuid.uuid4().hex
    domain = f"gui/{os.getuid()}"
    labels = []
    with tempfile.TemporaryDirectory(prefix="foldover-registration-") as directory:
        root = Path(directory)
        app = root / "Registration.app"
        plist_name = prefix + ".plist"
        wrapper = root / "Main.swift"
        wrapper.write_text('''
import Foundation
import ServiceManagement

@main struct RegistrationFixture {
    @MainActor static func main() async throws {
        let args = CommandLine.arguments
        let agent = BackgroundAgent(plistName: args[2], labelPrefix: args[1])
        let before = await agent.registrationForUpdate()
        switch args[3] {
        case "start": _ = try await agent.register()
        case "update":
            if before == .enabled { _ = try await agent.reregister() }
        case "stop":
            await withCheckedContinuation { (done: CheckedContinuation<Void, Never>) in
                SMAppService.agent(plistName: args[2]).unregister { _ in done.resume() }
            }
        default: break
        }
        print(before == .enabled ? "enabled" : "off")
    }
}
''')
        runner = root / "Registration"
        source = PROJECT / "apple/FoldoverSystem/Sources/FoldoverSystem/BackgroundAgent.swift"
        command(["swiftc", "-parse-as-library", "-swift-version", "6", "-warnings-as-errors",
                 "-module-cache-path", str(root / "ModuleCache"), str(source), str(wrapper), "-o", str(runner)])

        def invoke(action):
            return command([str(app / "Contents/MacOS/Registration"), prefix, plist_name, action]).stdout.strip()

        def job(label):
            return command(["/bin/launchctl", "print", f"{domain}/{label}"], check=False)

        def build(version, legacy=False):
            incoming = root / "incoming.app"
            for relative in ("MacOS", "Library/LaunchServices", "Library/LaunchAgents"):
                (incoming / "Contents" / relative).mkdir(parents=True)
            shutil.copy2(runner, incoming / "Contents/MacOS/Registration")
            helper_source = root / "helper.c"
            helper_source.write_text(
                '#include <unistd.h>\n'
                f'int main(void) {{ sleep(300); return {version}; }}\n'
            )
            helper = incoming / "Contents/Library/LaunchServices/helper"
            command(["clang", str(helper_source), "-o", str(helper)])
            sign(helper, prefix + ".helper")
            signing = command(["codesign", "--display", "--verbose=4", str(helper)]).stderr
            code_hash = re.search(r"^CDHash=([0-9a-f]{40})$", signing, re.MULTILINE)[1]
            label = prefix if legacy else prefix + "." + code_hash
            labels.append(label)
            info = {"CFBundleIdentifier": prefix, "CFBundleExecutable": "Registration",
                    "CFBundleVersion": "1.0", "CFBundlePackageType": "APPL"}
            (incoming / "Contents/Info.plist").write_bytes(plistlib.dumps(info))
            definition = {"Label": label, "BundleProgram": "Contents/Library/LaunchServices/helper",
                          "RunAtLoad": True}
            (incoming / "Contents/Library/LaunchAgents" / plist_name).write_bytes(plistlib.dumps(definition))
            sign(incoming)
            command(["codesign", "--verify", "--deep", "--strict", str(incoming)])
            if app.exists():
                outgoing = root / "outgoing.app"
                app.rename(outgoing)
                incoming.rename(app)
                shutil.rmtree(outgoing)
            else:
                incoming.rename(app)
            return label

        def expect_running(label):
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if "state = running" in job(label).stdout:
                    return
                time.sleep(0.05)
            raise RuntimeError(f"Updated agent did not start: {job(label).stdout}")

        try:
            first = build(1, legacy=True)
            invoke("start")
            expect_running(first)
            second = build(2)
            migration = invoke("update")
            assert migration == "enabled", "New label did not find the previous enabled job"
            expect_running(second)
            assert job(first).returncode != 0, "Legacy job survived migration"

            # A Login Items disable must never authorize the next build.
            command(["/bin/launchctl", "disable", f"{domain}/{second}"])
            # An unrelated stale enabled build must not override that choice.
            stale = prefix + "." + "0" * 40
            labels.append(stale)
            stale_plist = root / "stale.plist"
            stale_plist.write_bytes(plistlib.dumps({
                "Label": stale, "ProgramArguments": ["/bin/sleep", "300"], "RunAtLoad": True,
            }))
            command(["/bin/launchctl", "enable", f"{domain}/{stale}"])
            command(["/bin/launchctl", "bootstrap", domain, str(stale_plist)])
            third = build(3)
            assert invoke("update") == "off", "Update overrode the disabled job"
            assert job(third).returncode != 0, "Disabled migration started the new job"

            command(["/bin/launchctl", "bootout", f"{domain}/{stale}"])
            command(["/bin/launchctl", "enable", f"{domain}/{second}"])
            assert invoke("update") == "enabled"
            expect_running(third)
            assert job(second).returncode != 0, "Previous build's job survived migration"
            print(json.dumps({"legacy_migration": "passed", "second_update": "passed",
                              "disabled_job_preserved": "passed", "old_jobs_removed": "passed"}))
        finally:
            if app.exists():
                invoke("stop")
            for label in labels:
                command(["/bin/launchctl", "bootout", f"{domain}/{label}"], check=False)
                command(["/bin/launchctl", "enable", f"{domain}/{label}"], check=False)


if __name__ == "__main__":
    run()
