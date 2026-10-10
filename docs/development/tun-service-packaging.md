# TUN 服务打包记录

本文对应 [开发计划](tun-service-plan.md) P5，记录已接入的产物路径及验证边界；不代表发布包已经完成三平台 TUN 验收。

正式产物的安装、升级、卸载与权限验证按 [三平台原生验收](tun-service-native-validation.md) 记录，区分受控打包行为与目标系统结果。

## Linux 产物

### 2026-10-03 v3 本地 release helper 验证

当前工作区 v3 源码已用 Windows Rust 工具链和普通 WSL Ubuntu 26.04 链接桥构建 Linux x64 GNU helper：`cargo build -p zenclash-service --features server --locked --release --target x86_64-unknown-linux-gnu` 退出 0。产物为 `target/x86_64-unknown-linux-gnu/release/zenclash-service`，2,647,040 字节，SHA-256 `5E43D41BF6C39F898F9FF8DB832F99639467EF0065EC0E7D3056E646190CF9E4`；只读 ELF 检查为 x86-64 ELF64 PIE，普通用户 zen 的 `--version` 输出 `zenclash-service 0.2.0`，退出 0。

这是本地构建证据，不是固定发布校验和或正式 DEB/RPM。`readelf --version-info` 显示符号需求最高为 `GLIBC_2.39`，未证明旧发行版兼容；正式包仍需在既有发布环境构建、验证最低支持系统。没有执行安装、注册、服务运行入口或 TUN。旧 v2 helper 与当前 v3 GUI 不兼容，打包应从配套源码重新构建 helper；`--version` 仅验证构建版本，不是实际 IPC 握手证明。

[DEB 构建脚本](../../scripts/build_deb_package.sh) 和 [RPM 构建脚本](../../scripts/build_rpm_package.sh) 在构建 GUI 后单独执行：

```sh
cargo build --release --locked -p zenclash-service --features server --bin zenclash-service
```

构建脚本确认 helper 非空、可执行且 `--version` 成功并返回非空白内容后，才进入安装包生成阶段。helper 缺失、版本命令失败或输出为空时不能输出成功安装包。现有 Mihomo 下载版本、校验方式和发布架构保持原约定。

| 包管理器拥有的文件 | 用途 |
| --- | --- |
| `/usr/lib/zenclash/zenclash-service` | 普通用户应用调用的安装维护源程序 |
| `/usr/lib/systemd/system/zenclash-service.service` | 固定 systemd unit 资源 |
| `/usr/share/polkit-1/actions/org.zenclash.service.policy` | 固定 Polkit 授权资源 |

unit 的 `ExecStart` 指向 `/var/lib/zenclash-service/zenclash-service run`。该保护目录中的服务与 Mihomo 副本由管理员授权的安装事务部署，并记录批准摘要。打包脚本不创建安装元数据、不启动服务，也不修改网络；仅安装 DEB/RPM 不足以完成首次 TUN 开启。

应用内维护只管理自己的 `/etc/systemd/system/zenclash-service.service` 注册与保护目录数据，不删除上述包管理器文件。DEB/RPM 已接入下述升级与最终卸载策略；真实运行服务协调仍待目标系统验收，payload 测试不能证明原生升级/卸载完成。

### 包升级与最终卸载（2026-10-02）

