<p align="center">
  <img src="platforms/macos/ZenClash.png" width="120" alt="ZenClash Logo">
</p>

<h1 align="center">ZenClash</h1>

<p align="center">
  A native Mihomo desktop client built with Rust and GPUI Kit<br>
  Manage profiles, switch proxies, and monitor traffic on macOS, Windows, and Linux.
</p>

<p align="center">
  <a href="README.md">简体中文</a> · English<br>
  <a href="https://github.com/HaiwenZhang/ZenClash/releases">Download</a> ·
  <a href="https://github.com/HaiwenZhang/ZenClash/issues">Report an issue</a> ·
  <a href="LICENSE">GPL-3.0</a>
</p>

> ZenClash is in early development. Feedback and contributions are welcome.

![ZenClash home page](docs/home_en.png)

## Features

- **Profiles and configuration**: Add subscriptions, import Clash/Mihomo YAML files, apply overrides, and back up or restore configurations.
  Local profiles, disabled overrides, traffic history, and local preferences remain available while the core is offline; operations requiring the core report connection failures.
  Edits made while the YAML editor is saving remain in the draft. Leaving the page does not cancel a submitted save.
- **Proxies and routing**: Switch proxy groups and nodes, test latency, and choose Rule, Global, or Direct mode.
  Proxy groups are paged eight at a time; the expanded group shows up to 24 nodes per page.
- **Desktop integration**: System proxy, TUN, launch at login, and quick controls from the tray.
- **Monitoring and diagnostics**: Live traffic, active connections, rules, logs, network diagnostics, and local usage history.
- **Native interface**: Simplified Chinese and English, with light, dark, and system appearance modes.
  The new desktop layout includes home area charts and metric trends, profile quota charts, connection and rule statistics, log details, and Settings section navigation. See the [UI implementation record](docs/development/ui-design-implementation.md) for current implementation and acceptance coverage. Home process traffic queries locally recorded samples from the last 24 hours; recording pauses or missing samples mean coverage may be incomplete.

## Download and Install

Download the package for your platform from [Releases](https://github.com/HaiwenZhang/ZenClash/releases):

| Platform | Architecture | Package |
| --- | --- | --- |
| macOS | Apple Silicon | `.dmg` |
| Windows | x86_64 | `.exe` |
| Ubuntu 24.04 and newer | amd64 | `.deb` |
| Fedora 44 / Rocky Linux 8 | x86_64 | `.rpm` |

Installers bundle Mihomo, so no separate core download is needed. Releases include `SHA256SUMS` to verify your download.

The macOS package is not yet notarized by Apple. See the [macOS installation guide](docs/installation/macos_en.md) for first-launch instructions.

## Quick Start

1. Open ZenClash and add a subscription URL or import a local YAML file in **Profiles**.
2. Select a profile on Home, then open **Proxies** to choose a node and test its latency.
3. Enable the system proxy or TUN as needed, then choose Rule, Global, or Direct mode.

TUN requires system permissions. The service mode under development has an in-app first-install and enable flow, but native acceptance on all three platforms is still pending. Check the documentation for your version; use the system proxy to get started.

Managed Mihomo startup reports an error and exits when service status, ownership, or authorization prevents startup, including when the service is missing and saved TUN is enabled. Resolve the service or configuration issue described in the error, then restart. A missing service with confirmed TUN-off still permits normal local startup; the app does not automatically fall back to bypass a service rejection.

## Development

You need the current Rust stable toolchain, your platform's native build tools, and a working Mihomo executable. On Linux, install dependencies with `sudo scripts/install_linux_build_deps.sh`.

The TUN service for all three platforms is being implemented according to the [development plan](docs/development/tun-service-plan.md). The in-app first-install and enable flow has initial integration and behavior tests. The TUN page now provides repair and uninstall confirmation controls, restoring the local core before maintenance; TUN can then be enabled again. Background preparation of ordinary core identity after direct Service startup is integrated; automatic capture restoration after repair, proxy preference persistence during maintenance, and UI synchronization are integrated, while the complete administrator maintenance flow and native acceptance on all three platforms remain unfinished, so the complete workflow is still in development. Packaging scripts for all three platforms now include the separate service artifact; see the [service packaging record](docs/development/tun-service-packaging.md) for deployment boundaries and validation commands.

Run from the repository root on macOS or Linux:

```sh
ZENCLASH_MIHOMO_BINARY=/absolute/path/to/mihomo \
  cargo run --locked -p zenclash-ui --bin zenclash
```

In Windows PowerShell, set the core path with `$env:ZENCLASH_MIHOMO_BINARY = 'C:\path\to\mihomo.exe'`, then run the same `cargo run` command.

Check formatting and run tests before submitting changes:

```sh
cargo fmt --all -- --check
cargo test --workspace --all-features --locked
```

See the [CI workflow](.github/workflows/ci.yml) for the full Clippy rules. Real-core integration tests are ignored by default and must be run explicitly with a core path configured.

Further reading: [packaging scripts](scripts) · [development and validation notes](docs/development) · [project guidelines](AGENTS.md) · [GPUI Kit migration and Windows acceptance](docs/development/gpui-kit-migration.md). The development notes and guidelines are primarily in Chinese.

## Data and Privacy

- Imported subscriptions and YAML source files are never rewritten in place.
- Profile indexes reject unsafe paths, duplicate records, and managed-file symlinks. Reads and writes enforce the same size limit, including backups.
- Mode changes and profile applications run serially, using the profile and override chain committed when execution starts.
- A mode change that conflicts with an enabled YAML override is rejected with guidance to edit or disable that override. If a profile application result is uncertain, explicitly reapply the recorded profile’s current contents.
- System proxies are released on the recorded network service. PAC replacement closes the old listener after native readback and preference persistence; failures restore state or retain listeners for recovery.
- If proxy release or core shutdown fails during quit, the app remains running and shows the error. Fix the system permissions or proxy state, then retry quitting.
- Traffic history stays local; you can disable it or change its retention period in Settings. At most 1,000,000 samples are retained, with the oldest observation timestamps evicted first. Normal exit awaits the final write; a failed write preserves the bounded pending queue for retry.
- The log buffer and persistence queue limit both entry counts and serialized bytes. Oversized logs show an error, and filtering and display data are prepared in the background.
- Cores started by ZenClash stop when the app exits normally.
- Ordinary Mihomo startup and restart use the checked configuration snapshot. Later source-file changes cannot replace that launch input; a failed restart check preserves the current core.
- Configurations, logs, and backups may contain subscription URLs or controller secrets. Redact them before sharing.

## Contributing

[Issues](https://github.com/HaiwenZhang/ZenClash/issues) and pull requests are welcome. When reporting a bug, include your OS version, ZenClash and Mihomo versions, steps to reproduce, and redacted logs.

## Acknowledgments and License

Thanks to [Mihomo](https://github.com/MetaCubeX/mihomo), [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui), and [GPUI Kit](https://github.com/longbridge/gpui-kit).

ZenClash is licensed under [GPL-3.0-only](LICENSE). Copyright © 2026 Haiwen Zhang.
