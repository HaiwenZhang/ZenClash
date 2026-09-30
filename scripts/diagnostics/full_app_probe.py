#!/usr/bin/env python3
"""Prepare and optionally run an isolated macOS full-app probe. Standard library only."""
import argparse
import io
import json
import os
from pathlib import Path
import platform
import plistlib
import shutil
import signal
import socket
import subprocess
import sys
import tarfile
import tempfile
import time

REPOSITORY = Path(__file__).resolve().parents[2]
PAGES = {"proxies": "Proxies", "profiles": "Profiles", "override": "Override",
         "traffic": "Traffic", "rules": "Rules", "connections": "Connections",
         "logs": "Logs", "mihomo": "Mihomo"}
DATA_ROOTS = {
    "process/resources.rs": 'home.join("Library/Application Support/ZenClash")',
    "controlled_config/storage.rs": 'home()?.join("Library/Application Support/ZenClash")',
    "preferences.rs": 'home()?.join("Library/Application Support/ZenClash")',
    "yaml_overrides.rs": 'home.join("Library/Application Support/ZenClash")',
    "profiles/store.rs": 'home_dir()?.join("Library/Application Support/ZenClash/profiles")',
}


def replace_once(path, before, after):
    payload = path.read_text()
    if payload.count(before) != 1:
        raise ValueError(f"source changed: expected one injection point in {path}")
    path.write_text(payload.replace(before, after))


def ports():
    # Reserve both together to avoid selecting the same port twice.
    with socket.socket() as controller, socket.socket() as mixed:
        controller.bind(("127.0.0.1", 0))
        mixed.bind(("127.0.0.1", 0))
        return controller.getsockname()[1], mixed.getsockname()[1]


def prepare(args, root):
    source = root / "source"
    source.mkdir()
    if args.baseline:
        archive = subprocess.check_output(["git", "archive", "HEAD", "crates", "platforms",
                                           "Cargo.toml", "Cargo.lock"], cwd=REPOSITORY)
        with tarfile.open(fileobj=io.BytesIO(archive)) as contents:
            contents.extractall(source, filter="data")
    else:
        for name in ("crates", "platforms"):
            shutil.copytree(REPOSITORY / name, source / name)
        for name in ("Cargo.toml", "Cargo.lock"):
            shutil.copy2(REPOSITORY / name, source / name)
    data = root / "data"
    for filename, expression in DATA_ROOTS.items():
        destination = data / "profiles" if filename == "profiles/store.rs" else data
        replace_once(source / "crates/zenclash-core/src" / filename, expression,
                     f"std::path::PathBuf::from({json.dumps(str(destination))})")
    ui = source / "crates/zenclash-ui"
    replace_once(ui / "Cargo.toml", 'name = "zenclash"', 'name = "zenclash-full-app-probe"')
    replace_once(ui / "src/app.rs", "mod bootstrap;",
                 "mod bootstrap;\npub(crate) mod diagnostic_probe;")
    replace_once(ui / "src/app/bootstrap.rs", '                #[cfg(target_os = "macos")]\n                keep_main_window_alive_when_closed',
                 '                app.update(cx, |app, cx| app.start_full_app_probe(cx));\n'
                 '                #[cfg(target_os = "macos")]\n                keep_main_window_alive_when_closed')
    for method in ("hide_main_window", "show_main_window"):
        path = ui / "src/app/tray/window.rs"
        before, after = f"pub(super) fn {method}", f"pub(in crate::app) fn {method}"
        if before in path.read_text():
            replace_once(path, before, after)
        elif path.read_text().count(after) != 1:
            raise ValueError(f"source changed: window method {method}")
    replace_once(ui / "src/pages/runtime/view.rs", "        let theme = cx.theme().clone();",
                 "        crate::app::diagnostic_probe::record_render();\n"
                 "        let theme = cx.theme().clone();")
    driver = Path(__file__).with_suffix(".rs").read_text()
    for placeholder, value in (("IDLE_SECONDS", args.idle_seconds),
                               ("INTERACTIVE_SECONDS", args.interactive_seconds),
                               ("NODES", args.nodes), ("RULES", args.rules),
                               ("INTERACTIVE_PAGE", PAGES[args.interactive_page])):
        driver = driver.replace(f"@{placeholder}@", str(value))
    (ui / "src/app/diagnostic_probe.rs").write_text(driver)
    controller, mixed = ports()
    lines = [f"mixed-port: {mixed}", "bind-address: 127.0.0.1", "allow-lan: false",
             f"external-controller: 127.0.0.1:{controller}", "secret: ''", "mode: rule",
             "log-level: info", "tun: {enable: false}", "dns: {enable: false}", "proxies:"]
    lines.extend(f"  - {{name: node-{i:05}, type: direct}}" for i in range(args.nodes))
    lines.append("proxy-groups:")
    for i in range(20):
        lines.extend([f"  - name: group-{i:02}", "    type: select", "    proxies:",
                      *(f"      - node-{n:05}" for n in range(args.nodes))])
    lines.append("rules:")
    lines.extend(f"  - DOMAIN,fixture-{i:06}.example,DIRECT" for i in range(args.rules))
    lines.append("  - MATCH,DIRECT")
    fixture = root / "fixture.yaml"
    fixture.write_text("\n".join(lines) + "\n")
    files = data / "profiles/files"
    files.mkdir(parents=True)
    shutil.copy2(fixture, files / "fixture.yaml")
    (data / "profiles/profiles.json").write_text(json.dumps({"active": "fixture", "profiles": [{
        "id": "fixture", "name": "Probe fixture", "file_name": "fixture.yaml",
        "source": {"kind": "local", "original_path": str(fixture)},
        "updated_at": 0, "size_bytes": fixture.stat().st_size}]}))
    (data / "preferences.json").write_text(json.dumps({"version": 1,
        "system_proxy_enabled": False, "system_proxy_ownership": None,
        "appearance": "light", "traffic_history_enabled": True}))
    manifest = {"root": str(root), "source": str(source), "nodes": args.nodes,
                "revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPOSITORY, text=True).strip(),
                "rules": args.rules + 1, "build_mode": args.build_mode,
                "baseline": args.baseline, "controller_port": controller,
                "idle_seconds": args.idle_seconds, "interactive_seconds": args.interactive_seconds,
                "platform": platform.platform(), "architecture": platform.machine(),
                "window": "1280x820 (production default)",
                "metrics": "navigation call, 50ms timer gaps, RuntimePage renders, separate RSS"}
    (root / "probe.json").write_text(json.dumps(manifest, indent=2))
    return manifest


