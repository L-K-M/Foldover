#!/usr/bin/env python3
"""Focused checks for qualification harness failure reporting and raw retention."""

import importlib.util
import json
import os
import pathlib
import signal
import threading
import time
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest import mock

PROJECT = Path(__file__).resolve().parent.parent
HARNESS = PROJECT / "perf" / "run-qualification.py"


def load_harness():
    spec = importlib.util.spec_from_file_location("run_qualification", HARNESS)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def write_runner(directory, body):
    runner = directory / "runner.sh"
    runner.write_text("#!/bin/sh\n" + body, encoding="utf-8")
    runner.chmod(0o755)
    return runner


def process_line():
    return "printf 'process\\t%s\\n' \"$$\"\n"


class FakeStdout:
    def __init__(self, lines, close_error=None):
        self.lines = lines
        self.close_error = close_error
        self.closed = False

    def __iter__(self):
        return iter(self.lines)

    def close(self):
        self.closed = True
        if self.close_error is not None:
            raise self.close_error


class FakeChild:
    def __init__(self, stdout, wait_results, poll_result=None, pid=4242):
        self.stdout = stdout
        self.pid = pid
        self._wait_results = list(wait_results)
        self._poll_result = poll_result
        self.wait_calls = []

    def wait(self, timeout=None):
        self.wait_calls.append(timeout)
        result = self._wait_results.pop(0)
        if isinstance(result, BaseException):
            raise result
        return result

    def poll(self):
        return self._poll_result


class ThreadSpy:
    """Record the harness's Event and Thread objects without global patches."""

    def __init__(self):
        self.events = []
        self.threads = []

    def Event(self):
        event = threading.Event()
        self.events.append(event)
        return event

    def Thread(self, target, args=(), kwargs=None):
        thread = threading.Thread(target=target, args=args, kwargs=kwargs or {})
        self.threads.append(thread)
        return thread


class HarnessTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.harness = load_harness()

    def run_harness(self, runner, name, revision="test-revision", output=None, cwd=None, env=None):
        output = output if output is not None else (self.root / f"out-{name}")
        arguments = [sys.executable, str(HARNESS), "--runner", str(runner),
                     "--state", str(self.root / "state"), "--left", str(self.root / "left"),
                     "--right", str(self.root / "right"), "--iterations", "1",
                     "--label", name, "--output", str(output)]
        if revision is not None:
            arguments += ["--revision", revision]
        result = subprocess.run(arguments, capture_output=True, text=True, check=False,
                                cwd=cwd, env=env, timeout=60)
        return result, output

    def summary(self, output):
        return json.loads((output / "summary.json").read_text(encoding="utf-8"))

    def run_fake_child(self, lines, *, killpg_side_effect=None, close_error=None,
                       parse_failure=False, wait_results=(0, 0), poll_result=None):
        """Run the harness in-process with a fake child, capturing cleanup state."""
        output = self.root / "out-cleanup"
        runner = write_runner(self.root, "")
        fake_stdout = FakeStdout(lines, close_error=close_error)
        child = FakeChild(fake_stdout, wait_results, poll_result)
        spy = ThreadSpy()
        fake_subprocess = types.SimpleNamespace(
            Popen=mock.Mock(return_value=child),
            TimeoutExpired=subprocess.TimeoutExpired,
            run=subprocess.run,
            DEVNULL=subprocess.DEVNULL,
            PIPE=subprocess.PIPE,
            CalledProcessError=subprocess.CalledProcessError,
            check_output=subprocess.check_output,
        )
        arguments = [str(HARNESS), "--runner", str(runner), "--state", "state",
                     "--left", "left", "--right", "right", "--iterations", "1",
                     "--label", "cleanup", "--output", str(output),
                     "--revision", "test-revision"]
        error = None
        try:
            with mock.patch.object(sys, "argv", arguments), \
                    mock.patch.object(self.harness, "subprocess", fake_subprocess), \
                    mock.patch.object(self.harness, "threading", spy), \
                    mock.patch.object(self.harness.os, "killpg",
                                      side_effect=killpg_side_effect):
                if parse_failure:
                    original = self.harness.parse_record
                    def fail_metric(line, line_number):
                        if line.startswith("metric"):
                            raise RuntimeError("injected parser failure")
                        return original(line, line_number)
                    with mock.patch.object(self.harness, "parse_record",
                                          side_effect=fail_metric):
                        self.harness.main()
                else:
                    self.harness.main()
        except BaseException as caught:
            error = caught
        finally:
            for event in spy.events:
                event.set()
            for thread in spy.threads:
                if thread.is_alive():
                    thread.join(timeout=5)
        summary_path = output / "summary.json"
        summary = None
        if summary_path.exists():
            summary = json.loads(summary_path.read_text(encoding="utf-8"))
        return error, summary, spy, child

    def test_valid_phase_and_metric_records_succeed(self):
        runner = write_runner(self.root, process_line() +
                              "printf 'metric\\t0\\tInputLoad\\t1234\\n'\n"
                              "sleep 0.3\n"
                              "printf 'phase\\t0\\tOpenEndpoints\\t5678\\n'\n"
                              "printf 'metric\\t0\\tTotal\\t9012\\n'\n"
                              "sleep 0.3\n")
        result, output = self.run_harness(runner, "valid")
        self.assertEqual(result.returncode, 0, result.stderr)
        summary = self.summary(output)
        self.assertEqual(summary["parsing_errors"], [])
        self.assertEqual(summary["sampling_errors"], [])
        self.assertEqual(summary["returncode"], 0)
        self.assertEqual([m["event"] for m in summary["measurements"]],
                         ["metric", "phase", "metric"])
        self.assertEqual([m["ns"] for m in summary["measurements"]], [1234, 5678, 9012])
        self.assertTrue(all("rss_kib" in m for m in summary["measurements"]))
        self.assertEqual(summary["revision"], "test-revision")
        self.assertTrue((output / "samples.jsonl").exists())
        raw = (output / "runner.tsv").read_text(encoding="utf-8")
        self.assertIn("phase\t0\tOpenEndpoints\t5678", raw)

    def test_malformed_known_record_fails_with_raw_trace_and_clear_error(self):
        runner = write_runner(self.root, process_line() +
                              "printf 'phase\\t0\\tOpenEndpoints\\tnot-a-number\\n'\n"
                              "printf 'metric\\t0\\tTotal\\t9012\\n'\n"
                              "sleep 0.3\n")
        result, output = self.run_harness(runner, "malformed")
        self.assertNotEqual(result.returncode, 0)
        summary = self.summary(output)
        self.assertEqual(summary["returncode"], 0)
        self.assertEqual(len(summary["parsing_errors"]), 1)
        self.assertIn("phase", summary["parsing_errors"][0])
        self.assertIn("not-a-number", summary["parsing_errors"][0])
        raw = (output / "runner.tsv").read_text(encoding="utf-8")
        self.assertIn("phase\t0\tOpenEndpoints\tnot-a-number", raw)
        self.assertEqual([m["ns"] for m in summary["measurements"]], [9012])

    def test_malformed_records_are_rejected(self):
        cases = {
            "negative-ns": "printf 'metric\\t0\\tTotal\\t-5\\n'",
            "short-phase": "printf 'phase\\t0\\tOpenEndpoints\\n'",
            "empty-name": "printf 'metric\\t0\\t\\t5\\n'",
            "bad-iteration": "printf 'phase\\tx\\tOpenEndpoints\\t5\\n'",
            "bad-pid": "printf 'process\\tabc\\n'",
            "zero-pid": "printf 'process\\t0\\n'",
            "short-process": "printf 'process\\n'",
        }
        for name, line in cases.items():
            with self.subTest(name=name):
                runner = write_runner(self.root, process_line() + line +
                                      "\nprintf 'metric\\t0\\tTotal\\t1\\n'\nsleep 0.2\n")
                result, output = self.run_harness(runner, f"reject-{name}")
                self.assertNotEqual(result.returncode, 0)
                summary = self.summary(output)
                self.assertTrue(summary["parsing_errors"], name)

    def test_duplicate_process_record_starts_one_sampler(self):
        duplicates = (process_line(), "printf 'process\\t999999\\n'")
        for index, duplicate in enumerate(duplicates):
            with self.subTest(duplicate=duplicate):
                runner = write_runner(self.root, process_line() + duplicate +
                                      "\nprintf 'metric\\t0\\tTotal\\t1\\n'\nsleep 0.3\n")
                result, output = self.run_harness(runner, f"duplicate-{index}")
                self.assertNotEqual(result.returncode, 0)
                summary = self.summary(output)
                self.assertTrue(any("duplicate process record" in error
                                    for error in summary["parsing_errors"]))
                self.assertTrue((output / "samples.jsonl").exists())
                self.assertFalse(any("samples.jsonl" in error
                                     for error in summary["sampling_errors"]))

    def test_runner_nonzero_exit_retains_summary(self):
        runner = write_runner(self.root, process_line() +
                              "printf 'metric\\t0\\tTotal\\t1\\n'\n"
                              "sleep 0.2\n"
                              "exit 7\n")
        result, output = self.run_harness(runner, "nonzero")
        self.assertEqual(result.returncode, 7)
        summary = self.summary(output)
        self.assertEqual(summary["returncode"], 7)
        self.assertEqual(summary["parsing_errors"], [])
        self.assertEqual(len(summary["measurements"]), 1)
        self.assertTrue((output / "resources.txt").exists())

    def test_missing_runner_still_writes_summary(self):
        result, output = self.run_harness(self.root / "missing-runner.sh", "missing")
        self.assertNotEqual(result.returncode, 0)
        summary = self.summary(output)
        self.assertIn("runner unreadable", summary["error"])
        self.assertEqual(summary["revision"], "test-revision")

    def test_launch_failure_writes_summary(self):
        runner = write_runner(self.root, "printf 'metric\\t0\\tTotal\\t1\\n'\n")
        output = self.root / "out-launch"
        arguments = [str(HARNESS), "--runner", str(runner),
                     "--state", str(self.root / "state"), "--left", str(self.root / "left"),
                     "--right", str(self.root / "right"), "--iterations", "1",
                     "--label", "launch", "--output", str(output), "--revision", "test-revision"]
        with mock.patch.object(sys, "argv", arguments), \
                mock.patch.object(subprocess, "Popen", side_effect=OSError("spawn refused")):
            with self.assertRaises(SystemExit) as caught:
                self.harness.main()
        self.assertEqual(caught.exception.code, 1)
        summary = self.summary(output)
        self.assertIn("runner launch failed", summary["error"])

    def test_parser_exception_stops_the_runner_and_its_child(self):
        child_pid_file = self.root / "child.pid"
        runner_pid_file = self.root / "runner.pid"
        runner = write_runner(self.root,
            f'printf "%s" "$$" > "{runner_pid_file}"\n'
            f'sleep 60 &\nprintf "%s" "$!" > "{child_pid_file}"\n' + process_line() +
            "printf 'metric\\t0\\tTotal\\t1\\n'\nwait\n")
        arguments = [str(HARNESS), "--runner", str(runner), "--state", "state",
                     "--left", "left", "--right", "right", "--iterations", "1",
                     "--label", "cancel", "--output", str(self.root / "out-cancel"),
                     "--revision", "test-revision"]
        original = self.harness.parse_record
        def fail_metric(line, line_number):
            if line.startswith("metric"):
                raise RuntimeError("injected parser failure")
            return original(line, line_number)
        pids = []
        try:
            with mock.patch.object(sys, "argv", arguments), \
                    mock.patch.object(self.harness, "parse_record", side_effect=fail_metric):
                with self.assertRaisesRegex(RuntimeError, "injected parser failure"):
                    self.harness.main()
            pids = [int(path.read_text()) for path in (runner_pid_file, child_pid_file)]
            def running(pid):
                result = subprocess.run(["ps", "-p", str(pid), "-o", "state="],
                                        capture_output=True, text=True, check=False)
                return result.returncode == 0 and not result.stdout.strip().startswith("Z")
            deadline = time.monotonic() + 2
            while any(running(pid) for pid in pids) and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertFalse(any(running(pid) for pid in pids), "benchmark descendants survived cleanup")
            summary = self.summary(self.root / "out-cancel")
            self.assertIn("injected parser failure", summary["error"])
        finally:
            for path in (runner_pid_file, child_pid_file):
                if path.exists():
                    try:
                        os.kill(int(path.read_text()), signal.SIGKILL)
                    except ProcessLookupError:
                        pass

    def test_sigkill_process_group_race_reaps_and_preserves_status(self):
        error, summary, spy, child = self.run_fake_child(
            ["process\t4242\n", "metric\t0\tTotal\t1\n"],
            killpg_side_effect=[None, ProcessLookupError("group already gone")],
            wait_results=[0, subprocess.TimeoutExpired(cmd="time", timeout=5), 0],
        )
        self.assertIsNone(error)
        self.assertIsNotNone(summary)
        self.assertEqual(summary["cleanup_errors"], [])
        self.assertEqual(summary["returncode"], 0)
        self.assertTrue(spy.events[0].is_set())
        self.assertTrue(all(not thread.is_alive() for thread in spy.threads))

    def test_killpg_permission_error_records_actionable_error_and_fails(self):
        error, summary, spy, child = self.run_fake_child(
            ["process\t4242\n", "metric\t0\tTotal\t1\n"],
            killpg_side_effect=PermissionError("killpg refused"),
        )
        self.assertIsInstance(error, SystemExit)
        self.assertEqual(error.code, 1)
        self.assertIsNotNone(summary)
        self.assertTrue(any("PermissionError" in item and "4242" in item
                            for item in summary["cleanup_errors"]))
        self.assertIn("PermissionError", summary["error"])
        self.assertTrue(spy.events[0].is_set())
        self.assertTrue(all(not thread.is_alive() for thread in spy.threads))

    def test_stdout_close_failure_records_actionable_error_and_fails(self):
        error, summary, spy, child = self.run_fake_child(
            ["process\t4242\n", "metric\t0\tTotal\t1\n"],
            close_error=OSError("stdout close refused"),
        )
        self.assertIsInstance(error, SystemExit)
        self.assertEqual(error.code, 1)
        self.assertIsNotNone(summary)
        self.assertTrue(any("stdout" in item and "OSError" in item
                            for item in summary["cleanup_errors"]))
        self.assertIn("stdout", summary["error"])
        self.assertTrue(spy.events[0].is_set())
        self.assertTrue(all(not thread.is_alive() for thread in spy.threads))

    def test_cleanup_failure_preserves_primary_parser_exception(self):
        error, summary, spy, child = self.run_fake_child(
            ["process\t4242\n", "metric\t0\tTotal\t1\n"],
            killpg_side_effect=PermissionError("killpg refused"),
            parse_failure=True,
        )
        self.assertIsInstance(error, RuntimeError)
        self.assertIn("injected parser failure", str(error))
        self.assertIsNotNone(summary)
        self.assertIn("injected parser failure", summary["error"])
        self.assertTrue(summary["cleanup_errors"])
        self.assertTrue(spy.events[0].is_set())
        self.assertTrue(all(not thread.is_alive() for thread in spy.threads))

    def test_cleanup_signals_stopped_and_joins_sampler_before_summary_write(self):
        output = self.root / "out-order"
        runner = write_runner(self.root, "")
        fake_stdout = FakeStdout(["process\t4242\n", "metric\t0\tTotal\t1\n"],
                                 close_error=OSError("stdout close refused"))
        child = FakeChild(fake_stdout, [0, 0])
        spy = ThreadSpy()
        fake_subprocess = types.SimpleNamespace(
            Popen=mock.Mock(return_value=child),
            TimeoutExpired=subprocess.TimeoutExpired,
            run=subprocess.run,
            DEVNULL=subprocess.DEVNULL,
            PIPE=subprocess.PIPE,
            CalledProcessError=subprocess.CalledProcessError,
            check_output=subprocess.check_output,
        )
        arguments = [str(HARNESS), "--runner", str(runner), "--state", "state",
                     "--left", "left", "--right", "right", "--iterations", "1",
                     "--label", "order", "--output", str(output),
                     "--revision", "test-revision"]
        observed = []
        original_write_text = pathlib.Path.write_text
        def spy_write_text(self, data, *args, **kwargs):
            observed.append((spy.events[0].is_set(),
                             all(not thread.is_alive() for thread in spy.threads)))
            return original_write_text(self, data, *args, **kwargs)
        try:
            with mock.patch.object(sys, "argv", arguments), \
                    mock.patch.object(self.harness, "subprocess", fake_subprocess), \
                    mock.patch.object(self.harness, "threading", spy), \
                    mock.patch.object(pathlib.Path, "write_text", autospec=True,
                                      side_effect=spy_write_text):
                with self.assertRaises(SystemExit):
                    self.harness.main()
        finally:
            for event in spy.events:
                event.set()
            for thread in spy.threads:
                if thread.is_alive():
                    thread.join(timeout=5)
        self.assertEqual(observed, [(True, True)])

    def test_existing_output_directory_is_preserved(self):
        runner = write_runner(self.root, "printf 'metric\\t0\\tTotal\\t1\\n'\n")
        output = self.root / "out-existing"
        output.mkdir()
        sentinel = output / "sentinel"
        sentinel.write_text("keep", encoding="utf-8")
        result, output = self.run_harness(runner, "existing", output=output)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(sentinel.read_text(encoding="utf-8"), "keep")
        self.assertFalse((output / "summary.json").exists())

    def test_revision_unavailable_fails_clearly(self):
        runner = write_runner(self.root, "printf 'metric\\t0\\tTotal\\t1\\n'\n")
        env = {**os.environ, "GIT_CEILING_DIRECTORIES": str(self.root)}
        result, output = self.run_harness(runner, "revision", revision=None,
                                          cwd=self.root, env=env)
        self.assertNotEqual(result.returncode, 0)
        summary = self.summary(output)
        self.assertIsNone(summary["revision"])
        self.assertIn("--revision", summary["error"])


if __name__ == "__main__":
    unittest.main()
