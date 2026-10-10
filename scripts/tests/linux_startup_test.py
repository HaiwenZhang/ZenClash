#!/usr/bin/env python3
"""Exercise native Linux startup with a bundled core and a visible window.

Usage: python3 scripts/tests/linux_startup_test.py /path/to/usr/bin/zenclash
Requires a running Hyprland desktop and an executable with its packaged resources.
"""

import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--gtk-unavailable", action="store_true",
                        help="verify a failed GTK display connection does not terminate the app")
    arguments = parser.parse_args()
    binary = arguments.binary.resolve(strict=True)
    # Mihomo's Unix socket must fit the kernel's 108-byte pathname limit.
    with tempfile.TemporaryDirectory(prefix="zc-test-", dir="/tmp") as directory:
        root = Path(directory)
        environment = os.environ.copy()
        for name in ("ZENCLASH_CONTROLLER", "ZENCLASH_CORE", "ZENCLASH_PROFILE"):
            environment.pop(name, None)
        environment.update(
            XDG_DATA_HOME=str(root / "data"),
            RUST_BACKTRACE="1",
            RUST_LOG="zenclash=info,zenclash_core=info,zenclash_ui=info",
        )
        if arguments.gtk_unavailable:
            environment["GDK_BACKEND"] = "zenclash-test-unavailable"
        log_path = root / "startup.log"
        started = time.monotonic()
        with log_path.open("w") as log:
            process = subprocess.Popen(
                [str(binary)], cwd=root, env=environment, stdout=log,
                stderr=subprocess.STDOUT, start_new_session=True,
            )
            try:
                try:
                    process.wait(timeout=12)
                except subprocess.TimeoutExpired:
                    clients = subprocess.run(
                        ["hyprctl", "clients", "-j"], capture_output=True,
                        text=True, check=True,
                    )
                    windows = json.loads(clients.stdout)
                    if not any(window.get("pid") == process.pid and window.get("mapped")
                               for window in windows):
                        raise AssertionError("startup did not produce a visible window")
                    startup_log = log_path.read_text()
                    if "managed core is ready" not in startup_log:
                        raise AssertionError("core startup did not finish; tray creation was not exercised")
                    if arguments.gtk_unavailable:
                        if "Failed to initialize the native tray" not in startup_log:
                            raise AssertionError("the GTK initialization failure was not exercised")
                    else:
                        if "failed to create native traffic tray icon" in startup_log:
                            raise AssertionError("the tray could not be created")
                        bus = subprocess.run(
                            ["busctl", "--user", "--no-legend", "--no-pager", "list"],
                            capture_output=True, text=True, check=True,
                        )
                        owners = {
                            fields[0]: fields[1] for line in bus.stdout.splitlines()
                            if len(fields := line.split()) >= 2
                        }
                        registered = subprocess.run(
                            ["busctl", "--user", "--json=short", "get-property",
                             "org.kde.StatusNotifierWatcher", "/StatusNotifierWatcher",
                             "org.kde.StatusNotifierWatcher", "RegisteredStatusNotifierItems"],
                            capture_output=True, text=True, check=True,
                        )
                        items = json.loads(registered.stdout)["data"]
                        # AppIndicator registers a unique bus name and object path,
                        # rather than owning a named StatusNotifierItem service.
                        if not any(owners.get(item.split("/", 1)[0]) == str(process.pid)
                                   for item in items):
                            raise AssertionError("the tray event loop did not register its D-Bus item")
                    print("PASS: managed core ready, tray startup checked, main window visible after 12s")
                    return
                raise AssertionError(
                    f"startup exited after {time.monotonic()-started:.2f}s "
                    f"with status {process.returncode}"
                )
            except (AssertionError, subprocess.CalledProcessError):
                print(log_path.read_text())
                raise
            finally:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()


if __name__ == "__main__":
    main()
