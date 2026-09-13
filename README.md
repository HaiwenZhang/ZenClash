<p align="center">
  <img src="platforms/macos/ZenClash.png" width="120" alt="ZenClash Logo">
</p>

<h1 align="center">ZenClash</h1>

<p align="center">
  基于 Rust 与 GPUI 的原生 Mihomo 桌面客户端<br>
  在 macOS、Windows 和 Linux 上管理订阅、切换节点、查看流量。
</p>

<p align="center">
  简体中文 · <a href="README_en.md">English</a><br>
  <a href="https://github.com/HaiwenZhang/ZenClash/releases">下载</a> ·
  <a href="https://github.com/HaiwenZhang/ZenClash/issues">问题反馈</a> ·
  <a href="LICENSE">GPL-3.0</a>
</p>

> 项目仍在早期开发中，欢迎试用与反馈。

![ZenClash 首页](docs/home.png)

## 特点

- **订阅与配置**：添加在线订阅、导入 Clash/Mihomo YAML，支持配置覆写与备份恢复。
- **节点与路由**：切换代理组和节点、测试延迟，支持规则、全局和直连模式。
- **系统集成**：系统代理、TUN、开机启动和托盘快捷操作。
- **流量与排障**：实时流量、活动连接、规则、日志、网络诊断与本地用量统计。
- **远程管理**：连接多个 Mihomo 控制器；macOS 支持按 Wi-Fi 切换本地配置。
- **原生界面**：支持简体中文、英文，以及浅色、深色和跟随系统主题。

## 下载与安装

从 [Releases](https://github.com/HaiwenZhang/ZenClash/releases) 下载对应平台的安装包：

| 平台 | 架构 | 安装包 |
| --- | --- | --- |
| macOS | Apple Silicon | `.dmg` |
| Windows | x86_64 | `.exe` |
| Ubuntu 24.04 及以上 | amd64 | `.deb` |
| Fedora 44 / Rocky Linux 8 | x86_64 | `.rpm` |

安装包已内置 Mihomo，无需另行下载内核。Release 提供 `SHA256SUMS` 用于校验下载文件。

macOS 安装包尚未经过 Apple 公证，首次打开请参考 [macOS 安装指南](docs/installation/macos.md)。

## 快速开始

1. 打开 ZenClash，在「订阅管理」中添加订阅链接或导入本地 YAML。
2. 返回首页选择配置，再到「代理组」选择节点并测速。
3. 按需启用系统代理或 TUN，选择规则、全局或直连模式。

TUN 需要系统权限；Windows 暂不支持在应用内自动获取 TUN 所需的管理员权限，可先使用系统代理。

## 开发

准备当前 Rust stable 工具链、对应平台的原生构建工具，以及一个可执行的 Mihomo 内核。Linux 依赖可通过 `sudo scripts/install_linux_build_deps.sh` 安装。

在仓库根目录运行（macOS / Linux）：

```sh
ZENCLASH_MIHOMO_BINARY=/absolute/path/to/mihomo \
  cargo run --locked -p zenclash-ui --bin zenclash
```

Windows PowerShell 先用 `$env:ZENCLASH_MIHOMO_BINARY = 'C:\path\to\mihomo.exe'` 设置内核路径，再运行相同的 `cargo run` 命令。

提交前运行格式检查与测试：

```sh
cargo fmt --all -- --check
cargo test --workspace --all-features --locked
```

完整 Clippy 规则见 [CI 工作流](.github/workflows/ci.yml)。真实内核集成测试默认忽略，需要提供内核路径后显式运行。

更多开发资料：[打包脚本](scripts) · [开发与验收文档](docs/development) · [项目规约](AGENTS.md)。

## 数据与隐私

- 导入的订阅与 YAML 源文件不会被原地改写。
- 流量历史保存在本地，可在设置中关闭或调整保留时间。
- ZenClash 启动的内核会随应用正常退出而停止。
- 配置、日志和备份可能包含订阅地址或控制器密钥，请在分享前脱敏。

## 参与贡献

欢迎提交 [Issue](https://github.com/HaiwenZhang/ZenClash/issues) 和 Pull Request。报告问题时，请附上系统版本、ZenClash 与 Mihomo 版本、复现步骤及脱敏后的日志。

## 致谢与许可证

感谢 [Mihomo](https://github.com/MetaCubeX/mihomo)、[GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) 和 [GPUI Component](https://github.com/longbridge/gpui-component)。

ZenClash 采用 [GPL-3.0-only](LICENSE) 许可证。Copyright © 2026 Haiwen Zhang。
