"""Regression checks for actual notice files and corresponding-source boundaries."""

import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import sys
import unittest


SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))


def module(name):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / (name + ".py"))
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


stage_module = module("stage_release_licenses")
source_module = module("build_corresponding_source")
geodata_module = module("source_geodata")


class LicenseStageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="zenclash-license-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.project = self.root / "project"
        self.project.mkdir()
        (self.project / "Cargo.lock").write_bytes(b"fixture lock")
        for crate in ("zenclash-service", "zenclash-service-integration"):
            folder = self.project / "crates" / crate
            folder.mkdir(parents=True)
            for name in ("LICENSE", "NOTICE.md", "README.md", "UPSTREAM.json"):
                (folder / name).write_bytes((crate + " " + name).encode())
        (self.project / "LICENSE").write_bytes(b"fixture GPL")
        (self.project / "NOTICE.md").write_bytes(b"fixture authors")
        self.notices = self.root / "dependency-licenses"
        (self.notices / "mihomo").mkdir(parents=True)
        (self.notices / "mihomo" / "LICENSE").write_bytes(b"fixture license text")
        self.manifest = {"cargo_lock_sha256": hashlib.sha256(b"fixture lock").hexdigest(),
                         "mihomo_tag": "v1.19.30", "packages": [{"name": "fixture"}],
                         "files": {"mihomo/LICENSE": hashlib.sha256(b"fixture license text").hexdigest()}}
        self.destination = self.root / "payload"

    def stage(self):
        (self.notices / "MANIFEST.json").write_text(json.dumps(self.manifest), encoding="utf-8")
        stage_module.stage(self.project, self.destination, "9.8.7", self.notices, "v1.19.30")

    def test_preserves_both_original_licenses_and_authors_byte_for_byte(self):
        self.stage()
        for crate in ("zenclash-service", "zenclash-service-integration"):
            for name in ("LICENSE", "NOTICE.md", "README.md", "UPSTREAM.json"):
                self.assertEqual((self.destination / "licenses" / crate / name).read_bytes(),
                                 (self.project / "crates" / crate / name).read_bytes())
        self.assertIn("ZenClash-9.8.7-corresponding-source.tar.gz",
                      (self.destination / "CORRESPONDING-SOURCE.md").read_text(encoding="utf-8"))

    def test_refuses_a_different_dependency_lock(self):
        (self.project / "Cargo.lock").write_bytes(b"changed dependency")
        with self.assertRaisesRegex(ValueError, "Cargo.lock"):
            self.stage()
        self.assertFalse(self.destination.exists())

    def test_refuses_a_different_bundled_core_tag(self):
        self.manifest["mihomo_tag"] = "v1.0.0"
        with self.assertRaisesRegex(ValueError, "Mihomo tag"):
            self.stage()

    def test_refuses_missing_or_modified_license_text(self):
        (self.notices / "mihomo" / "LICENSE").write_bytes(b"modified")
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            self.stage()

    def test_rejects_inventory_path_outside_notice_directory(self):
        self.manifest["files"]["../private"] = "unused"
        with self.assertRaisesRegex(ValueError, "Invalid dependency notice path"):
            self.stage()

    def test_does_not_package_unlisted_files(self):
        (self.notices / "private-profile.yaml").write_bytes(b"private fixture")
        self.stage()
        self.assertFalse((self.destination / "licenses" / "dependencies" / "private-profile.yaml").exists())


    def test_geodata_byte_identity_is_checked_before_staging(self):
        data = self.root / "geoip.metadb"
        data.write_bytes(b"actual geo fixture")
        self.manifest["geodata_sha256"] = hashlib.sha256(data.read_bytes()).hexdigest()
        (self.notices / "geodata").mkdir()
        for name in ("NOTICE.md", "SOURCE.json"):
            content = b"retained GeoData source fixture"
            (self.notices / "geodata" / name).write_bytes(content)
            self.manifest["files"]["geodata/" + name] = hashlib.sha256(content).hexdigest()
        (self.notices / "MANIFEST.json").write_text(json.dumps(self.manifest), encoding="utf-8")
        stage_module.stage(self.project, self.destination, "9.8.7", self.notices, "v1.19.30", data)
        data.write_bytes(b"different mutable latest")
        with self.assertRaisesRegex(ValueError, "GeoData does not match"):
            stage_module.stage(self.project, self.root / "wrong-payload", "9.8.7", self.notices, "v1.19.30", data)
        self.assertFalse((self.root / "wrong-payload").exists())

    def test_missing_geodata_attribution_refuses_native_payload(self):
        data = self.root / "geoip.metadb"
        data.write_bytes(b"actual geo fixture")
        self.manifest["geodata_sha256"] = hashlib.sha256(data.read_bytes()).hexdigest()
        (self.notices / "MANIFEST.json").write_text(json.dumps(self.manifest), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "GeoData source attribution is missing"):
            stage_module.stage(self.project, self.destination, "9.8.7", self.notices, "v1.19.30", data)


class SourceSnapshotTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="zenclash-source-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.repository = self.root / "repository"
        self.repository.mkdir()
        self.git("init", "--quiet")
        self.git("config", "user.name", "Source test fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        (self.repository / "LICENSE").write_bytes(b"fixture license")
        self.git("add", "LICENSE")
        self.git("commit", "--quiet", "-m", "Source fixture")

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.repository, check=True, capture_output=True, text=True)

    def test_snapshot_contains_exact_committed_source(self):
        destination = self.root / "export"
        destination.mkdir()
        commit = source_module.snapshot(self.repository, destination)
        self.assertEqual(commit, self.git("rev-parse", "HEAD").stdout.strip())
        self.assertEqual((destination / "LICENSE").read_bytes(), b"fixture license")
        self.assertFalse((destination / ".git").exists())

    def test_dirty_tree_cannot_export_wrong_head_for_binary(self):
        (self.repository / "LICENSE").write_bytes(b"modified source")
        with self.assertRaisesRegex(ValueError, "clean committed checkout"):
            source_module.snapshot(self.repository, self.root / "export")

    def test_uncommitted_new_source_cannot_be_silently_omitted(self):
        (self.repository / "new-integration.rs").write_bytes(b"source fixture")
        with self.assertRaisesRegex(ValueError, "clean committed checkout"):
            source_module.snapshot(self.repository, self.root / "export")

    def test_uninitialized_gitlink_cannot_be_silently_omitted(self):
        commit = self.git("rev-parse", "HEAD").stdout.strip()
        (self.repository / "vendor-example").mkdir()
        self.git("update-index", "--add", "--cacheinfo", "160000," + commit + ",vendor-example")
        self.git("commit", "--quiet", "-m", "Uninitialized submodule fixture")
        self.assertFalse(self.git("status", "--porcelain").stdout.strip())
        with self.assertRaisesRegex(ValueError, "Submodules require explicit"):
            source_module.snapshot(self.repository, self.root / "export")

    def test_dependency_without_license_text_blocks_release(self):
        package = {"name": "fixture", "version": "1.0.0", "source": "registry+fixture",
                   "manifest_path": str(self.repository / "Cargo.toml")}
        (self.repository / "LICENSE").unlink()
        with self.assertRaisesRegex(ValueError, "no license text"):
            source_module.collect_notices([package], self.repository, self.root / "notices")


class SupplementalNoticeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="zenclash-supplement-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.dependency = self.root / "vendor" / "fixture-1.0.0"
        self.dependency.mkdir(parents=True)
        self.manifest = self.dependency / "Cargo.toml"
        self.manifest.write_bytes(b"published manifest")
        (self.dependency / ".cargo_vcs_info.json").write_text(json.dumps({"git": {"sha1": "a" * 40}}), encoding="utf-8")
        self.package = {"name": "fixture", "version": "1.0.0", "license": "MIT", "source": "registry+fixture", "manifest_path": str(self.manifest)}
        self.supplemental = self.root / "reviewed"
        folder = self.supplemental / "fixture-1.0.0"
        folder.mkdir(parents=True)
        self.license = folder / "LICENSE-MIT"
        self.license.write_bytes(b"actual upstream permission and copyright fixture")
        self.record = {"license": "MIT", "published_manifest_sha256": hashlib.sha256(self.manifest.read_bytes()).hexdigest(),
                       "vcs_revision": "a" * 40, "files": {"LICENSE-MIT": hashlib.sha256(self.license.read_bytes()).hexdigest()}}

    def collect(self):
        (self.supplemental / "MANIFEST.json").write_text(json.dumps({"packages": {"fixture-1.0.0": self.record}}), encoding="utf-8")
        return source_module.collect_notices([self.package], self.root, self.root / "notices", self.supplemental)

    def test_preserves_exact_recovered_text_and_provenance(self):
        inventory, files = self.collect()
        path = "rust/fixture-1.0.0/supplemental/LICENSE-MIT"
        self.assertEqual((self.root / "notices" / path).read_bytes(), self.license.read_bytes())
        self.assertIn(path, files)
        self.assertIn("rust/fixture-1.0.0/supplemental/PROVENANCE.json", files)
        self.assertEqual(inventory[0]["name"], "fixture")

    def test_rejects_changed_published_manifest(self):
        self.manifest.write_bytes(b"different published crate")
        with self.assertRaisesRegex(ValueError, "match published"):
            self.collect()

    def test_rejects_different_vcs_revision(self):
        self.record["vcs_revision"] = "b" * 40
        with self.assertRaisesRegex(ValueError, "VCS revision mismatch"):
            self.collect()

    def test_rejects_tampered_text_and_path_escape(self):
        self.license.write_bytes(b"altered copyright fixture")
        with self.assertRaisesRegex(ValueError, "checksum/path"):
            self.collect()
        self.record["files"] = {"../private": "unused"}
        with self.assertRaisesRegex(ValueError, "checksum/path"):
            self.collect()


class GeoDataEditableSourceTests(unittest.TestCase):
    def test_round_trip_keeps_ipv4_ipv6_inverse_and_default_prefix(self):
        data = [{"country_code": "PRIVATE", "inverse_match": True, "cidrs": ["10.0.0.0/8", "::/0", "fd00::/8"]},
                {"country_code": "CN", "inverse_match": False, "cidrs": ["1.0.0.0/24"]}]
        self.assertEqual(geodata_module.editable_geoip(geodata_module.encode_geoip(data)), data)

    def test_truncation_and_unhandled_fields_cannot_silently_lose_source(self):
        with self.assertRaisesRegex(ValueError, "Truncated"):
            geodata_module.editable_geoip(b"\x0a\x05\x01")
        with self.assertRaisesRegex(ValueError, "Unexpected GeoIPList"):
            geodata_module.editable_geoip(geodata_module.encode_field(2, b"new field"))
        bad = geodata_module.encode_field(1, b"CN") + geodata_module.encode_field(7, b"unhandled")
        with self.assertRaisesRegex(ValueError, "New GeoIP fields"):
            geodata_module.editable_geoip(geodata_module.encode_field(1, bad))


if __name__ == "__main__":
    unittest.main()