def descendants(pid):
    found = set()
    pending = [pid]
    while pending:
        result = subprocess.run(["pgrep", "-P", str(pending.pop())], capture_output=True, text=True)
        children = {int(value) for value in result.stdout.split()}
        children -= found
        found.update(children)
        pending.extend(children)
    return found


def identity(pid):
    result = subprocess.run(["ps", "-p", str(pid), "-o", "lstart=,comm="], capture_output=True, text=True)
    return result.stdout.strip() if result.returncode == 0 else None


def rss(pid):
    result = subprocess.run(["ps", "-p", str(pid), "-o", "rss="], capture_output=True, text=True)
    return int(result.stdout.strip()) if result.stdout.strip() else None


def run(args, manifest):
    root = Path(manifest["root"])
    binary = args.core_binary.resolve()
    if not binary.is_file():
        raise ValueError(f"Mihomo binary absent: {binary}")
    target = REPOSITORY / "target"
    cargo = ["cargo"] + ([f"+{args.cargo_toolchain}"] if args.cargo_toolchain else [])
    command = cargo + ["build", "--manifest-path", str(root / "source/Cargo.toml"),
                       "-p", "zenclash-ui", "--bin", "zenclash-full-app-probe", "--locked", "--offline"]
    if args.build_mode == "release":
        command.append("--release")
    environment = os.environ.copy()
    environment["CARGO_TARGET_DIR"] = str(target)
    with (root / "build.log").open("w") as log:
        subprocess.run(command, env=environment, stdout=log, stderr=subprocess.STDOUT, check=True)
    executable = target / args.build_mode / "zenclash-full-app-probe"
    # Keep an independent executable, so another probe build cannot replace a running instance.
    bundle = root / "ZenClashProbe.app/Contents"
    (bundle / "MacOS").mkdir(parents=True)
    installed = bundle / "MacOS/zenclash-full-app-probe"
    shutil.copy2(executable, installed)
    with (bundle / "Info.plist").open("wb") as metadata:
        plistlib.dump({"CFBundleExecutable": installed.name, "CFBundleName": "ZenClash Probe",
                       "CFBundleIdentifier": "org.zenclash.diagnostic." + root.name,
                       "CFBundlePackageType": "APPL", "CFBundleVersion": "1",
                       "NSHighResolutionCapable": True}, metadata)
    for key in list(environment):
        if key.startswith("ZENCLASH_"):
            del environment[key]
    environment.update(ZENCLASH_CORE="mihomo", ZENCLASH_CORE_BINARY=str(binary),
                       ZENCLASH_CONFIG=str(root / "fixture.yaml"),
                       ZENCLASH_CORE_HOME=str(root / "core-home"))
    samples, owned = [], {}
    timeout = 35 + args.idle_seconds + args.interactive_seconds
    started = time.monotonic()
    with (root / "run.log").open("w") as log:
        process = subprocess.Popen([str(installed)], env=environment, stdout=log, stderr=log)
        try:
            while process.poll() is None:
                children = descendants(process.pid)
                for pid in children:
                    observed = identity(pid)
                    if observed:
                        owned[pid] = observed
                core_pid = next(iter(children), None) if len(children) == 1 else None
                samples.append({"seconds": round(time.monotonic() - started, 3),
                                "ui_rss_kib": rss(process.pid),
                                "core_rss_kib": rss(core_pid) if core_pid else None})
                if time.monotonic() - started > timeout:
                    raise TimeoutError("probe did not finish its normal quit path")
                time.sleep(0.5)
        finally:
            for pid in descendants(process.pid):
                observed = identity(pid)
                if observed:
                    owned[pid] = observed
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            # Inspect only descendants observed under this diagnostic process.
            remaining = [pid for pid, observed in owned.items() if identity(pid) == observed]
            for pid in remaining:
                os.kill(pid, signal.SIGTERM)
    events = [json.loads(line.split("ZENCLASH_PROBE ", 1)[1])
              for line in (root / "run.log").read_text().splitlines() if "ZENCLASH_PROBE " in line]
    loaded = any(event["event"] == "fixture_loaded" and event["loaded"] for event in events)
    report = {**manifest, "exit_code": process.returncode, "fixture_loaded": loaded,
              "core_version": subprocess.check_output([str(binary), "-v"], text=True).splitlines()[0],
              "samples": samples, "events": events, "remaining_owned_pids": remaining,
              "passed": process.returncode == 0 and loaded and not remaining
                        and sum(event["event"] == "navigation" for event in events) == 20
                        and any(event["event"] == "library" and event["profiles"] == 1
                                and event["active"] == "fixture" for event in events)
                        and any(event["event"] == "normal_quit" for event in events)}
    (root / "result.json").write_text(json.dumps(report, indent=2))
    return report


