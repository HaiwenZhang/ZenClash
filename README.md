<p align="center">
  <img src="platforms/macos/ZenClash.png" width="120" alt="ZenClash Logo">
</p>

<h1 align="center">ZenClash</h1>

<p align="center">
  基于 Rust 与 GPUI Kit 的原生 Mihomo 桌面客户端<br>
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
  内核离线时仍可管理本地配置、禁用的覆写、流量历史和本地偏好；需要内核的应用操作会报告连接失败。
  YAML 编辑器保存期间继续编辑会保留新草稿，切换页面不会取消已提交的保存。
- **节点与路由**：切换代理组和节点、测试延迟，支持规则、全局和直连模式。
  代理组每页最多显示 8 组，展开组的节点每页最多显示 24 个。
- **系统集成**：系统代理、TUN、开机启动和托盘快捷操作。
- **流量与排障**：实时流量、活动连接、规则、日志、网络诊断与本地用量统计。
- **原生界面**：支持简体中文、英文，以及浅色、深色和跟随系统主题。
  新版桌面布局包含首页面积图与指标曲线、订阅额度图、连接与规则统计、日志详情和设置分区导航；当前实施与验收范围见 [UI 实现记录](docs/development/ui-design-implementation.md)。首页进程流量查询最近 24 小时的本地已记录样本，记录关闭或缺失时不代表完整覆盖。

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

TUN 需要系统权限。开发中的服务模式已接通应用内首次安装与开启入口，但三平台原生验收尚未完成；使用前请核对所用版本的说明，需要稳定代理时可先使用系统代理。

托管 Mihomo 启动因服务状态、占用或授权被拒绝时会报告原因并退出，服务缺失且已保存 TUN 开启时也如此。处理错误提示中的服务或配置问题后重新启动。服务缺失且确认 TUN 关闭时仍可正常启动本地内核；应用不会自动回退来绕过服务拒绝。

## 开发

准备当前 Rust stable 工具链、对应平台的原生构建工具，以及一个可执行的 Mihomo 内核。Linux 依赖可通过 `sudo scripts/install_linux_build_deps.sh` 安装。

三平台 TUN 服务正在按 [开发计划](docs/development/tun-service-plan.md) 实施。应用内首次安装与开启已有阶段接线和行为测试；TUN 页新增修复、卸载确认入口，维护前恢复本地内核，维护后可重新开启 TUN。直接以 Service 启动时已接入后台普通内核身份准备；修复后的自动捕获恢复、维护期间代理偏好保存和界面同步已有接线，完整管理员维护链路及三平台实机验收尚未完成，完整工作流仍在开发。三平台打包脚本已增加独立服务产物；其部署边界与验证入口见 [服务打包记录](docs/development/tun-service-packaging.md)。

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

本轮实机测试的范围、问题修复和未验证事项见 [Windows 验收记录（2026-10-04）](docs/development/windows-acceptance-2026-10-04.md)。

更多开发资料：[打包脚本](scripts) · [开发与验收文档](docs/development) · [项目规约](AGENTS.md) · [GPUI Kit 迁移与 Windows 验收](docs/development/gpui-kit-migration.md)。

## 数据与隐私

- 导入的订阅与 YAML 源文件不会被原地改写。
- 配置索引拒绝不安全路径、重复记录和托管文件符号链接；读写均限制索引大小，备份沿用相同校验。
- 模式切换与配置应用串行执行，并使用执行时已提交的配置及覆写链。
- 启用的 YAML 覆写固定了不同模式时，模式切换会拒绝冲突并提示修改或停用该覆写。配置应用结果无法确认时，可明确重新应用已记录订阅的当前内容。
- 系统代理按接管时记录的网络服务释放；PAC 替换完成原生状态回读和偏好保存后才关闭旧服务，失败时恢复或保留待恢复服务。
- 退出时若代理释放或内核停止失败，应用保持运行并显示错误；修复系统权限或代理状态后可重试退出。
- 流量历史保存在本地，可在设置中关闭或调整保留时间；最多保留 1,000,000 条样本，超限时按观测时间淘汰最旧记录。正常退出会等待最后一批写入，写入失败时保留有界的待写队列并允许重试。
- 日志缓冲与落盘队列同时限制条目数和序列化字节数；超大日志条目会显示错误，日志筛选和展示数据在后台准备。
- ZenClash 启动的内核会随应用正常退出而停止。
- 普通 Mihomo 启动和重启使用已检查的配置快照，源文件随后变化不会替换本次启动内容；重启检查失败保留原内核。
- 配置、日志和备份可能包含订阅地址或控制器密钥，请在分享前脱敏。

## 参与贡献

欢迎提交 [Issue](https://github.com/HaiwenZhang/ZenClash/issues) 和 Pull Request。报告问题时，请附上系统版本、ZenClash 与 Mihomo 版本、复现步骤及脱敏后的日志。

## 致谢与许可证

感谢 [Mihomo](https://github.com/MetaCubeX/mihomo)、[GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) 和 [GPUI Kit](https://github.com/longbridge/gpui-kit)。

ZenClash 采用 [GPL-3.0-only](LICENSE) 许可证。Copyright © 2026 Haiwen Zhang。


`crates/zenclash-service` Fork 自 [clash-verge-rev/clash-verge-service-ipc](https://github.com/clash-verge-rev/clash-verge-service-ipc) 2.7.5 的本地源码副本，保留上游作者 Tunglies 及其他贡献者的声明和完整 GPL 第 3 版许可证；ZenClash 适配日期为 2026-10-04。来源、修改内容和接入状态见 [服务 README](crates/zenclash-service/README.md)、[修改声明](crates/zenclash-service/NOTICE.md) 和 [原始许可证](crates/zenclash-service/LICENSE)。

分发包含该 Fork 的二进制时，须保留许可证和修改声明，并按 GPL 第 6 节提供与发布版本一致的完整对应源码，包含 ZenClash 的修改和必要构建/安装文件；只链接上游仓库不足以代替对应源码。具体发布操作见 [GPL 分发与对应源码](docs/development/gpl-distribution.md)。主应用打包携带根和 Fork 的许可及修改声明；发布流程从同一干净提交导出对应源码，纳入 Rust/Go 依赖，并将源码附件与安装包一起提供。流程文件已接入；2026-10-05 已在本地生成阶段 Windows 安装包和同版完整对应源码，源码解压后的哈希、锁定依赖离线解析及 GUI/服务编译检查通过。交付文件位于 `dist/gpl-2026-10-05/`；尚未发布正式 Release 或完成三平台实机/TUN 验收。
