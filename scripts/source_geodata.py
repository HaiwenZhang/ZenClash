#!/usr/bin/env python3
"""Build pinned, attributed GeoData from its editable JSON and original dat input."""

import hashlib
import ipaddress
import json
from pathlib import Path
import re
import shutil
import subprocess
import urllib.request


def fetch(url):
    request = urllib.request.Request(url, headers={"User-Agent": "ZenClash-source-export", "Accept": "application/vnd.github+json"})
    with urllib.request.urlopen(request, timeout=60) as response:
        return response.read()


def varint(data, offset):
    value = 0
    for shift in range(0, 70, 7):
        if offset >= len(data):
            raise ValueError("Truncated GeoIP varint")
        byte = data[offset]
        offset += 1
        value |= (byte & 127) << shift
        if byte < 128:
            return value, offset
    raise ValueError("Oversized GeoIP varint")


def fields(data):
    offset = 0
    while offset < len(data):
        key, offset = varint(data, offset)
        number, wire = key >> 3, key & 7
        if number == 0:
            raise ValueError("Invalid GeoIP field")
        if wire == 0:
            value, offset = varint(data, offset)
        elif wire == 2:
            length, offset = varint(data, offset)
            if length > len(data) - offset:
                raise ValueError("Truncated GeoIP field")
            value = data[offset:offset + length]
            offset += length
        else:
            raise ValueError("Unsupported GeoIP wire type; do not omit data")
        yield number, value


def editable_geoip(data):
    result = []
    for number, entry in fields(data):
        if number != 1 or not isinstance(entry, bytes):
            raise ValueError("Unexpected GeoIPList field")
        country, inverse, networks = None, False, []
        for tag, value in fields(entry):
            if tag == 1:
                country = value.decode("utf-8")
            elif tag == 2:
                cidr = dict(fields(value))
                if set(cidr) - {1, 2} or 1 not in cidr:
                    raise ValueError("Unexpected CIDR fields")
                address = ipaddress.ip_address(cidr[1])
                networks.append(str(ipaddress.ip_network((address, cidr.get(2, 0)), strict=False)))
            elif tag == 3:
                inverse = bool(value)
            else:
                raise ValueError("New GeoIP fields require explicit source export support")
        if country is None:
            raise ValueError("GeoIP country is missing")
        result.append({"country_code": country, "inverse_match": inverse, "cidrs": networks})
    if not result:
        raise ValueError("GeoIP dataset is empty")
    return result


def encode_varint(value):
    output = bytearray()
    while value >= 128:
        output.append((value & 127) | 128)
        value >>= 7
    output.append(value)
    return bytes(output)


def encode_field(tag, value):
    if isinstance(value, bytes):
        return encode_varint((tag << 3) | 2) + encode_varint(len(value)) + value
    return encode_varint(tag << 3) + encode_varint(value)


def encode_geoip(entries):
    output = bytearray()
    for entry in entries:
        country = entry["country_code"].encode("utf-8")
        encoded = bytearray(encode_field(1, country))
        for cidr in entry["cidrs"]:
            network = ipaddress.ip_network(cidr, strict=True)
            fields = encode_field(1, network.network_address.packed)
            if network.prefixlen:
                fields += encode_field(2, network.prefixlen)
            encoded.extend(encode_field(2, fields))
        if entry["inverse_match"]:
            encoded.extend(encode_field(3, 1))
        output.extend(encode_field(1, bytes(encoded)))
    return bytes(output)


