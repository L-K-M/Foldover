#!/usr/bin/env python3
"""Exercise release notes with the same versions the publishing pipeline accepts."""

import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent


class ReleaseNotesTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        root = pathlib.Path(self.directory.name)
        scripts = root / "scripts"
        scripts.mkdir()
        for name in ("release-notes.py", "version-shape.sh"):
            shutil.copyfile(ROOT / "scripts" / name, scripts / name)
        self.script = scripts / "release-notes.py"
        (root / "CHANGELOG.md").write_text(
            "# Changes\n\n## [1.2.3]\nStable change.\n\n"
            "## [1.2.3-beta.1]\nBeta change.\n\n"
            "## [1.2.2]\nOlder change.\n",
            encoding="utf-8",
        )

    def notes(self, *arguments):
        return subprocess.run(
            [sys.executable, str(self.script), *arguments],
            capture_output=True, text=True, check=False,
        )

    def test_stable_and_prerelease_notes(self):
        for version, change in (("1.2.3", "Stable change."), ("1.2.3-beta.1", "Beta change.")):
            with self.subTest(version=version):
                result = self.notes(version)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(f"Foldover-{version}.zip", result.stdout)
                self.assertIn(change, result.stdout)
                self.assertNotIn("Older change.", result.stdout)
                other = "Beta change." if version == "1.2.3" else "Stable change."
                self.assertNotIn(other, result.stdout)

    def test_valid_versions_without_changelog_use_fallback(self):
        for version in ("0.0.0", "2.0.0-rc.10", "2.0.0-alpha-1", "2.0.0-0"):
            with self.subTest(version=version):
                result = self.notes(version)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(f"Foldover-{version}.zip", result.stdout)
                self.assertIn("See CHANGELOG.md.", result.stdout)

    def test_malformed_versions_produce_no_notes(self):
        for version in ("", "v1.2.3", "1.2", "01.2.3", "1.2.3-beta.01",
                        "1.2.3+build", "1.2.3-beta..1", "1.2.3\n"):
            with self.subTest(version=version):
                result = self.notes(version)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")

    def test_argument_count(self):
        for arguments in ((), ("1.2.3", "extra")):
            with self.subTest(arguments=arguments):
                result = self.notes(*arguments)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