包内增加 [package-service.sh](../../platforms/linux/package-service.sh)。DEB 的 `prerm remove` 和 RPM 的 `%preun` 最后一个实例卸载调用固定包内 helper 的 `--package-uninstall`；升级不执行卸载，也不自动替换管理员批准的副本或启动内核。包配置和文件移除后按需执行 `daemon-reload`，不主动启用服务。参数时机依据 [Debian 维护脚本约定](https://www.debian.org/doc/debian-policy/ch-maintainerscripts.html) 和 [RPM scriptlet 约定](https://rpm.org/docs/latest/manual/file_triggers.html)。

新入口位于 [package_maintenance.rs](../../crates/zenclash-service/src/package_maintenance.rs)，先确认当前管理员权限，再用真实进程身份、受保护执行文件与固定句柄摘要委托既有卸载事务；不接收客户端路径或身份参数。确认服务及内核停止并注销后才清理固定私有产物。即使保护目录已不存在，也仍确认停止/注销，防止遗留已运行宿主。未知元数据、未完成维护记录和原生失败沿用现有恢复或拒绝语义；非零退出阻止继续移除包，不删除源 helper、包文件或用户配置。

Windows 非提权进程下的入口权限行为有先失败再通过证据，未操作真实服务。共享脚本的 remove、重复调用、升级零卸载及失败传播测试通过；DEB/RPM 测试运行实际构建和暂存逻辑，并执行生成的卸载脚本，以受控 helper 回调验证升级零调用、最终卸载调用与退出码 17 传播。service all-targets/all-features check、默认客户端与服务端严格 clippy、脚本语法和差异检查通过，合并阶段服务完整测试 **179 项通过**；本批首次独立审查未发现功能性 bug 或重大漏洞。CI 和发布前 Linux 检查增加共享包策略测试。目标平台安装与原生卸载仍待验证，不能将这些结果报告为 P5 完成。

## Windows 产物

[Windows 构建脚本](../../scripts/build_windows_installer.ps1) 额外使用 `server` feature 构建 `x86_64-pc-windows-msvc` 服务二进制。helper 暂存到应用根目录的 `zenclash-service.exe`，校验非空及 `--version` 的退出码和输出后才调用 Inno Setup。

[安装包定义](../../platforms/windows/ZenClash.iss) 显式包含 helper，保留 `PrivilegesRequired=lowest`。安装包分发维护源程序；首次启用时另行请求 UAC，将批准的副本部署到保护目录。此阶段没有新增自动注册服务或执行 helper 的安装项。卸载 GUI 与已安装服务的协调尚待完成。

固定 SCM 注册的路径、参数、服务类型、账户和 owner/DACL 门栓已通过源码、行为及独立首审验证，见 [移植记录](tun-service-upstream-migration.md#windows-scm-注册身份)。2026-10-02 实际 Windows release helper 构建成功，`--version` 输出 `zenclash-service 0.1.2` 且退出码为 0；当前没有 Inno Setup 编译器，这不是安装包验收。GUI 卸载不能直接调用要求保护路径的 `--package-uninstall`：仍需普通权限桥接固定 UAC 维护入口，并在提权 worker 中确认共享授权及实际 owner，避免停止其他账户或新会话的内核。共享服务的卸载策略待确认。

2026-10-03 日志适配最终源码的 Windows helper 已重新构建：`cargo build --release --locked -p zenclash-service --features server --bin zenclash-service` 退出 0。产物为 `target/release/zenclash-service.exe`，2,775,040 字节，SHA-256 为 `94B9F95D4E167A1C8EA08BCB745FD55EC2712579294ED18CFA5F8648755BB77F`；实际 `--version` 输出 `zenclash-service 0.2.0`，退出 0。此摘要仅标识本次本地产物，不是固定发布校验和；未执行安装、卸载或服务运行入口。上段 0.1.2 是 2026-10-02 历史证据，不能作为当前版本。

## macOS 产物

### 2026-10-10：按需启动与退出

当前 macOS 安装器生成 `RunAtLoad=false`、`KeepAlive=false` 的 LaunchDaemon，并通过已注册的 Mach service 接受 XPC 唤醒。现有 Unix socket 继续承担版本探测和经过 owner/session 认证的控制操作，XPC 只负责启动与客户端连接生命周期。

GUI 保持一条原生 XPC 连接。所有客户端断开、活跃 owner 已清理且内核已停止后，service 空闲约两秒退出；下次 IPC 调用通过 launchd 重新启动。GUI 异常终止时，现有十秒 owner 租约先停止内核与清理代理，随后执行空闲退出。退出决定与 owner 操作共享生命周期锁，并拒绝退出决定之后的新 Start。

service 构建版本更新为 `2.7.5+zenclash.2`。已安装的旧 helper 和系统 plist 必须通过应用内“修复服务”更新；覆盖 `.app` 或重新打包 DMG 不会修改 `/Library` 中的旧安装。旧 helper 未声明按需唤醒能力时，客户端仍能查询其 Unix IPC 版本并展示修复入口。

原生验证使用当前 macOS 用户的独立 LaunchAgent 和实际 XPC/launchd，覆盖仅注册不启动、保留其他客户端、保留活跃 owner、最后会话结束后退出和再次唤醒新 PID：

```sh
cargo +1.95.0 test -p zenclash-service --all-features --lib \
  macos_activation::tests::launchd_starts_on_demand_exits_after_last_client_and_starts_again \
  --locked -- --ignored --exact --nocapture
```

该测试不替代 root LaunchDaemon 安装或实际 TUN 流量验收。另有安装 plist 原生解析回归和服务退出后的 Start 准入回归。离线恢复不再访问占位控制器 `127.0.0.1:0`，已用持有系统代理的实际 capture 协调器及模拟 native backend 复现并锁定。

本批服务全套回归、29 项系统代理回归、375 项 UI 回归、4 项翻译回归、client-only/standalone-only check 和严格 Clippy 通过。完整 core 库回归在既有 `cancelled_local_recovery_save_waiter_does_not_abandon_admitted_persistence` 用例挂起，已停止该次测试；用本批改动前、2026-10-10 01:44 构建的已有测试二进制执行同名用例，同样超过二十秒未结束。本批不声明 core 全库通过，该取消恢复用例另行处理。

2026-10-03 已补充既有发布目标 `aarch64-apple-darwin` 的 service all-targets/all-features 交叉 check 和 CI 全部附加严格 lint，均退出 0；此前 Intel 目标的检查证据仍保留。此次只向当前开发 Rust 工具链添加对应目标标准库，不修改 Cargo 依赖、Rust 版本、签名、最低系统版本或发布目标。ARM64 和 Intel 交叉 check 都不验证 Apple SDK/framework 链接、App 签名或原生 API 运行，这些仍需 macOS 实机。

[App 构建脚本](../../scripts/build_macos_app.sh) 使用原有 `aarch64-apple-darwin` 目标构建服务，校验非空、可执行及版本检查后，将 helper 放入 `Contents/MacOS/zenclash-service`，LaunchDaemon 资源放入 `Contents/Resources/org.zenclash.service.plist`。plist 中的服务路径仍指向管理员保护目录，打包不会向 `/Library/LaunchDaemons` 写文件。

helper 按现有 Developer ID 或 ad-hoc 分支单独签名，再独立验证，之后签名和验证整个 App；未调整签名身份、最低系统版本或发布架构。App 构建脚本改用 macOS 自带 Bash 支持的语法，便于直接运行受控打包行为测试；DMG 入口仍使用原有脚本。

## 自动验证

```sh
bash scripts/tests/build_deb_package_test.sh
bash scripts/tests/build_rpm_package_test.sh
bash scripts/tests/package_service_test.sh
bash scripts/tests/build_macos_app_test.sh
pwsh -File scripts/tests/build_windows_installer_test.ps1
```

DEB 测试运行真实构建脚本，并检查传给 `dpkg-deb` 的暂存树包含原样 helper、unit 和 policy。RPM 测试运行真实构建脚本和 spec 的 `%install` 命令，检查暂存文件的内容。两者均覆盖成功、helper 缺失、版本空输出与非零退出；失败分支须在进入打包工具前停止。两个测试均用受控 Cargo 和打包工具替身，离线执行且不安装系统文件。

这些测试证明构建与暂存行为，不证明原生 DEB/RPM 格式、签名、systemd/Polkit 授权或真实服务生命周期。CI 与发布前 Linux 检查运行两个入口；原生包安装、升级、卸载和首次 TUN 仍需目标发行版实机验收。

macOS 测试直接执行原 App 构建逻辑，检查真实暂存文件、两种签名分支的调用顺序，以及 helper 缺失、空文件和版本失败时不得进入签名。新增批次还覆盖版本命令退出 0 但输出为空或仅含空白：实际空白输出先被错误接受，修后完整 fixture 通过，失败保留既有 App 标记和 helper 字节且不调用签名；语法与差异检查及独立首审 PASS（1/2）。Cargo、签名、架构探测及原生 plist 工具使用受控替身；这不证明 Mach-O 链接、有效签名、Gatekeeper 或 launchd 授权。

Windows 测试执行原 PowerShell 构建脚本，使用普通原生可执行 fixture 验证实际暂存字节及版本命令；覆盖缺失、空文件、构建失败、版本非零退出和空输出。Cargo、图标检查及 ISCC 使用替身，不生成真实安装包，也不操作 SCM。CI 和发布前流程分别在对应平台运行这些入口；本机已验证暂存行为，真实平台产物仍待验收。

## 待接入

Windows GUI 卸载与已安装服务的协调、macOS 维护入口、旧 setuid 迁移，以及三平台包管理、授权、签名、首次 TUN 和故障恢复的实机验收仍按 P5/P6 推进。Linux 本批只交付上述包级策略与失败阻断，不包含自动授权新版本内核副本。
