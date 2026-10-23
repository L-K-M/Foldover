#!/usr/bin/env python3
"""Portable tests for scripts/agent-plist.py.

Runs via: python3 scripts/test-agent-plist.py
"""

import importlib.util
import plistlib
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

# Load the hyphenated module via importlib.util.
_module_path = Path(__file__).resolve().parent / "agent-plist.py"
_spec = importlib.util.spec_from_file_location("agent_plist", _module_path)
_agent_plist = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_agent_plist)


class TestSignedLabel(unittest.TestCase):
    """Tests for the signed_label() function."""

    def test_single_valid_hash(self):
        result = mock.Mock()
        result.stderr = "CDHash=abcdef0123456789abcdef0123456789abcdef01\n"
        with mock.patch.object(subprocess, "run", return_value=result):
            label = _agent_plist.signed_label("/fake/helper")
        self.assertEqual(label, "ch.lkmc.foldover.agent.abcdef0123456789abcdef0123456789abcdef01")

    def test_missing_hash(self):
        result = mock.Mock()
        result.stderr = "some other output\n"
        with mock.patch.object(subprocess, "run", return_value=result):
            with self.assertRaises(RuntimeError):
                _agent_plist.signed_label("/fake/helper")

    def test_malformed_hash(self):
        result = mock.Mock()
        result.stderr = "CDHash=SHORT\n"
        with mock.patch.object(subprocess, "run", return_value=result):
            with self.assertRaises(RuntimeError):
                _agent_plist.signed_label("/fake/helper")

    def test_uppercase_hash_rejected(self):
        result = mock.Mock()
        result.stderr = "CDHash=ABCDEF0123456789ABCDEF0123456789ABCDEF01\n"
        with mock.patch.object(subprocess, "run", return_value=result):
            with self.assertRaises(RuntimeError):
                _agent_plist.signed_label("/fake/helper")

    def test_multiple_hashes(self):
        result = mock.Mock()
        result.stderr = (
            "CDHash=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"
            "CDHash=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n"
        )
        with mock.patch.object(subprocess, "run", return_value=result):
            with self.assertRaises(RuntimeError):
                _agent_plist.signed_label("/fake/helper")

    def test_subprocess_error_propagates(self):
        with mock.patch.object(subprocess, "run", side_effect=subprocess.CalledProcessError(1, [])):
            with self.assertRaises(subprocess.CalledProcessError):
                _agent_plist.signed_label("/fake/helper")


class TestCLIMain(unittest.TestCase):
    """Tests for the main() CLI entry point via sys.argv and tempfiles."""

    def _make_plist(self, extra=None):
        """Return a temp file path containing a minimal launchd plist."""
        data = {"Label": "ch.lkmc.foldover.agent", "Program": "/usr/bin/test"}
        if extra:
            data.update(extra)
        tmp = tempfile.NamedTemporaryFile(suffix=".plist", delete=False)
        tmp.write(plistlib.dumps(data))
        tmp.close()
        path = Path(tmp.name)
        self.addCleanup(path.unlink)
        return path

    def test_stamp_changes_label_preserves_keys(self):
        plist_path = self._make_plist({"RunAtLoad": True})
        result = mock.Mock()
        result.stderr = "CDHash=abcdef0123456789abcdef0123456789abcdef01\n"
        with mock.patch.object(subprocess, "run", return_value=result):
            with mock.patch.object(sys, "argv", ["agent-plist", "stamp", "/fake/helper", str(plist_path)]):
                _agent_plist.main()
        contents = plistlib.loads(plist_path.read_bytes())
        self.assertEqual(contents["Label"], "ch.lkmc.foldover.agent.abcdef0123456789abcdef0123456789abcdef01")
        self.assertEqual(contents["Program"], "/usr/bin/test")
        self.assertTrue(contents["RunAtLoad"])

    def test_check_accepts_matching_hash(self):
        plist_path = self._make_plist()
        # First stamp it
        result = mock.Mock()
        result.stderr = "CDHash=abcdef0123456789abcdef0123456789abcdef01\n"
        with mock.patch.object(subprocess, "run", return_value=result):
            with mock.patch.object(sys, "argv", ["agent-plist", "stamp", "/fake/helper", str(plist_path)]):
                _agent_plist.main()
        # Then check it
        with mock.patch.object(subprocess, "run", return_value=result):
            with mock.patch.object(sys, "argv", ["agent-plist", "check", "/fake/helper", str(plist_path)]):
                _agent_plist.main()

    def test_check_rejects_mismatch(self):
        plist_path = self._make_plist()
        stamp_result = mock.Mock()
        stamp_result.stderr = "CDHash=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"
        check_result = mock.Mock()
        check_result.stderr = "CDHash=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n"
        with mock.patch.object(subprocess, "run", return_value=stamp_result):
            with mock.patch.object(sys, "argv", ["agent-plist", "stamp", "/fake/helper", str(plist_path)]):
                _agent_plist.main()
        with mock.patch.object(subprocess, "run", return_value=check_result):
            with mock.patch.object(sys, "argv", ["agent-plist", "check", "/fake/helper", str(plist_path)]):
                with self.assertRaises(RuntimeError):
                    _agent_plist.main()

    def test_repeated_stamp_idempotent(self):
        plist_path = self._make_plist()
        result = mock.Mock()
        result.stderr = "CDHash=abcdef0123456789abcdef0123456789abcdef01\n"
        expected_label = "ch.lkmc.foldover.agent.abcdef0123456789abcdef0123456789abcdef01"
        previous = None
        for _ in range(3):
            with mock.patch.object(subprocess, "run", return_value=result):
                with mock.patch.object(sys, "argv", ["agent-plist", "stamp", "/fake/helper", str(plist_path)]):
                    _agent_plist.main()
            current = plist_path.read_bytes()
            if previous is not None:
                self.assertEqual(current, previous)
            previous = current
        contents = plistlib.loads(plist_path.read_bytes())
        self.assertEqual(contents["Label"], expected_label)
        self.assertEqual(contents["Program"], "/usr/bin/test")


if __name__ == "__main__":
    unittest.main()