def arguments(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--nodes", type=int, default=500)
    parser.add_argument("--rules", type=int, default=10_000)
    parser.add_argument("--idle-seconds", type=int, default=180)
    parser.add_argument("--interactive-seconds", type=int, default=0)
    parser.add_argument("--interactive-page", choices=PAGES, default="rules")
    parser.add_argument("--build-mode", choices=("debug", "release"), default="release")
    parser.add_argument("--cargo-toolchain")
    parser.add_argument("--baseline", action="store_true", help="probe tracked HEAD for comparison")
    parser.add_argument("--run", action="store_true", help="build offline, run, verify shutdown")
    parser.add_argument("--core-binary", type=Path,
                        default=Path("/Applications/ZenClash.app/Contents/Resources/mihomo"))
    args = parser.parse_args(argv)
    if not 1 <= args.nodes <= 10_000 or not 0 <= args.rules <= 100_000:
        parser.error("nodes must be 1..10000 and rules 0..100000")
    if not 0 <= args.idle_seconds <= 3600 or not 0 <= args.interactive_seconds <= 3600:
        parser.error("durations must be 0..3600 seconds")
    return args


def main():
    args = arguments()
    if platform.system() != "Darwin":
        raise SystemExit("full-app probe currently supports macOS only")
    root = Path(tempfile.mkdtemp(prefix="zenclash-full-app-"))
    manifest = prepare(args, root)
    print(json.dumps(manifest), flush=True)
    if args.run:
        result = run(args, manifest)
        print(json.dumps({"result": str(root / "result.json"), "passed": result["passed"]}), flush=True)
        return 0 if result["passed"] else 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
