#!/usr/bin/env python3
"""Export a clean release commit, vendor dependencies and its pinned Mihomo source."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib

from source_geodata import collect as collect_geodata


def command(args, cwd, capture=False):
    return subprocess.run(args, cwd=cwd, check=True, text=True, stdout=subprocess.PIPE if capture else None).stdout


def snapshot(project, destination):
    dirty = command(["git", "status", "--porcelain", "--untracked-files=normal"], project, True)
    if dirty.strip():
        raise ValueError("Release source must be a clean committed checkout; refusing to archive a different HEAD")
    # Gitlinks are mode 160000 even when a submodule was never initialized.
    # The built-in tree query also avoids Git Bash helper dependencies on Windows.
    tree = command(["git", "ls-tree", "-r", "-z", "HEAD"], project, True)
    if any(entry.startswith("160000 ") for entry in tree.split("\0")):
        raise ValueError("Submodules require explicit corresponding-source export; refusing to omit them")
    commit = command(["git", "rev-parse", "HEAD"], project, True).strip()
    with tempfile.TemporaryDirectory(prefix="zenclash-git-archive-") as temporary:
        archive = Path(temporary) / "tree.tar"
        command(["git", "archive", "--format=tar", "--output=" + str(archive), "HEAD"], project)
        with tarfile.open(archive) as source:
            source.extractall(destination, filter="data")
    return commit


def collect_notices(packages, source_root, output, supplemental=None):
    files = {}
    inventory = []
    reviewed = {}
    if supplemental is not None and (supplemental / "MANIFEST.json").is_file():
        reviewed = json.loads((supplemental / "MANIFEST.json").read_text(encoding="utf-8"))["packages"]
    for package in packages:
        root = Path(package["manifest_path"]).parent.resolve()
        if not root.is_relative_to(source_root.resolve()):
            raise ValueError("Dependency source is outside the archive: " + package["name"])
        if not package.get("source"):
            continue
        name = package["name"] + "-" + package["version"]
        # Preserve actual copyright/license files rather than replacing them with SPDX labels.
        candidates = [path for path in root.rglob("*") if path.is_file() and
                      re.match(r"(?i)^(license|licence|copying|copyright|notice)(?:[._-].*)?$", path.name)]
        license_file = package.get("license_file")
        if license_file:
            candidate = (root / license_file).resolve()
            if not candidate.is_relative_to(root) or not candidate.is_file():
                raise ValueError("Invalid license_file for " + name)
            candidates.append(candidate)
        supplement = reviewed.get(name)
        if supplement:
            if supplement["license"] != package.get("license") or supplement["published_manifest_sha256"] != hashlib.sha256(Path(package["manifest_path"]).read_bytes()).hexdigest():
                raise ValueError("Supplemental notice does not match published dependency: " + name)
            vcs_file = root / ".cargo_vcs_info.json"
            revision = json.loads(vcs_file.read_text(encoding="utf-8"))["git"]["sha1"] if vcs_file.exists() else None
            if revision != supplement.get("vcs_revision"):
                raise ValueError("Supplemental notice VCS revision mismatch: " + name)
            for filename, digest in supplement["files"].items():
                file = (supplemental / name / filename).resolve()
                if not file.is_relative_to((supplemental / name).resolve()) or not file.is_file() or hashlib.sha256(file.read_bytes()).hexdigest() != digest:
                    raise ValueError("Invalid supplemental notice checksum/path: " + name)
                relative = Path("rust") / name / "supplemental" / filename
                destination = output / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(file, destination)
                files[relative.as_posix()] = digest
            provenance = output / "rust" / name / "supplemental" / "PROVENANCE.json"
            provenance.write_text(json.dumps(supplement, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
            files[provenance.relative_to(output).as_posix()] = hashlib.sha256(provenance.read_bytes()).hexdigest()
        if not candidates and not supplement:
            raise ValueError("Dependency has no license text; resolve attribution before release: " + name)
        for path in sorted(set(candidates)):
            if not path.resolve().is_relative_to(root):
                raise ValueError("License symlink leaves source: " + str(path))
            relative = Path("rust") / name / path.relative_to(root)
            destination = output / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, destination)
            files[relative.as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
        inventory.append({key: package.get(key) for key in ("name", "version", "license", "source")})
    return inventory, files


def build(project, output, version, mihomo_tag, geodata_tag="latest"):
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:[.+-][0-9A-Za-z.-]+)?", version):
        raise ValueError("Invalid release version")
    if not re.fullmatch(r"v\d+\.\d+\.\d+(?:[.+-][0-9A-Za-z.-]+)?", mihomo_tag):
        raise ValueError("Mihomo source must use an immutable release tag")
    project, output = project.resolve(), output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="zenclash-corresponding-source-") as temporary:
        root = Path(temporary) / ("ZenClash-" + version + "-source")
        root.mkdir()
        commit = snapshot(project, root)
        workspace = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
        if workspace["workspace"]["package"]["version"] != version:
            raise ValueError("Source archive version does not match Cargo.toml")
        vendor_config = command(["cargo", "vendor", "--locked", "--versioned-dirs", "vendor"], root, True)
        cargo_dir = root / ".cargo"
        cargo_dir.mkdir(exist_ok=True)
        config = cargo_dir / "config.toml"
        previous = config.read_text(encoding="utf-8") if config.exists() else ""
        if "[source." in previous or (cargo_dir / "config").exists():
            raise ValueError("Resolve existing Cargo source replacement before exporting")
        config.write_text(previous + "\n" + vendor_config, encoding="utf-8")
        metadata = json.loads(command(["cargo", "metadata", "--offline", "--locked", "--format-version=1", "--all-features"], root, True))
        notices = root / "dependency-licenses"
        notices.mkdir()
        inventory, files = collect_notices(metadata["packages"], root, notices, root / "resources/licenses/rust-supplemental")
        mihomo_checkout = Path(temporary) / "mihomo-checkout"
        command(["git", "clone", "--depth=1", "--branch", mihomo_tag,
                 "https://github.com/MetaCubeX/mihomo.git", str(mihomo_checkout)], root)
        mihomo = root / "third-party" / "mihomo"
        mihomo.mkdir(parents=True)
        mihomo_commit = snapshot(mihomo_checkout, mihomo)
        command(["go", "mod", "vendor"], mihomo)
        for path in (mihomo / "LICENSE",):
            destination = notices / "mihomo" / path.name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, destination)
            files["mihomo/" + path.name] = hashlib.sha256(path.read_bytes()).hexdigest()
        for path in (mihomo / "vendor").rglob("*"):
            if path.is_file() and re.match(r"(?i)^(license|licence|copying|copyright|notice)(?:[._-].*)?$", path.name):
                relative = Path("mihomo") / "vendor" / path.relative_to(mihomo / "vendor")
                destination = notices / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(path, destination)
                files[relative.as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
        geodata, geodata_hash, geodata_files = collect_geodata(root, notices, geodata_tag)
        files.update(geodata_files)
        bundled_resources = notices / "resources"
        bundled_resources.mkdir()
        shutil.copy2(geodata, bundled_resources / "geoip.metadb")
        lock_hash = hashlib.sha256((root / "Cargo.lock").read_bytes()).hexdigest()
        manifest = {"zenclash_version": version, "zenclash_commit": commit, "cargo_lock_sha256": lock_hash,
                    "mihomo_tag": mihomo_tag, "mihomo_commit": mihomo_commit,
                    "geodata_sha256": geodata_hash, "geodata_tag_requested": geodata_tag, "packages": inventory, "files": files}
        (notices / "MANIFEST.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        source_manifest = dict(manifest)
        source_manifest["rustc"] = command(["rustc", "--version", "--verbose"], root, True).strip()
        source_manifest["go"] = command(["go", "version"], mihomo, True).strip()
        (root / "SOURCE-MANIFEST.json").write_text(json.dumps(source_manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        (root / "SOURCE-BUILD.md").write_text(
            "# Rebuild ZenClash " + version + "\n\n"
            "The snapshot is release commit " + commit + ". Rust registry and Git dependencies are in vendor/; "
            "the generated .cargo/config.toml selects them. Use cargo build --offline --locked -p zenclash-ui --bin zenclash "
            "and cargo build --offline --locked -p zenclash-service --features standalone,client --bins. "
            "Install the platform toolchain and system libraries described in README.md and scripts/install_linux_build_deps.sh.\n\n"
            "third-party/mihomo contains " + mihomo_tag + " (" + mihomo_commit + ") and its Go vendor/ tree. "
            "Follow its Makefile and .github/workflows to rebuild the platform release binary with go build -mod=vendor. "
            "Mihomo release build flags and target variants belong to that upstream build process.\n\n"
            "Use scripts/build_windows_installer.ps1, build_macos_package.sh, build_deb_package.sh or build_rpm_package.sh "
            "with your rebuilt Mihomo supplied via ZENCLASH_MIHOMO_BINARY and geoip.metadb via ZENCLASH_GEODATA_FILE. "
            "Set ZENCLASH_DEPENDENCY_LICENSE_DIR to the absolute path of dependency-licenses/. "
            "The scripts and platform resources are included. GUI helpers use ordinary OS elevation when installing a service. "
            "macOS permits ad-hoc signing when APPLE_SIGNING_IDENTITY is unset; Windows installers use no required publisher signature. "
            "No ZenClash publisher key is required to build or install a modified version.\n\n"
            "third-party/geodata retains the original GeoIP dat input and an editable geoip.json containing every country/CIDR. "
            "Rebuild a modified dat using python scripts/source_geodata.py --rebuild-json third-party/geodata/geoip.json --output-dat modified.dat. "
            "Build third-party/geo-converter with go build -mod=vendor -o geo ./cmd/geo, then geo convert ip -i v2ray -o meta -f geoip.metadb modified.dat. "
            "The pinned converter source, Go dependencies, original data notices and exact resource hash are included. "
            "Use the archived geoip.metadb for an unchanged rebuild; release notices match this exact resource.\n\n"
            "Original license files and attribution are retained. dependency-licenses/MANIFEST.json is an inventory; "
            "it does not itself certify license compatibility. Additional runtime assets retain their own upstream terms.\n",
            encoding="utf-8")
        archive = output / ("ZenClash-" + version + "-corresponding-source.tar.gz")
        with tarfile.open(archive, "w:gz") as bundle:
            bundle.add(root, arcname=root.name)
        shutil.copytree(notices, output / "dependency-licenses", dirs_exist_ok=True)
        print("Created " + str(archive))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--mihomo-tag", required=True)
    parser.add_argument("--geodata-tag", default="latest")
    args = parser.parse_args()
    build(args.project, args.output, args.version, args.mihomo_tag, args.geodata_tag)


if __name__ == "__main__":
    main()
