#!/usr/bin/env python3
"""Exercise release notes with the same versions the publishing pipeline accepts."""

import datetime
import os
import pathlib
import re
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


class RollChangelogTests(unittest.TestCase):
    """The release's post-bump step cuts the section the notes then print."""

    UNRELEASED = "# Changes\n\nIntro.\n\n## [Unreleased]\n\n### Fixed\n\n- New fix.\n\n"
    OLDER = "## [1.2.2] - 2026-01-02\n\n- Older change.\n"

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = pathlib.Path(self.directory.name)
        scripts = self.root / "scripts"
        scripts.mkdir()
        for name in ("release-notes.py", "roll-changelog.py", "version-shape.sh"):
            shutil.copyfile(ROOT / "scripts" / name, scripts / name)
        self.changelog = self.root / "CHANGELOG.md"
        self.changelog.write_text(self.UNRELEASED + self.OLDER, encoding="utf-8")

    def roll(self, version, *arguments):
        environment = dict(os.environ, RELEASE_NEW_VERSION=version)
        return subprocess.run(
            [sys.executable, str(self.root / "scripts" / "roll-changelog.py"), *arguments],
            capture_output=True, text=True, check=False, env=environment,
        )

    def notes(self, version):
        return subprocess.run(
            [sys.executable, str(self.root / "scripts" / "release-notes.py"), version],
            capture_output=True, text=True, check=True,
        ).stdout

    def test_unreleased_entries_become_the_releases_notes(self):
        for version in ("1.2.3", "1.2.3-beta.1"):
            with self.subTest(version=version):
                self.changelog.write_text(self.UNRELEASED + self.OLDER, encoding="utf-8")
                result = self.roll(version)
                self.assertEqual(result.returncode, 0, result.stderr)
                text = self.changelog.read_text(encoding="utf-8")
                today = datetime.date.today().isoformat()
                self.assertEqual(
                    text,
                    "# Changes\n\nIntro.\n\n## [Unreleased]\n\n"
                    f"## [{version}] - {today}\n\n### Fixed\n\n- New fix.\n\n" + self.OLDER,
                )
                notes = self.notes(version)
                self.assertIn("- New fix.", notes)
                self.assertNotIn("Older change.", notes)
                self.assertNotIn("See CHANGELOG.md.", notes)

    def test_a_rerun_changes_nothing(self):
        self.assertEqual(self.roll("1.2.3").returncode, 0)
        once = self.changelog.read_text(encoding="utf-8")
        result = self.roll("1.2.3")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.changelog.read_text(encoding="utf-8"), once)

    def test_the_check_passes_only_for_a_released_section(self):
        before = self.changelog.read_text(encoding="utf-8")
        missing = self.roll("1.2.3", "--check")
        self.assertNotEqual(missing.returncode, 0)
        self.assertIn("no [1.2.3] section", missing.stderr)
        self.assertEqual(self.changelog.read_text(encoding="utf-8"), before)
        self.assertEqual(self.roll("1.2.2", "--check").returncode, 0)
        self.assertEqual(self.changelog.read_text(encoding="utf-8"), before)
        self.assertNotEqual(self.roll("1.2.3", "--roll").returncode, 0)

    def test_a_release_without_entries_is_refused(self):
        for text in ("# Changes\n\n## [Unreleased]\n\n" + self.OLDER,
                     "# Changes\n\n## [Unreleased]\n",
                     "# Changes\n\n" + self.OLDER):
            with self.subTest(text=text):
                self.changelog.write_text(text, encoding="utf-8")
                result = self.roll("1.2.3")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("error:", result.stderr)
                self.assertEqual(self.changelog.read_text(encoding="utf-8"), text)

    def test_malformed_versions_are_refused(self):
        before = self.changelog.read_text(encoding="utf-8")
        for version in ("", "v1.2.3", "1.2", "1.2.3+build"):
            with self.subTest(version=version):
                result = self.roll(version)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.changelog.read_text(encoding="utf-8"), before)

    def test_the_repositorys_changelog_can_be_released(self):
        """Its Unreleased heading is where the hook expects it."""
        text = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
        self.assertEqual(len(re.findall(r"^## \[Unreleased\]$", text, re.M)), 1)
        self.assertLess(text.index("## [Unreleased]"), text.index("## [0.2.0]"))


if __name__ == "__main__":
    unittest.main()
