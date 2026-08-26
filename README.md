# ZenClash

ZenClash is a native Clash-compatible client implemented in Rust with GPUI and
gpui-component. Mihomo is the default production core; meow-rs is an explicit
experimental alternative with capability-aware UI and restart-based full
configuration transactions. The workspace currently contains:

- `zenclash-core`: typed Mihomo HTTP/WebSocket APIs, managed core process,
  macOS system proxy integration, traffic/log monitors, and real integration
  tests.
- `zenclash-ui`: GPUI native window, Clash Party-style card sidebar, proxy
  selection and latency tests, connections, rules, providers, runtime settings,
  TUN controls, logs, real SQLite-backed traffic history and rankings, local
  profile switching, ordered YAML overrides, Sub-Store connectivity, and a
  native status-bar traffic icon.

## Run against the supplied real profile

Use a real Mihomo executable. ZenClash will start it with
`examples/19facdf022b.yaml` when its controller is not already available.

```sh
ZENCLASH_MIHOMO_BINARY=/absolute/path/to/mihomo \
  cargo run -p zenclash-ui --bin zenclash
```

To connect to an already-running controller instead:

```sh
ZENCLASH_CONTROLLER=http://127.0.0.1:9090 \
  ZENCLASH_CONFIG="$PWD/examples/19facdf022b.yaml" \
  cargo run -p zenclash-ui --bin zenclash
```

Optional environment variables are `ZENCLASH_SECRET`, `ZENCLASH_CONFIG`,
`ZENCLASH_CORE_BINARY`, `ZENCLASH_CORE_HOME`, `ZENCLASH_MIHOMO_HOME`, and
`ZENCLASH_NETWORK_SERVICE`.

To run the downloaded `examples/meow-rs` source instead, build it and select it
explicitly. ZenClash never silently changes between cores:

```sh
cargo build --manifest-path examples/meow-rs/Cargo.toml -p meow-app
ZENCLASH_CORE=meow-rs \
ZENCLASH_MEOW_BINARY="$PWD/examples/meow-rs/target/debug/meow" \
  cargo run -p zenclash-ui --bin zenclash
```

The same selection is available under Settings and takes effect after restart.
If the selected binary is absent or fails its real `/version` readiness check,
startup fails clearly. An external controller is used only when
`ZENCLASH_CONTROLLER` is set explicitly.

The profile page can switch to another local YAML through the native file
picker. The override page imports individual YAML files or the immediate YAML
children of a directory into a private managed store. Enablement and order are
persisted, later mappings win recursively, and the same final payload is used
for startup, profile switches, settings changes, tray actions, editing, and
backup restore. Imported source files are never rewritten.

ZenClash connects to an existing Sub-Store backend/frontend at
`http://127.0.0.1:38324` and `http://127.0.0.1:14122` by default. Override these
with `ZENCLASH_SUBSTORE_URL` and `ZENCLASH_SUBSTORE_FRONTEND_URL`.

## Real Mihomo integration test

The integration test never starts a mock controller. It launches the supplied
Mihomo binary, overrides only the controller address to isolate the test, loads
the real profile, and verifies version, runtime config, proxies, group switching,
rules, providers, connections, the traffic WebSocket, and persistent ordered
YAML overrides across a settings update.

```sh
ZENCLASH_MIHOMO_BINARY=/absolute/path/to/mihomo \
  cargo test -p zenclash-core --test real_mihomo -- --ignored --nocapture
```

If the profile needs GeoIP data, point `ZENCLASH_INTEGRATION_HOME` at a Mihomo
home directory that already contains the downloaded data or allow Mihomo to
download it during the test.

## Real meow-rs integration test

This test also uses no mock server. It starts the actual meow-rs process with
the supplied Clash YAML, reads version/proxy/connection APIs, writes a real
controlled setting, restarts the process, verifies the resulting mode from the
controller, and sends an HTTP request through its real Mixed listener:

```sh
ZENCLASH_MEOW_BINARY="$PWD/examples/meow-rs/target/debug/meow" \
ZENCLASH_CONFIG="$PWD/examples/19facdf022b.yaml" \
  cargo test -p zenclash-core --test real_meow -- --ignored --nocapture
```

## Build the macOS app bundle

```sh
scripts/build_macos_app.sh
open target/ZenClash.app
```

When `ZENCLASH_MIHOMO_BINARY` is not set, the script downloads the pinned
official Apple Silicon Mihomo release and requires its GitHub SHA-256 digest to
match before building. The bundle contains the verified binary and profile
under `Contents/Resources`; runtime data is stored in
`~/Library/Application Support/ZenClash/mihomo`. Set
`ZENCLASH_MIHOMO_BINARY=/absolute/path/to/mihomo` only to override the bundled
core deliberately.

## Release installers

Every installer bundles the pinned real Mihomo release, so normal installation
does not depend on a first-run download. The experimental meow-rs core is not
silently substituted and is currently supplied separately through
`ZENCLASH_MEOW_BINARY`. Local build scripts use
`examples/19facdf022b.yaml` by default, while public CI packages use the safe
bootstrap profile described below. The platform build entry points are:

```sh
# Apple Silicon macOS (.dmg)
scripts/build_macos_package.sh 0.1.0 dist

# Ubuntu 22.04 or newer (.deb), run on Ubuntu 22.04
sudo scripts/install_linux_build_deps.sh
scripts/build_deb_package.sh 0.1.0 dist

# Fedora / Rocky Linux (.rpm), run inside the target distribution
scripts/install_linux_build_deps.sh
ZENCLASH_PACKAGE_FLAVOR=fedora44 \
  scripts/build_rpm_package.sh 0.1.0 dist
```

All four platform scripts download the pinned official Mihomo release when no
binary override is supplied, verify the published SHA-256 digest, execute its
version check, and place it inside the installer. Set `MIHOMO_VERSION=vX.Y.Z`
to build with another official tag. A supplied `ZENCLASH_MIHOMO_BINARY` must
already be an executable for the target platform and is never silently
replaced by a download.

On Windows, run the following from PowerShell with Rust and Inno Setup 6
installed:

```powershell
scripts/build_windows_installer.ps1 -Version 0.1.0 -OutputDir dist
```

Before publishing, configure the repository secret `ZENCLASH_TEST_PROFILE_URL`
with a private URL that downloads the real Clash profile used by integration
tests. The downloaded profile is kept in the runner's temporary directory and
is never uploaded as an artifact. Public installers instead contain
`platforms/common/default.yaml`, a functional direct-only bootstrap profile;
users add their own online subscription or local YAML in subscription
management.

Pushing a tag whose value matches the workspace version, for example `v0.1.0`,
starts `.github/workflows/release.yml`. The workflow runs formatting, Clippy,
workspace tests, and the ignored real-Mihomo integration test before building:

- Windows Server 2022 x64 Inno Setup installer;
- macOS 15 Apple Silicon DMG;
- Ubuntu 22.04-baseline amd64 DEB for Ubuntu 22.04 and newer;
- Fedora 42, 43, and 44 x86_64 RPMs (the releases covering the latest two
  years at the time this matrix was added);
- Rocky Linux 8 and 9 x86_64 RPMs.

The release job publishes SHA-256 checksums and, for public repositories,
GitHub artifact attestations. Without `APPLE_SIGNING_IDENTITY`, the macOS app is
ad-hoc signed and is not notarized; set that environment variable on a trusted
release runner when distributing a Developer ID-signed build.
