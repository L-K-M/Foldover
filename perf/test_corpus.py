#!/usr/bin/env python3
"""Focused checks for the corpus fixture guard's path and marker rules."""

import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

PROJECT = Path(__file__).resolve().parent.parent
CORPUS = PROJECT / "perf" / "make-corpus.py"


class CorpusGuardTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.base = Path(self.directory.name)
        # Make created file modes independent of the runner's inherited mask.
        self.previous_umask = os.umask(0o077)
        self.addCleanup(os.umask, self.previous_umask)

    def run_corpus(self, target, files=4, bytes_=8, width=2):
        return subprocess.run(
            [sys.executable, str(CORPUS), str(target),
             "--files", str(files), "--bytes", str(bytes_), "--width", str(width)],
            capture_output=True, text=True, check=False, timeout=60,
        )

    def test_marker_component_prefix_is_required(self):
        # The exact component and the real random suffixes are all fixtures.
        for name in ("foldover-perf", "foldover-perf-abc123", "foldover-perf.2026"):
            with self.subTest(name=name):
                (self.base / name).mkdir()
                target = self.base / name / "source"
                result = self.run_corpus(target)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue(target.is_dir())
        # A leading other component is not the marker.
        (self.base / "my-foldover-perf-notes").mkdir()
        result = self.run_corpus(self.base / "my-foldover-perf-notes" / "source")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("foldover-perf", result.stderr)
        self.assertEqual(list((self.base / "my-foldover-perf-notes").iterdir()), [])

    def test_traversal_outside_the_fixture_is_rejected_without_writes(self):
        fixture = self.base / "foldover-perf-abc"
        fixture.mkdir()
        escape = self.base / "escape"
        escape.mkdir()
        target = fixture / ".." / "escape" / "new"
        result = self.run_corpus(target)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("foldover-perf", result.stderr)
        self.assertFalse((escape / "new").exists())
        self.assertEqual(list(escape.iterdir()), [])

    def test_symlinked_marker_to_outside_is_rejected_without_writes(self):
        outside = self.base / "outside"
        outside.mkdir()
        link = self.base / "foldover-perf-link"
        link.symlink_to(outside, target_is_directory=True)
        result = self.run_corpus(link / "new")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("foldover-perf", result.stderr)
        self.assertFalse((outside / "new").exists())
        self.assertEqual(list(outside.iterdir()), [])

    def test_good_marker_path_creates_deterministic_private_files(self):
        (self.base / "foldover-perf-abc").mkdir()
        target = self.base / "foldover-perf-abc" / "source"
        result = self.run_corpus(target, files=4, bytes_=8, width=2)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(target.stat().st_mode & 0o777, 0o700)
        for index in range(4):
            path = target / f"d-{index // 2:06}" / f"f-{index % 2:06}"
            self.assertEqual(
                path.read_bytes(),
                hashlib.shake_256(str(index).encode()).digest(8),
            )
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(len(list(target.rglob("f-*"))), 4)

    def test_repeated_target_is_rejected_without_modification(self):
        (self.base / "foldover-perf-abc").mkdir()
        target = self.base / "foldover-perf-abc" / "source"
        result = self.run_corpus(target, files=4, bytes_=8, width=2)
        self.assertEqual(result.returncode, 0, result.stderr)
        path = target / "d-000000" / "f-000000"
        before = (path.read_bytes(), path.stat().st_mtime_ns)
        result = self.run_corpus(target, files=4, bytes_=8, width=2)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("Traceback", result.stderr)
        self.assertIn("already exists", result.stderr)
        self.assertEqual(before, (path.read_bytes(), path.stat().st_mtime_ns))
        self.assertEqual(len(list(target.rglob("f-*"))), 4)

    def test_zero_byte_files_are_allowed(self):
        (self.base / "foldover-perf-zero").mkdir()
        target = self.base / "foldover-perf-zero" / "source"
        result = self.run_corpus(target, files=2, bytes_=0, width=2)
        self.assertEqual(result.returncode, 0, result.stderr)
        for index in range(2):
            path = target / "d-000000" / f"f-{index:06}"
            self.assertEqual(path.read_bytes(), b"")

    def test_unresolvable_fixture_path_is_rejected_cleanly(self):
        loop = self.base / "foldover-perf-loop"
        loop.symlink_to(loop, target_is_directory=True)
        result = self.run_corpus(loop / "source")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("Traceback", result.stderr)
        self.assertEqual(list(self.base.iterdir()), [loop])


if __name__ == "__main__":
    unittest.main()