def collect(root, notices, release_tag):
    if not re.fullmatch(r"[0-9A-Za-z._-]+", release_tag):
        raise ValueError("Invalid GeoData release tag")
    destination = root / "third-party/geodata"
    destination.mkdir(parents=True)
    release_url = "https://api.github.com/repos/MetaCubeX/meta-rules-dat/releases/tags/" + release_tag
    release = json.loads(fetch(release_url))
    asset = next(a for a in release["assets"] if a["name"] == "geoip.dat")
    digest = asset.get("digest", "")
    if not re.fullmatch(r"sha256:[a-f0-9]{64}", digest):
        raise ValueError("GeoIP input asset has no published SHA256")
    data = fetch(asset["browser_download_url"])
    if hashlib.sha256(data).hexdigest() != digest[7:]:
        raise ValueError("GeoIP input checksum mismatch")
    (destination / "geoip.dat").write_bytes(data)
    entries = editable_geoip(data)
    # Preserve original bytes, and verify the editable form represents every entry.
    rebuilt = encode_geoip(entries)
    if editable_geoip(rebuilt) != entries:
        raise ValueError("GeoIP editable form did not round-trip")
    (destination / "geoip.json").write_text(json.dumps(entries, indent=2) + "\n", encoding="utf-8")
    provenance = {"input_release_url": release_url, "input_release_id": release["id"], "input_asset": asset,
                  "input_sha256": digest[7:], "format_conversion_by": "ZenClash contributors, 2026-10-05",
                  "data_changes": "No CIDRs changed; original data preserved and represented in editable JSON.", "notices": []}
    notice_dir = notices / "geodata"
    notice_dir.mkdir(parents=True)
    files = {}
    for repo, paths in {"MetaCubeX/meta-rules-dat": ["LICENSE", "README.md", ".github/workflows/run.yml"],
                        "Loyalsoldier/geoip": ["LICENSE", "LICENSE-GPL", "README.md"]}.items():
        commit = json.loads(fetch("https://api.github.com/repos/" + repo + "/commits/master"))["sha"]
        for path in paths:
            url = "https://raw.githubusercontent.com/" + repo + "/" + commit + "/" + path
            content = fetch(url)
            relative = Path("geodata") / repo.replace("/", "-") / path
            output = notices / relative
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_bytes(content)
            files[relative.as_posix()] = hashlib.sha256(content).hexdigest()
            provenance["notices"].append({"repository": repo, "commit": commit, "path": path, "url": url})
    converter = root / "third-party/geo-converter"
    converter_checkout = root.parent / "geo-converter-checkout"
    subprocess.run(["git", "clone", "--depth=1", "https://github.com/MetaCubeX/geo.git", str(converter_checkout)], check=True)
    converter_commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=converter_checkout, text=True).strip()
    provenance["converter_commit"] = converter_commit
    shutil.copytree(converter_checkout, converter, ignore=shutil.ignore_patterns(".git"))
    subprocess.run(["go", "mod", "vendor"], cwd=converter, check=True)
    tool = destination / ("geo.exe" if __import__("os").name == "nt" else "geo")
    subprocess.run(["go", "build", "-mod=vendor", "-trimpath", "-o", str(tool), "./cmd/geo"], cwd=converter, check=True)
    output = destination / "geoip.metadb"
    subprocess.run([str(tool), "convert", "ip", "-i", "v2ray", "-o", "meta", "-f", str(output), str(destination / "geoip.dat")], check=True)
    tool.unlink()  # The archive contains the converter source and its dependencies.
    for path in converter.rglob("*"):
        if path.is_file() and re.match(r"(?i)^(license|licence|copying|copyright|notice)(?:[._-].*)?$", path.name):
            relative = Path("geodata/geo-converter") / path.relative_to(converter)
            target = notices / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, target)
            files[relative.as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
    attribution = notice_dir / "NOTICE.md"
    attribution.write_text(
        "# GeoData attribution\n\nThis product includes GeoLite2 data created by MaxMind, available from https://www.maxmind.com/.\n\n"
        "Country and supplemental CIDR data: Loyalsoldier/geoip and its listed contributors/sources; see its retained README and CC-BY-SA-4.0/GPL-3.0 license texts. "
        "Input GeoIP release: MetaCubeX/meta-rules-dat; its original licenses and build workflow are retained. "
        "Format conversion to geoip.metadb: ZenClash contributors on 2026-10-05 using the pinned MetaCubeX/geo converter. No CIDRs were edited. "
        "GeoData retains its upstream terms; it is not relabeled as ZenClash-owned code. SOURCE.json records exact inputs and converter commit.\n",
        encoding="utf-8")
    files["geodata/NOTICE.md"] = hashlib.sha256(attribution.read_bytes()).hexdigest()
    provenance["output_sha256"] = hashlib.sha256(output.read_bytes()).hexdigest()
    source_record = notice_dir / "SOURCE.json"
    source_record.write_text(json.dumps(provenance, indent=2) + "\n", encoding="utf-8")
    files["geodata/SOURCE.json"] = hashlib.sha256(source_record.read_bytes()).hexdigest()
    shutil.copytree(notice_dir, destination / "licenses")
    return output, provenance["output_sha256"], files


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rebuild-json", type=Path, required=True)
    parser.add_argument("--output-dat", type=Path, required=True)
    arguments = parser.parse_args()
    arguments.output_dat.write_bytes(encode_geoip(json.loads(arguments.rebuild_json.read_text(encoding="utf-8"))))
