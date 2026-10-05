#!/usr/bin/env python3
"""Create dependency attribution fixtures for mocked package tests, never a release."""

import hashlib
import json
from pathlib import Path
import sys


project, output, geodata = map(Path, sys.argv[1:])
(output / "mihomo").mkdir(parents=True, exist_ok=True)
license_bytes = (project / "LICENSE").read_bytes()
(output / "mihomo" / "LICENSE").write_bytes(license_bytes)
manifest = {
    "fixture_only": True,
    "cargo_lock_sha256": hashlib.sha256((project / "Cargo.lock").read_bytes()).hexdigest(),
    "mihomo_tag": "v1.19.30",
    "packages": [{"name": "packaging-test-fixture", "version": "0"}],
    "files": {"mihomo/LICENSE": hashlib.sha256(license_bytes).hexdigest()},
}
(output / "geodata").mkdir()
for name in ("NOTICE.md", "SOURCE.json"):
    content = b"GeoData packaging test fixture only"
    (output / "geodata" / name).write_bytes(content)
    manifest["files"]["geodata/" + name] = hashlib.sha256(content).hexdigest()
manifest["geodata_sha256"] = hashlib.sha256(geodata.read_bytes()).hexdigest()
(output / "MANIFEST.json").write_text(json.dumps(manifest), encoding="utf-8")
