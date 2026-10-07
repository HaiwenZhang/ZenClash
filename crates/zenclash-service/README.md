> 2026-10-08：应用侧集成已迁入 `zenclash-core::service`；后文旧日期为阶段记录。

# ZenClash Service

ZenClash 的跨平台特权服务，Fork 自 [clash-verge-rev/clash-verge-service-ipc](https://github.com/clash-verge-rev/clash-verge-service-ipc)。上游 Cargo 作者信息为 **Tunglies**；原作者及其他贡献者的权利、原始声明和 GPL 第 3 版许可证均保留。

本次适配日期：**2026-10-04**。导入版本：**2.7.5**；ZenClash Fork 版本：**2.7.5+zenclash.1**，与 GUI 的版本号独立。用户提供的源码副本没有附带可验证的 Git 提交号，因此不声称对应某个上游提交；[UPSTREAM.json](UPSTREAM.json) 记录修改前的文件 SHA-256，供追溯导入基线。[NOTICE.md](NOTICE.md) 记录 Fork 修改声明。

本目录的代码在 ZenClash 仓库独立开发与维护；上游仓库和 examples 仅为来源/参考，不是本服务的构建输入，也不进行自动源码同步。

## 当前开发范围：Windows（2026-10-05）

按当前要求先完成 Windows；macOS/Linux 的进一步接线和验证留给对应平台开发。
已经复制的跨平台基础源码保留，不以它证明其他系统已经验收。本轮新增的 macOS
代理接线已撤回。两个 Fork 都在本仓库独立维护，examples 与上游仓库不参与构建。

Windows 主应用使用原生所有者会话、Start/Stage/Stop、命名管道 HTTP/WebSocket、
共享 RunState、后台授权与 Service/Sidecar 互斥。服务可用时走认证服务，不可用时
依上游 Windows 策略选择 Sidecar，不能绕过服务/残留内核的执行锁。GUI 始终由
zenclash.exe 启动；服务启动或维修失败也保留 GUI。系统代理由普通用户桌面后端
控制，TUN 的特权内核操作走服务；此次没有运行真实 TUN。

本轮复验：Fork 服务 123 项通过、1 项忽略；集成库 118 项通过；主应用真实隔离
Fork IPC 生命周期 1 项通过；默认生产 GUI 编译及全 target/feature 严格 Clippy
通过。Windows 打包 9 个用例验证 GUI/服务载荷、构建失败、版本失败和入口。
安装脚本显式要求非空 GUI，Inno 清单独立列出 zenclash.exe；开始菜单、桌面及
安装后启动均指向 GUI。隔离模拟内核测试不等于真实 TUN、订阅或安装后 UI 验收。

GPL 原始许可、作者与修改来源继续保留。旧 dist/gpl-2026-10-05-native-owner
是不可变的同版二进制/源码配对；本轮更新不得冒充已包含在旧源码中。
新增交付计划放在 dist/gpl-2026-10-05-windows-focus，只有配套安装包、完整源码和
BUILD-MANIFEST.json 全部完成才构成交付。真实安装与原生 GUI 操作仍未验收。

## 实现和接入状态

保留上游的 IPC、所有者认证、会话、受保护内核安装、运行时配置、Service/Sidecar 互斥、恢复和三平台安装/卸载实现。本次改动主要适配 ZenClash 标识、路径、构建目标和现有的 `mihomo` 内核文件名；移除了上游 macOS 安装器对旧 Clash Verge helper 的清理。

- Windows 使用 SCM 服务和命名管道。
- macOS 使用 launchd 和 PrivilegedHelperTools，生产 GUI Bundle ID 与本仓库 `platforms/macos/Info.plist` 的 `org.zenclash.app` 一致。
- Linux 使用 systemd、Unix socket 和受保护的持久化目录。

该程序是后台服务；**GUI 始终由 `zenclash` / `zenclash.exe` 启动**，应用快捷方式和登录启动项应指向 GUI。GUI 对 TUN、非 TUN、权限提升以及 Service/Sidecar 切换的选择属于主程序接入逻辑。

**接入状态（2026-10-05）：** `zenclash-core` 与 GUI 已改用`zenclash-core::service` 的所有者会话、原生 HTTP/WebSocket、Start/Stage/Stop 和安装维护接口，Windows GUI 编译通过。主应用打包脚本改为 `standalone,client` 并携带三个工具，四种打包载荷回归通过。2026-10-05 已生成阶段 Windows 安装包及同版对应源码，解压后的文件哈希与离线编译检查通过。持久共享 RunState/PAC 和核心事务已接通；后续策略/恢复收尾、修复的重新打包及三平台实机验收仍未完成；不能仅凭这些结果宣称安装包已验收。`crates/zenclash-service-bak` 是用户保留的旧实现备份，不参与构建。

`ClashConfig`、`start_clash`、`/clash/...` 等名称沿用上游的内核控制接口语义。协议 epoch/revision 保持 2/5，但产品 IPC 地址、协议头和所有者令牌文件名均已改为 ZenClash；本 Fork 无需与 Clash Verge 服务互通。

## 产品标识

| 项目 | 生产通道 | 开发通道（`development-channel`） |
| --- | --- | --- |
| 服务 slug | `zenclash-service` | `zenclash-service-dev` |
| Windows SCM 名称 | `zenclash_service` | `zenclash_service_dev` |
| Windows 显示名称 | `ZenClash Service` | `ZenClash Development Service` |
| Windows 服务 IPC | `\\.\pipe\zenclash-service` | `\\.\pipe\zenclash-service-dev` |
| macOS GUI Bundle ID | `org.zenclash.app` | `org.zenclash.app.dev` |
| macOS launchd Label | `org.zenclash.app.service` | `org.zenclash.app.dev.service` |
| macOS 服务 IPC | `/var/run/zenclash-service/service.sock` | `/var/run/zenclash-service-dev/service.sock` |
| Linux 服务单元 | `zenclash-service.service` | `zenclash-service-dev.service` |
| Linux 服务 IPC | `/run/zenclash-service/service.sock` | `/run/zenclash-service-dev/service.sock` |

Windows 的受保护目录位于 `%ProgramData%/zenclash-service`，Linux 位于 `/var/lib/zenclash-service`，macOS 位于 `/Library/Application Support/zenclash-service`；开发通道使用对应的 `-dev` 目录。macOS helper 安装到 `/Library/PrivilegedHelperTools/<launchd Label>.bundle/Contents/MacOS/zenclash-service`。两个通道保留上游共享的内核运行锁，避免各启动一个内核。

捆绑内核使用 `mihomo` / `mihomo-alpha`，Windows 加 `.exe`。安装器在自己的可执行文件目录查找 `zenclash-service` 以及这些内核文件；构建产物应按此约定放置。生产与开发 GUI 必须链接相同通道的服务库；开发版 macOS GUI 打包时还需使用开发 Bundle ID。

## 构建

在 **ZenClash 仓库根目录**运行。需要支持 Rust 2024 edition 的 Rust 工具链及目标平台构建工具。根目录 `Cargo.lock` 是 workspace 构建使用的锁文件；本目录的锁文件保留自上游快照，经 Fork 包名/版本适配，不替代根锁文件。发布构建使用根 workspace 的 release profile。

```sh
# 生产服务及安装、卸载工具
cargo build --release --locked -p zenclash-service --features standalone,client \
  --bin zenclash-service --bin zenclash-service-install --bin zenclash-service-uninstall

# 开发通道（GUI 也需要开启 development-channel）
cargo build --release --locked -p zenclash-service --features standalone,client,development-channel \
  --bin zenclash-service --bin zenclash-service-install --bin zenclash-service-uninstall

# 使用测试 IPC、测试状态目录和模拟内核，不安装系统服务
cargo test --locked -p zenclash-service --features standalone,client,test
```

Windows PowerShell 可把上述多行构建命令合成一行；反斜线续行是 POSIX shell 语法。产物在 workspace 的 `target/release` 下；显式使用 `--target` 时在 `target/<target>/release` 下。`test` feature 改变 IPC、状态路径和测试认证行为，**不得用于发布服务**；`--all-features` 同时开启测试和开发通道，不能用来构建生产包。

GUI 依赖示例：

```toml
zenclash-service = { path = "../zenclash-service", features = ["client"] }
```

服务可执行文件支持 `--version`，此查询在初始化日志、进入 SCM 或启动 IPC 之前返回，不启动后台服务。

安装/卸载工具会修改系统服务注册、受保护目录及权限，需要由完成接入的主程序在适当权限下调用。接口和维护参数见 `src/management.rs`、`src/bin/install_service.rs` 与 `src/bin/uninstall_service.rs`。这里没有执行系统服务安装、卸载或 TUN 实机测试。

## 本次验证（2026-10-04）

在 Windows x64 上完成：

- 生产通道的服务、安装和卸载工具构建通过（`standalone,client`，未开启 `test` 或开发通道）。生产服务 `--version` 正常返回 `zenclash-service 2.7.5+zenclash.1`。
- `cargo test --locked -p zenclash-service --features standalone,client,test`：**123 通过，0 失败，1 忽略**；忽略的是上游进程探测 benchmark。覆盖认证、所有者/会话、占用互斥、运行时配置、监听器恢复、内核崩溃限制及安装参数等。
- `cargo clippy --locked -p zenclash-service --all-targets --all-features -- -D warnings`、服务格式检查和 Git diff 空白检查通过。
- LICENSE 的 SHA-256 为 `81cbae84a29ce7e770bf2bc7b178e50bda0ce8de6067aba661b0bc7b05b562f8`，与导入前和本仓库上游示例完全一致。

macOS/Linux 本次完成源码标识与路径适配，未在对应系统编译或实机验证。Windows 主机没有 NSIS 编译器，服务 NSIS 模板未编译；主应用整包、GUI 接入及 TUN 测试不在这些验证结果内。

## GPL-3.0 与分发

本 Fork 及修改以 **GPL-3.0-only** 发布，与 ZenClash 根 workspace 的许可一致。[LICENSE](LICENSE) 是原样保留的完整 GNU GPL 第 3 版；原始作者信息没有被 ZenClash 作者替换。被修改的源码带有修改日期及指向 NOTICE 的声明，其他原始版权和许可证声明也须继续保留。

发布源码或安装包时落实以下要求（对应 GPL 第 4、5、6 节）：

1. 保留原作者、版权、许可、无担保声明；随分发提供完整 `LICENSE` 和本 Fork 的 `NOTICE.md`。修改形成的受 GPL 覆盖作品整体按 GPL 第 3 版提供，不能把这一 Fork 改为闭源或附加限制接收者 GPL 权利的条款。
2. 网上发布二进制时，在下载处提供免费、等同可访问的**完整对应源码**，或在二进制下载处明确指向这样的源码下载。源码必须对应实际发布的那一版，包含 ZenClash 修改、锁文件、必要的构建/安装脚本与资源；只提供上游仓库链接或未修改源码不够。
3. 本服务被 ZenClash 主程序通过 Rust 库集成后，对应源码应覆盖组合程序的 GPL 覆盖部分，包括主程序接入代码；不要只交付这个 crate。必须保留生成该版本的构建说明、配置及非系统依赖的必要源码获取信息/源码，并按 GPL 对应源码要求提供所需内容。
4. 发布前核对各依赖和捆绑内核的许可证兼容性及自身声明要求，带齐第三方许可证；适用 GPL 第 6 节 User Product 条款时，还需提供要求的 Installation Information。交互界面的适当法律声明按 GPL 第 5(d) 节落实。
5. 建议用不可变 tag/提交和源码归档关联每个安装包，并记录构建命令、工具链和校验值。将 `LICENSE`、`NOTICE.md` 及对应源码获取说明实际纳入最终安装包，验证安装后能访问它们，再发布；README 中写出要求本身不替代实际提供源码。

`resources/installer.nsi` 模板已加入许可证页面及 LICENSE、NOTICE.md、README.md 文件；构建时改变模板所在目录需设置 `ZENCLASH_SERVICE_SOURCE_DIR`。它是服务工具的上游 NSIS 模板，主应用使用的安装脚本已携带三个维护工具及许可文件；正式分发仍需提供同版完整对应源码。

完整许可文本优先于这份分发操作说明；官方文本：[GNU GPL v3](https://www.gnu.org/licenses/gpl-3.0.html)。本地阶段交付已携带二进制对应源码与声明，未发布正式 Release；每次后续构建仍须提供与其一致的对应源码，不能复用旧归档替代。

主应用的 GPL 文件暂存和对应源码分发已接入根打包/发布流程，操作和当前验证边界见 [GPL 分发文档](../../docs/development/gpl-distribution.md)。已完成阶段 Windows release 构建和对应源码离线编译检查；真实安装验收、三平台实机及完整生命周期接线仍未完成。


### 主仓库验证补充（2026-10-05）

ZenClash GUI 与三个生产服务工具在主仓库 Windows release 构建成功。
集成库 115 项单元测试及 1 项隔离原生 IPC 测试通过。追加离线维修后，核心 641 项回归、
GUI 19 项启动测试及 319 项组件/交互测试通过，core/UI 严格 Clippy 通过。设置页“许可证与来源”
可离线查看 GPL 和两个 Fork 的声明。离线会话可以维修服务，维修成功后 GUI 再尝试正常启动；
取消/失败保留窗口。外部控制器、过期/其他会话请求在原生授权前被拒绝。
阶段安装包及完整对应源码位于 `dist/gpl-2026-10-05/`，不包含后续离线维修修复。新构建必须
重新导出同版源码。这些结果不代表 macOS/Linux 实机、真实 TUN、真实安装或全部生命周期验收完成。


### 启动策略验证补充（2026-10-05）

主仓库已补齐 bootstrap 服务健康、接受的 Sidecar 会话许可、GUI capability/attention
读取，以及明确失败/取消维护后的 allowance 恢复。核心 667 项、GUI 320 项及启动
19 项回归通过，严格 Clippy、生产 GUI 编译和 19 项 GPL/对应源码检查通过。GUI
整套在 4 个测试线程下复验通过；首轮全并行的两项旧等待超时与验证边界见
`docs/development/service-sidecar-alignment.md`。本次后续修改未进入旧阶段安装包；
新的二进制仍须配套导出同版完整对应源码，没有执行真实 TUN 或三平台安装验收。


## 主应用原生 IPC 与所有权恢复验证（2026-10-05）

新增 `zenclash-core/tests/native_service_session.rs`，通过 `service-ipc-tests` 启动
真实 Fork IPC supervisor 和隔离模拟内核，使用与 GUI 相同的 CoreSession 初始化与
配置事务。验证启动保存、Service 运行模式、原生 HTTP、Stage/Reload 保持 PID 和 owner
generation、PAC 503/200/503 与地址保持，以及旧应用失去所有权后不能控制或停止替换
会话；原生服务运行时 Sidecar 锁被拒绝，退出后可获取。

测试最初只启动监听器，服务 lifecycle 保持 Starting，已改为调用生产使用的 supervisor；
没有为这一 fixture 问题更改产品的运行事实判定。

复制上游 service.rs 的 owner_recovery_policy 与持续控制通道失败健康观察。后台 owner
supervisor 按三平台策略清理属于本应用的代理，macOS 在被替换或服务不可达时保留机器级
代理。上游原始 LICENSE、作者、UPSTREAM 文件哈希仍保留，新增符号和日期已记录于
两个 NOTICE 和集成 Fork 的 UPSTREAM.json。旧会话不把外部 generation 当作新授权。

验证结果：核心 667 项通过、5 项忽略；集成库 118 项通过；主应用原生 IPC 1 项通过，
共享 fixture 的集成库原生 IPC 1 项复验通过。core/integration/UI 全 target 和 feature
严格 Clippy、默认生产 GUI cargo check，以及 19 项 GPL/源码/GeoData 回归通过。
此前 GUI 320 项与启动 19 项通过，本轮未修改 GUI 源码。

生产打包禁止启用 `service-ipc-tests`、`ipc-tests`、服务 `test` 或 development-channel；
不能使用 `--all-features` 构建发布二进制。隔离测试不修改系统服务注册、不安装服务、不
启动真实 TUN，不等同于真实 Mihomo、Windows 整包安装或 macOS/Linux 实机验收。

当前后续源码计划另存于 `dist/gpl-2026-10-05-native-owner/`，只有配套安装包、完整对应
源码及 BUILD-MANIFEST.json 全部生成并验证后才构成交付。旧 `dist/gpl-2026-10-05/`
保持为以前的匹配文件，不将旧归档重新标为当前二进制的对应源码。
