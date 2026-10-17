import copy
import os
from pathlib import Path
import tempfile
import unittest

import spike


def capability(value):
    return {"state": "observed", "value": value}


class FidelityTests(unittest.TestCase):
    def setUp(self):
        self.before = {"entries": {"file": {
            "kind": "file", "sha256": "original", "mtime_ns": 123,
            "mode": 0o640, "xattrs": {"tag": "original"},
        }}, "hard_links": [], "sidecars": []}

    def changed(self, field, value):
        after = copy.deepcopy(self.before)
        after["entries"]["file"][field] = value
        return after

    def test_content_loss_is_never_predicted_by_missing_metadata_support(self):
        changes = spike.differences(self.before, self.changed("sha256", "corrupt"), {
            "permissions": capability(False), "xattr:tag": capability(False),
        })
        self.assertFalse(changes[0]["predicted"])

    def test_unknown_metadata_support_does_not_excuse_loss(self):
        changes = spike.differences(self.before, self.changed("xattrs", {}), {
            "xattr:tag": {"state": "unavailable", "value": None},
        })
        self.assertFalse(changes[0]["predicted"])

    def test_unsupported_attribute_predicts_absence_not_corruption(self):
        capabilities = {"xattr:tag": capability(False)}
        missing = spike.differences(self.before, self.changed("xattrs", {}), capabilities)
        corrupt = spike.differences(
            self.before, self.changed("xattrs", {"tag": "corrupt"}), capabilities,
        )
        self.assertTrue(missing[0]["predicted"])
        self.assertFalse(corrupt[0]["predicted"])

    def test_timestamp_tolerance_is_bounded_by_the_probe(self):
        capabilities = {"mtime_observations": capability([
            {"stored_ns": 0}, {"stored_ns": 10}, {"stored_ns": 20},
        ])}
        near = spike.differences(self.before, self.changed("mtime_ns", 120), capabilities)
        far = spike.differences(self.before, self.changed("mtime_ns", 100), capabilities)
        self.assertTrue(near[0]["predicted"])
        self.assertFalse(far[0]["predicted"])

    def test_case_and_unicode_collisions_are_detected_before_copy(self):
        self.assertEqual(spike.name_collisions(
            ["Name", "name", "résumé", "re\u0301sume\u0301"],
            {"case_sensitive": capability(False)},
        ), [["Name", "name"], ["re\u0301sume\u0301", "résumé"]])
        self.assertEqual(spike.name_collisions(
            ["Name", "name"], {"case_sensitive": capability(True)},
        ), [])

    def test_missing_entry_is_a_failure(self):
        after = {"entries": {}, "hard_links": [], "sidecars": []}
        self.assertFalse(spike.differences(self.before, after, {})[0]["predicted"])

    def test_snapshot_preserves_link_identity_without_following_symlinks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "first").write_bytes(b"content")
            os.link(root / "first", root / "second")
            (root / "outside").symlink_to("/nonexistent/foldover-s4")
            captured = spike.snapshot(root)
            self.assertEqual(captured["hard_links"], [["first", "second"]])
            self.assertEqual(captured["entries"]["outside"]["kind"], "symlink")
            self.assertNotIn("sha256", captured["entries"]["outside"])

    def test_snapshot_includes_directory_root_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            root.chmod(0o750)
            captured = spike.snapshot(root)
            self.assertIn(".", captured["entries"])
            self.assertEqual(captured["entries"]["."]["mode"], 0o750)

    def test_only_identified_appledouble_files_are_metadata_sidecars(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "file").write_bytes(b"data")
            (root / "._file").write_bytes(
                spike.APPLEDOUBLE_MAGIC + b"\x00\x02\x00\x00" + bytes(16) +
                b"\x00\x01" + b"\x00\x00\x00\x09" +
                (38).to_bytes(4, "big") + (32).to_bytes(4, "big") + bytes(32)
            )
            (root / "._user-data").write_bytes(b"ordinary file")
            captured = spike.snapshot(root)
            self.assertEqual(captured["sidecars"], ["._file"])
            self.assertIn("._user-data", captured["entries"])

    def test_truncated_appledouble_prefix_remains_an_ordinary_file(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "file").write_bytes(b"data")
            (root / "._file").write_bytes(spike.APPLEDOUBLE_MAGIC)
            captured = spike.snapshot(root)
            self.assertEqual(captured["sidecars"], [])
            self.assertIn("._file", captured["entries"])


if __name__ == "__main__":
    unittest.main()
