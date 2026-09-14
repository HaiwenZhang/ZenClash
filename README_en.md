<p align="center">
  <img src="platforms/macos/ZenClash.png" width="120" alt="ZenClash Logo">
</p>

<h1 align="center">ZenClash</h1>

<p align="center">
  A native Mihomo desktop client built with Rust and GPUI<br>
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
- **Proxies and routing**: Switch proxy groups and nodes, test latency, and choose Rule, Global, or Direct mode.
- **Desktop integration**: System proxy, TUN, launch at login, and quick controls from the tray.
- **Monitoring and diagnostics**: Live traffic, active connections, rules, logs, network diagnostics, and local usage history.
- **Native interface**: Simplified Chinese and English, with light, dark, and system appearance modes.

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

TUN requires system permissions. On Windows, ZenClash cannot yet request the administrator permissions needed for TUN from within the app; use the system proxy to get started.

## Development

You need the current Rust stable toolchain, your platform's native build tools, and a working Mihomo executable. On Linux, install dependencies with `sudo scripts/install_linux_build_deps.sh`.

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

Further reading: [packaging scripts](scripts) · [development and validation notes](docs/development) · [project guidelines](AGENTS.md). The development notes and guidelines are primarily in Chinese.

## Data and Privacy

- Imported subscriptions and YAML source files are never rewritten in place.
- Traffic history stays local; you can disable it or change its retention period in Settings.
- Cores started by ZenClash stop when the app exits normally.
- Configurations, logs, and backups may contain subscription URLs or controller secrets. Redact them before sharing.

## Contributing

[Issues](https://github.com/HaiwenZhang/ZenClash/issues) and pull requests are welcome. When reporting a bug, include your OS version, ZenClash and Mihomo versions, steps to reproduce, and redacted logs.

## Acknowledgments and License

Thanks to [Mihomo](https://github.com/MetaCubeX/mihomo), [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui), and [GPUI Component](https://github.com/longbridge/gpui-component).

ZenClash is licensed under [GPL-3.0-only](LICENSE). Copyright © 2026 Haiwen Zhang.
