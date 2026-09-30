"""Behavior checks for isolated probe preparation; no native settings or app launch."""
import json
from pathlib import Path
import tempfile
import unittest

import full_app_probe as probe


class ProbeTests(unittest.TestCase):
    def test_fixture_and_all_storage_roots_are_isolated(self):
        args = probe.arguments(["--nodes", "3", "--rules", "5", "--idle-seconds", "0"])
        with tempfile.TemporaryDirectory(prefix="zenclash-probe-test-") as directory:
            root = Path(directory)
            manifest = probe.prepare(args, root)
            self.assertEqual(manifest["rules"], 6)
            self.assertNotEqual(manifest["controller_port"], 0)
            fixture = (root / "fixture.yaml").read_text()
            self.assertIn("tun: {enable: false}", fixture)
            self.assertIn("fixture-000004.example", fixture)
            catalog = json.loads((root / "data/profiles/profiles.json").read_text())
            record = catalog["profiles"][0]
            self.assertEqual(record["size_bytes"], (root / "fixture.yaml").stat().st_size)
            self.assertEqual((root / "data/profiles/files/fixture.yaml").read_text(), fixture)
            preferences = json.loads((root / "data/preferences.json").read_text())
            self.assertFalse(preferences["system_proxy_enabled"])
            self.assertIsNone(preferences["system_proxy_ownership"])
            for filename in probe.DATA_ROOTS:
                content = (root / "source/crates/zenclash-core/src" / filename).read_text()
                self.assertIn(str(root / "data"), content)
                self.assertNotIn("Library/Application Support/ZenClash", content)

    def test_changed_source_aborts_instead_of_using_host_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "source.rs"
            path.write_text("changed source")
            with self.assertRaisesRegex(ValueError, "source changed"):
                probe.replace_once(path, "expected location", "isolated location")
            self.assertEqual(path.read_text(), "changed source")



if __name__ == "__main__":
    unittest.main()
