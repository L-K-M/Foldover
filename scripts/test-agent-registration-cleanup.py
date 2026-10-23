#!/usr/bin/env python3
"""Portable cleanup regressions for the native registration fixture."""

import importlib.util
from contextlib import redirect_stderr
import io
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock


module_path = Path(__file__).resolve().parent / "test-agent-registration.py"
spec = importlib.util.spec_from_file_location("agent_registration", module_path)
registration = importlib.util.module_from_spec(spec)
spec.loader.exec_module(registration)


class CleanupTests(unittest.TestCase):
    def test_cleanup_failures_preserve_original_error_and_try_every_action(self):
        prefix = "ch.lkmc.foldover.tests.registration.cleanup"
        labels = [prefix, prefix + "." + "1" * 40, prefix + "." + "2" * 40]
        original = RuntimeError("The updated helper did not start")

        def failing_command(arguments, **kwargs):
            if arguments[-1] == "stop":
                raise subprocess.CalledProcessError(1, arguments)
            if arguments[1] == "bootout" and arguments[-1].endswith(labels[0]):
                raise subprocess.TimeoutExpired(arguments, 60)
            return subprocess.CompletedProcess(arguments, 0)

        with tempfile.TemporaryDirectory() as directory:
            app = Path(directory) / "Registration.app"
            app.mkdir()
            with mock.patch.object(registration, "command", side_effect=failing_command) as calls, \
                    redirect_stderr(io.StringIO()):
                with self.assertRaises(RuntimeError) as caught:
                    try:
                        raise original
                    finally:
                        registration.cleanup(app, prefix, prefix + ".plist", labels)

                self.assertIs(caught.exception, original)
                expected = [[str(app / "Contents/MacOS/Registration"), prefix, prefix + ".plist", "stop"]]
                for label in labels:
                    target = f"gui/{registration.os.getuid()}/{label}"
                    expected += [["/bin/launchctl", "bootout", target], ["/bin/launchctl", "enable", target]]
                self.assertEqual([call.args[0] for call in calls.call_args_list], expected)
                self.assertTrue(all(call.kwargs == {"check": False} for call in calls.call_args_list))

    def test_os_errors_do_not_skip_later_labels(self):
        labels = ["first", "second"]
        with tempfile.TemporaryDirectory() as directory:
            app = Path(directory) / "absent.app"
            with mock.patch.object(registration, "command", side_effect=OSError("cannot execute")) as calls, \
                    redirect_stderr(io.StringIO()):
                registration.cleanup(app, "fixture", "fixture.plist", labels)
            self.assertEqual(calls.call_count, 4)


class CodeHashTests(unittest.TestCase):
    def test_missing_hash_reports_the_signing_problem(self):
        for signing in ["no signing information", "CDHash=SHORT\n"]:
            with self.subTest(signing=signing):
                with self.assertRaisesRegex(RuntimeError, "fixture helper has no valid CDHash"):
                    registration.fixture_code_hash(signing)

    def test_valid_hash_is_read_without_changes(self):
        code_hash = "0123456789abcdef0123456789abcdef01234567"
        self.assertEqual(registration.fixture_code_hash("CDHash=" + code_hash + "\n"), code_hash)


if __name__ == "__main__":
    unittest.main()
