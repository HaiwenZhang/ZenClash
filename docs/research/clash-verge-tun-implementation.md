# Clash Verge TUN 模式实现研究与 ZenClash 对照

研究日期：2026-10-07。研究对象是用户提供的本地目录 `debug/clash-verge-rev-2.5.7` 和 `debug/clash-verge-service-ipc`，结论以这两份源码为准。目录名不代表未经修改的官方发布版本；服务 manifest 标记版本为 2.7.5，没有可验证的上游提交号。本文先记录研究和实施方案，实施结果另列在末尾。

## 1. 核心结论

TUN 不是 GUI 自己实现的网络栈，也没有一条“GUI 调用 TUN 安装器后直接接管网络”的路径。实际分工是：

1. **Mihomo** 创建虚拟网络接口、配置路由、截获 DNS、转发 IP 流量。
2. **特权服务** 以系统权限运行 Mihomo，管理内核进程、所有者会话、受保护配置和资源。
3. **安装器** 注册系统服务，把服务和批准的内核复制到受保护目录；卸载器负责停止和移除服务。
4. **GUI** 持久化 TUN 意图、生成有效 YAML、申请授权、通过本地 IPC 启动或更新内核、回读运行状态。

Windows 的“服务模式”使 GUI 可以保持普通用户权限，而内核具有创建 TUN 所需的权限。管理员 GUI 也可以直接用 Sidecar 内核运行 TUN。不能把“服务安装成功”“IPC 可用”“TUN 配置启用”和“网卡、路由工作正常”视为同一个状态。

两个参考目录中，`wintun` 检索命中的是历史 changelog，而不是当前安装调用。当前应用和服务安装器没有显式安装 `wintun.dll` 的代码。这个证据只能说明当前安装链路没有单独的 Wintun 安装步骤；具体 Mihomo 二进制是否内嵌驱动、是否需要额外文件，须以实际捆绑内核构建为准，不能从 GUI 源码反推。ZenClash 当前包携带 `mihomo.exe`，没有加入一个未经证实需要的驱动安装器。

## 2. 阅读入口

以下路径均相对本仓库根目录；函数名可用于定位。

| 主题 | 参考文件 / 符号 |
| --- | --- |
| TUN 开关与缺服务时的交互 | `debug/clash-verge-rev-2.5.7/src/components/shared/proxy-control-switches.tsx`：`handleTunToggle` |
| 服务请求意图 | 应用 `src/services/service-request.ts`：`requestService` 和 `restore` |
| TUN 表单 | 应用 `src/components/setting/mods/tun-viewer.tsx` |
| TUN 默认配置 | 应用 `src-tauri/src/config/clash.rs`：`template`；`constants.rs`：`tun` |
| 有效配置增强 | 应用 `src-tauri/src/enhance/mod.rs`、`enhance/tun.rs`：`use_tun` |
| 配置变更和内核刷新 | 应用 `src-tauri/src/feat/config.rs`：`apply_verge_patch` |
| TUN 可用性协调 | 应用 `src-tauri/src/feat/tun.rs` |
| Service/Sidecar 启动选择 | 应用 `src-tauri/src/core/manager/lifecycle.rs`、`state.rs` |
| 安装、内核批准、启动与 owner 监控 | 应用 `src-tauri/src/core/service.rs` |
| Tauri 命令 | 应用 `src-tauri/src/cmd/service.rs` |
| 非特权准备和原生授权 | 服务 `src/management.rs` |
| 系统服务注册 / 批准内核复制 | 服务 `src/bin/install_service.rs` |
| 服务启动、退出 | 服务 `src/bin/service.rs` |
| IPC 路由和授权 | 服务 `src/core/server.rs`、`auth.rs`、`owner.rs` |
| 内核启动参数、watchdog、停止 | 服务 `src/core/manager.rs` |
| 配置与资源物化 | 服务 `src/core/runtime_generation/` |
| 安装目录和平台权限 | 服务 `src/core/paths.rs`、`windows_security.rs`、`unix_security.rs` |
| Service/Sidecar 互斥 | 服务 `src/execution.rs`、`execution_windows*.rs` |

## 3. 点击 TUN 开关后发生什么

```mermaid
sequenceDiagram
    participant User as 用户
    participant GUI as GUI
    participant Installer as 安装器
    participant OS as 系统服务管理器
    participant Service as 特权服务
    participant Core as Mihomo
    User->>GUI: 开启 TUN
    alt 当前没有 TUN 能力
        GUI->>GUI: 保存恢复意图，显示服务处理入口
        GUI->>Installer: --prepare-install --core 名称 路径
        Installer->>Installer: 临时副本 + SHA-256
        Installer->>OS: 原生管理员授权、注册服务
        OS->>Service: 运行受保护的服务副本
        Installer->>Service: /version 和 /installation 核验
    end
    GUI->>Service: 认证 POST /clash/start + RuntimeBundle
    Service->>Service: 校验 owner、批准内核和运行时资源
    Service->>Core: -d 运行目录 -f 配置 -ext-ctl-pipe/unix IPC
    Core-->>Service: 控制端点就绪
    Service-->>GUI: owner session generation
    GUI->>Service: 认证 PUT /clash/stage-runtime
    GUI->>Core: 控制器配置重载
    Core->>Core: 创建 TUN 接口、配置路由和 DNS 捕获
    GUI->>Core: 回读有效配置
```

前端 `handleTunToggle` 在 `isTunModeAvailable` 为 false 时发出 `tunNeedsService`，携带 `restore: {enable_tun_mode: true}`，并返回 false，使开关恢复原态。服务处理成功后才恢复意图。在已有能力时通过 `patchVerge` 写 `enable_tun_mode`。

后端配置写入串行化，TUN 变更触发 `CLASH_CONFIG`，托盘也刷新。Linux 在启用 TUN 时额外请求重启内核。`enhance::use_tun` 在订阅及增强结果上修改最终 `tun.enable`，而不是仅更新 GUI 状态。

`feat/tun.rs` 还处理启动和运行中的能力丢失。启动前确认服务缺失时，直接持久化关闭 TUN，避免提前启动内核；运行中对已稳定但不能支持 TUN 的 RunState 关闭 TUN。原子标志防递归，配置锁防并发覆盖。Sidecar 第一次启动的 YAML 也投影为 TUN 关闭，避免后续协调尚未完成时先以普通权限启动启用 TUN 的内核。

## 4. TUN / DNS 配置规则

参考应用的基础 TUN 字段为 `enable: false`、`stack: gvisor`、`auto-route: true`、`strict-route: false`、`auto-detect-interface: true`、`dns-hijack: [any:53]`。表单默认设备为 Windows/Linux 的 `Mihomo`、macOS 的 `utun1024`，MTU 默认 1500。Linux `auto-redirect` 与 `auto-route` 有联动；排除路由使用 `route-exclude-address`。

`use_tun` 保留已有 TUN mapping，只写最终 enable。启用时的 DNS 行为更具体：

- DNS `enhanced-mode` 缺失或为 `fake-ip`：启用 DNS，把 `dns.ipv6` 对齐顶层 `ipv6`（缺失按 false）。
- 缺失的模式补 `fake-ip`，缺失的 IPv4 fake IP 范围补 `198.18.0.1/16`。
- 顶层 IPv6 启用且 IPv6 fake IP 范围缺失时补 `2001:2::0/64`。
- 自定义 fake IP 范围、过滤规则、nameserver 等保留。
- 已显式使用其他 DNS 模式时，不执行上述 fake IP 强制修改。
- 关闭 TUN 只关闭 `tun.enable`，不会清空用户 DNS 配置。

macOS 分支还异步恢复/设置公共 DNS `114.114.114.114`，关闭 TUN 时恢复。它依赖应用自己的 DNS 快照与恢复流程，不能仅复制一次 `networksetup` 调用到 ZenClash。

`utils/init.rs::init_dns_config` 另外创建用户可编辑的 DNS 模板文件。它的 IPv6 fake IP 范围是 `fdfe:dcba:9876::1/64`，与 `use_tun` 的缺失值回退不同；已存在的范围不会被回退覆盖。本次没有把整份 DNS 覆写模板强行覆盖订阅，只在裸配置缺少 nameserver 时补入其服务器默认值：`8.8.8.8`、`https://doh.pub/dns-query`、`https://dns.alidns.com/dns-query`。bootstrap 列表为 `system`、`223.6.6.6`、`8.8.8.8`、`2400:3200::1`、`2001:4860:4860::8888`。

现代 Mihomo 的字段含义可参阅 [Mihomo TUN 配置文档](https://wiki.metacubex.one/config/inbound/tun/)。该文档会随内核变化，本次复刻的默认值来自提供的应用快照，不用网站当前默认值覆盖参考版本。

## 5. “安装 TUN 模式程序”的真实安装过程

### 5.1 三个服务工具

服务 Cargo manifest 提供 `clash-verge-service`、`clash-verge-service-install`、`clash-verge-service-uninstall`。GUI 是独立程序，服务不是 GUI 入口。应用的 `install_service` 从 GUI 同目录收集 stable / alpha 内核源，调用 `management::install`。

安装命令第一阶段示意：

```text
clash-verge-service-install.exe
  --prepare-install
  --prompt <授权提示>
  --core verge-mihomo.exe <源文件路径>
  --core verge-mihomo-alpha.exe <源文件路径>
```

`--core-only` 只批准新的内核；`--gid` 为 Unix socket / 文件组提供账户信息。`--ensure` 支持现有安装满足要求时跳过授权，Windows 还可启动正确但停止的服务。

### 5.2 提权前准备

`PreparedCores::new` 验证允许的内核名称，复制到唯一临时目录，从**复制后的字节**计算 SHA-256，组成 `--install-core <临时路径> --sha256 <摘要>`。安装器和服务也复制到临时目录。临时目录在阶段完成后清理。

这样源文件后来被替换不会让服务静默执行另一份内核。全安装同时比对服务摘要、协议和请求内核的最终检查结果。退出码为零不替代 `/installation` 核验；结构化的 `InstallationVerificationError` 经 stdout 报告保留给 GUI。

### 5.3 Windows 原生提升与 SCM

参考 `management::elevate` 使用 `ShellExecuteExW`，verb 为 `runas`，`SW_HIDE` 隐藏安装器窗口，`SEE_MASK_NOCLOSEPROCESS` 保留子进程 handle；等待后读取真实退出码。调用前初始化 COM，参数按 Windows 命令行规则转义。UAC 拒绝通过 Win32 错误返回。

授权后的安装器：

1. 获取维修互斥 gate，校验请求内核与摘要。
2. 准备受保护的服务目录和 staged 服务文件。
3. 查询 SCM；已有服务先停止并有界等待，恢复 active-owner 状态。
4. 发布内核和服务副本、更新注册；没有旧服务则创建。
5. 生产服务 AutoStart，开发通道 OnDemand；默认服务账户为 LocalSystem。
6. 设置 5/10/30 秒重启恢复，启动服务，等待兼容 IPC 就绪。

服务文件位于 `%ProgramData%/<service-slug>/bin`，批准内核位于同根 `cores`。Windows 文件替换遇到运行中 EXE 时，可先移到 `.old` 再发布 `.next`；这两个后缀禁止作为可执行内核选择，防止执行半成品。

### 5.4 macOS 和 Linux

| 平台 | 授权 | 服务注册 | 服务 IPC | 持久化目录 |
| --- | --- | --- | --- | --- |
| Windows | 原生 `runas` / UAC | SCM | `\\.\pipe\clash-verge-service` | `%ProgramData%/clash-verge-service` |
| macOS | `osascript` 管理员授权 | 系统 launchd、PrivilegedHelperTools bundle | `/var/run/clash-verge-service/service.sock` | `/Library/Application Support/clash-verge-service` |
| Linux | root 或 `pkexec` / `sudo` | systemd unit、daemon-reload、enable/start | `/run/clash-verge-service/service.sock` | `/var/lib/clash-verge-service` |

Unix 安装器复制二进制并设置权限，指定 GUI 的账户组；运行时和状态目录由服务保护。Linux unit 依赖 network-online、nftables/iptables，`Restart=always`。macOS helper plist 配置 Label、GUI bundle ID、RunAtLoad、KeepAlive；开发模式应用还有安全位置 staging，避免 TCC 路径问题。

## 6. IPC 如何调用内核

协议 epoch/revision 为 2/5。服务控制 IPC 和 Mihomo 控制器 IPC 是两层独立接口：前者管理系统权限、所有者和启动，后者负责 `/configs`、代理、规则、流量等内核业务。

| 服务请求 | 用途 | 所需身份 |
| --- | --- | --- |
| `GET /version` | 协议与能力探测 | 非会话探测 |
| `GET /installation` | 服务/批准内核摘要检查 | 安装探测 |
| `GET /status` | owner、内核 PID、生命周期 | owner credentials |
| `POST /clash/start` | 切换 owner 并启动内核 | owner credentials + 提议 session token |
| `PUT /clash/stage-runtime` | 当前运行代内更新配置和资源 | active owner + session proof |
| `DELETE /clash/stop` | 停止自己的内核、清 owner | active owner + session proof |
| `GET /clash/runtime-file` | 读取服务侧 provider 缓存等 | active session |
| `/clash/logs`、`/clash/log-snapshot` | 内核日志 | active owner |

`StartClashRequest` 包含 `RuntimeBundle`、随机提议令牌和可选 macOS proxy。owner credentials 包括原生 SID 或 UID/GID、应用数据目录与所有者令牌。服务验证原生 IPC peer、目录所属账户、令牌和 active generation；旧 GUI 会话不能控制新会话。

服务先在 owner 生命周期锁中准备运行时、校验批准内核和资源，再停止旧内核、发布新的配置与 owner 状态。成功返回 generation，GUI 持有 `{generation, token}` 用于后续 stage/stop。启动不会只信 GUI 提供的任意 EXE 路径。

服务实际给 Mihomo 的参数为：

```text
<批准内核> -d <服务管理的运行目录> -f <有效 YAML>
           -ext-ctl-pipe <内核命名管道>     # Windows
           -ext-ctl-unix <内核 Unix socket> # Unix
```

先等待内核 IPC 就绪，再记录 PID/进程身份和 Running；IPC 尚未就绪退出的内核属于启动失败。watchdog 监控崩溃、限制重启并更新 lifecycle。Service 与 Sidecar 共享执行锁，还检查残留特权进程，防止同时接管网络。

`RuntimeBundle` 不仅是 YAML：它还包含本地 provider、证书/密钥、GeoData 和远程 provider 缓存描述。服务复制到受保护的运行代，配置最后提交；Stage 只清 manifest 管理的旧文件，provider URL 改变时使缓存失效。stage 后再通过内核控制器重载；只 stage 文件不会让内核自动采用配置。

## 7. 关闭、退出、卸载与失败

正常关闭 TUN通过配置重载撤销捕获。正常 Stop 清理服务代理、停止内核、持久化 owner stopped 状态、清 active owner。Unix 内核退出先 SIGTERM，最多 3 秒后 SIGKILL；输出读取最多 1 秒，避免子进程继承 stdout 后停止卡死。Windows 无 SIGTERM，走进程终止。

IPC handler 总预算 25 秒。Start takeover 为后续 15 秒 IPC 就绪和提交预留时间，Stop 为输出 drain 和 owner 清理预留时间；SIGKILL 截止时刻从 handler 开始计算，而不是队列等待结束后再给完整宽限期。

安装或维修失败需重新探测真实服务状态；用户取消不能让 pending action 持续弹窗。卸载成功后应用启动 Sidecar；非管理员 Sidecar 先关闭 TUN。批准内核路径被拒绝可以提示修复或退到 Sidecar，但不能绕过执行锁，也不能把拒绝当成安装成功。应用的 owner monitor 区分会话被替换、自己的内核故障和持续 IPC 失败。

## 8. ZenClash 已有实现与本次差异

2026-10-08 后续结构调整：应用侧独立集成 crate 已整体合并为 `zenclash-core::service`。
下面的起始状态和测试命令是迁移前的研究记录；现行构建与测试入口见
`crates/zenclash-core/src/service/README.md`。

研究开始时，本仓库已经导入 `zenclash-service` 和 `zenclash-service-integration`。不需要再次建立一个并行服务框架。

| 链路 | ZenClash 现有位置 | 研究开始时结论 |
| --- | --- | --- |
| 系统安装、批准内核、IPC 服务 | `crates/zenclash-service` | 已从上述服务导入，需保留产品身份和已有修复 |
| owner 会话、控制器传输、运行状态 | `crates/zenclash-core/src/service/session.rs`、`runstate/` | 已接通 |
| TUN 授权与交接 | `crates/zenclash-core/src/service_manager/tun.rs`、`core_session/service_tun.rs` | 本地配置在授权前冻结，服务启动后提交并可恢复 |
| 捕获切换 | `crates/zenclash-core/src/traffic_capture.rs` | 支持系统代理/TUN协调和回滚 |
| GUI 入口 | `crates/zenclash-ui/src/pages/runtime/tun.rs`、`tun/service.rs` | 已提供启用、安装、维修和卸载入口 |
| 系统事实 | `crates/zenclash-core/src/tun_runtime.rs` | 独立回读配置对应接口与路由 |
| Windows 提权 | 服务 `src/management.rs` | 当前绕 PowerShell `Start-Process`，不同于提供的原生实现 |
| 内核停止与输出 drain | 服务 `src/core/manager.rs`、`server.rs` | 当前立即 kill，缺上游的 Unix宽限和 handler 截止预算 |
| TUN 有效配置 | core `controlled_config.rs`、`service_runtime.rs` | 当前只补 `tun.enable` 和 `dns.enable`，缺 fake IP/IPv6 与基础 TUN 默认字段 |
| 包载荷 | `scripts/build_windows_installer.ps1` | 已构建三个服务工具，GUI 同目录放工具、`resources/mihomo.exe` 放内核 |

ZenClash Windows 身份为 SCM `zenclash_service`、管道 `\\.\pipe\zenclash-service`、根 `%ProgramData%/zenclash-service`。这些必须保留，以免与参考项目的服务和日常配置互相覆盖。此前 Windows 为主要接入范围；Unix 源码保留不能代替对应平台的实机安装与网络验收。

## 9. 本次实施方案（研究完成后执行）

1. 复制当前参考服务的原生 Windows 提权流程，保留 ZenClash 参数/身份，补足资源释放和错误处理；结构化错误跨准备子进程保留原生取消语义。
2. 同步参考服务的有界输出 drain、Unix SIGTERM→SIGKILL，以及 Start/Stop handler 停止预算，避免关闭 TUN 或切 owner 时卡死。
3. 在 ZenClash 有效配置生成层补齐缺失的 TUN 基础默认字段，以及参考 `use_tun` 的条件 DNS/fake IP/IPv6规则。自定义 TUN 参数、解析器和地址范围保留；fake-ip 模式按参考强制开启 DNS、对齐 IPv6，关闭 TUN不抹除 DNS。服务冻结 bundle 与持久化 YAML 使用相同规则。
4. 对默认配置、显式自定义、IPv6、redir-host、服务交接、停止/重启和原生 IPC 执行回归；编译 GUI 和三个服务工具，记录真实验证边界。
5. 保留 GPL、原作者与既有基线清单，新增这次本地源码 hash/符号来源，不能把旧安装包称为包含本次修复。

## 10. 实施与验证记录

研究正文与实施方案已先于代码变更写入；本节随后补充实施结果。

### 10.1 已落地的调用链

- 新增 `crates/zenclash-core/src/tun_config.rs`，统一生成基础 TUN 默认字段与条件 DNS 增强。普通有效配置、服务准备、冻结 bundle 的局部修改使用同一投影规则。
- `controlled_config.rs`、`core_session/service_tun.rs` 和 `traffic_capture.rs` 的开启意图只写 `tun.enable`，由投影决定 DNS 行为，因此 `redir-host` 不再被无条件开启 DNS。生成的默认字段只进入运行 YAML，不污染用户持久覆写。
- 局部配置更新保留当前已接受的订阅和资源；TUN 增强不额外引入 GeoData 地址、改变端口或接受磁盘上后来变化的规则。
- `zenclash-service/src/management.rs` 使用原生 `ShellExecuteExW(runas)`，检查等待结果与真实退出码，平衡 COM 初始化并自动释放进程 handle。Win32 授权错误通过 `ZENCLASH_AUTHORIZATION_FAILURE_V1` 结构化报告返回 GUI，UAC 取消码 1223 不依赖本地化 stderr 的文本猜测。
- 同步 `core/manager.rs`、`server.rs` 的停止、输出 drain 与截止预算；保留 ZenClash 身份、owner 授权、批准内核、资源发布、watchdog 和已有维护恢复逻辑。

安装、维修、卸载与 TUN 开关继续使用既有 GUI 入口及 `ServiceManager` / `CoreSession`。没有另建第二套服务或修改参考目录。新增源码 hash 清单为 `docs/research/clash-verge-tun-sources.json`；两个 NOTICE 保留作者、GPL 和修改日期。

### 10.2 回归验证

| 检查 | 实际结果 |
| --- | --- |
| `cargo test -p zenclash-core --lib --locked -- --test-threads=2` | 686 通过，5 忽略，0 失败 |
| `cargo test -p zenclash-service --features standalone,client,test --locked -- --test-threads=2` | 全部通过；库 91、安装器 21、卸载器 2、集成 13；1 项库测试忽略 |
| `cargo test -p zenclash-core --test native_service_session --features service-ipc-tests --locked -- --test-threads=1` | 1 项通过；覆盖实际原生命名管道、Start、TUN 开/关、Stage/Reload、配置回读、owner 替换和停止 |
| `cargo test -p zenclash-service-integration --features ipc-tests --locked -- --test-threads=2` | 119 项库测试和 1 项原生 IPC 验收通过 |
| core / service / integration / UI 的 `cargo clippy --all-targets --all-features --locked -- -D warnings` | 通过 |
| `cargo check -p zenclash-ui --all-targets --locked` | 默认生产特性通过 |
| `cargo build -p zenclash-service --features standalone,client --bin zenclash-service --bin zenclash-service-install --bin zenclash-service-uninstall --locked` | 三个 Windows debug 工具构建通过；未启用 test/development 特性，服务 `--version` 返回 `2.7.5+zenclash.1` |
| 服务 `standalone,client`、`--all-targets`、`--locked --offline`，目标 `x86_64-unknown-linux-gnu` | 交叉源码检查通过；未在 Linux 上执行 |
| 同上，目标 `x86_64-apple-darwin` | 修正失效卸载器引用后通过；未在 macOS 上执行 |
| 本次修改 Rust 文件 `rustfmt --check` 与 `git diff --check` | 通过；7 份参考源码 SHA-256 全部复核匹配 |

新增配置回归覆盖裸配置、IPv6、用户自定义参数、redir-host、关闭 TUN、幂等性、无效 DNS 保留，以及冻结 bundle 与保存 YAML 一致且不提前写缓存。停止回归验证最终输出读取、继承管道不关闭时的总时间上限、排队后过期的停止预算；授权回归验证原生取消错误跨子进程保留。

原生验收运行的是完整 IPC 服务监督器和明确隔离的模拟内核，配置从服务实际下发的路径加载；启用 TUN 后回读路由/DNS 捕获参数，Windows 上 PID 和 owner generation 保持不变。模拟内核只实现控制器，不创建物理 TUN 网卡。测试特性不得用于生产打包。

普通沙箱限制临时文件、IPC、rustfmt 执行或 Clippy 配置读取；已在允许这些本地操作的环境中完成重跑。日志保存在忽略目录 `target/tun-*.log`，不作为可分发源码文件。编译器有硬链接回退与 MSVC linker 输出提示，不影响成功结果。

macOS 交叉检查发现卸载器残留对 `uninstall_old_service` 的导入与调用，而 Fork 早已移除了该函数以免清理参考产品的 helper。本次删除失效引用，继续使用 ZenClash 自己的 helper 标识。

### 10.3 验收边界

本轮没有修改机器的 SCM 注册、公共 DNS、系统代理或真实路由，也没有使用管理员权限启动真实 Mihomo TUN。Windows 实机验收仍需用同版完整安装载荷完成以下步骤：

1. 普通用户 GUI 请求安装服务；批准 UAC 后核对服务健康和批准内核摘要，取消 UAC 后核对开关及待处理意图回滚。
2. 打开 TUN，核对 `Mihomo` 接口、路由、DNS 捕获与连通性；再关闭，核对捕获撤销。
3. 对服务维修、卸载、内核崩溃、GUI 退出和会话接管，核对恢复与资源清理。

跨平台源码停止策略已适配，但当前项目的交付范围以 Windows 为主。没有复制参考 macOS 的公共 DNS 快照/恢复流程；Linux 参考应用的“启用时额外重启”与三平台真实安装、路由、网络验收仍需对应平台确认。本文不将源码编译或模拟内核测试称为网络验收，也不将以前生成的安装包称为包含本次修改。

## 11. 2026-10-10 macOS 局域网 DNS 绕过 TUN 的实机排查

Mihomo v1.19.32、系统代理 127.0.0.1:7890：`tun_tls_probe.sh system-proxy --proxy-port 7890 --count 1` 返回 HTTP 200，TLS 约 3.27 秒。临时启用与应用相同的 gVisor/utun1024/auto-route/any:53 设置后，`route get 1.1.1.1` 确认走 utun1024，但相同 ChatGPT 请求稳定在约 10 秒后发生 TLS `SSL_ERROR_SYSCALL`，Google 同样失败。每次探测都在 finally 中恢复原 TUN 设置。

系统解析器仍指向局域网路由器的 IPv4/IPv6 地址；连接回读中两项 TUN HTTPS 请求的 host 为空，目标地址分别是 157.240.7.8 与 69.171.235.22。`dig @1.1.1.1` 经 TUN 返回 198.18.* Fake-IP；只给相同 curl 请求传入该解析结果，两域名立即返回 HTTP 200，TLS 分别约 0.50/0.16 秒。它证明该故障来自系统 DNS 路径，而不是这个场景中的 MTU 或 gVisor 传输。[上游 TUN 文档](https://wiki.metacubex.one/config/inbound/tun/)明确说明 macOS/Windows 无法自动劫持局域网 DNS。

本次新增服务拥有的 macOS 临时 supplemental 默认解析器，向已验证可被 TUN DNS 劫持的 1.1.1.1 发送查询；不改写永久 DNS 偏好。使用 `SCDynamicStoreAddTemporaryValue`，拒绝覆盖别的会话已有的 key，正常关闭时显式撤销，helper 被终止时由 configd 自动删除。启用、重载与恢复后按实际 TUN 回读同步；服务启动和 watchdog 恢复从受控配置建立，停止及异常退出释放。队列在等待前提交，取消不能让旧 enable 晚于 stop 执行。协议升至 revision 7，旧客户端/服务不能静默跳过新步骤。

捕获切换 39 项、运行恢复 10 项、DNS 回读规则 1 项、服务模拟库 108 项、旧会话拒绝 DNS 写入的真实隔离 IPC 回归 1 项均通过；Core 与 Service 严格 Clippy 通过。运行恢复夹具首次并行执行发生时间戳目录碰撞，增加进程内序号后重跑 10 项全部通过。release App `0.2.0-test.20261010-tun-dns` 已构建并通过签名校验。模拟测试不改变宿主 DNS。

**验收仍未完成**：当前用户没有免密码管理员权限，普通用户临时动态 store 写入返回 SCError 1003。新服务的实际 DNS 接管、恢复、浏览器访问与 Codex 模型回答须更新管理员服务后验证；`--resolve` 对照成功不等于系统 DNS 修复已在安装版生效。尚未证实 Codex 原有耗时来自 WebSocket 回退。

测试 App 已复制到 /Applications/ZenClash.app，四个可执行文件摘要与构建产物一致、签名复核通过；旧 App 完整备份在 /private/tmp/ZenClash-before-tun-dns-20261010.app。随后 `--prepare-install` 更新管理员服务的命令被自动审批审查拒绝，理由是用户尚未明确授权该特权服务变更及其范围，命令未执行。已请求这项具体授权，等待答复；仍运行原进程和原服务。最终只读复核确认 TUN 关闭，系统代理到 Google 返回 HTTP 200。

### 11.1 隔离 IPC 的 DNS 同步链路与模型调用基线

补审发现 `service-ipc-tests` 排除了 macOS 客户端的 DNS 同步逻辑，全功能严格 Clippy 因此报告该判定函数未使用。新增仅测试可见的模拟 DNS 状态，原生动态 store 仍完全排除；同一个受控配置读取步骤也进入模拟启动/恢复。实际应用 Start→Stage→Reload→回读回归首次失败于“未获得 DNS 解析器”，取消客户端测试路径的特殊跳过后，验证开启、关闭、替代 owner 接管、旧 owner 退出不撤销新解析器，以及 Stop 撤销。

这项夹具只创建本地控制器，不创建 TUN；宿主已有真实 Mihomo 时，完整 Sidecar 准入会正确拒绝，因此最终只用独立测试执行锁验证夹具释放。调试期间还观测到 IPC listener 重建失败，未据此改动生产监听器；清除临时日志后连续三次完整回归通过，不宣称所有 CI 调度条件都已稳定。

Codex CLI 0.162.0 使用已有 ChatGPT 登录、当前配置模型 gpt-6.1-sol、显式 HTTP 代理 127.0.0.1:7890，在临时只读目录发出一次仅回复 OK 的请求。使用[官方非交互文档](https://learn.chatgpt.com/docs/non-interactive-mode)所述的 `--ephemeral`，本次调用禁用 hooks、plugins 和 MCP 服务，不读取工作区。返回 OK，退出码 0，耗时 9.619 秒；未出现 WebSocket 重连/回退信息。这证明当前 CLI 的系统代理路径能完成模型调用，不证明桌面 App 的历史回退行为，也不替代 TUN 下的模型验收。只保存脱敏结果于 target/diagnostics/codex/system-proxy-baseline.json。

### 11.2 获得授权后的安装版网络验收

用户明确授权更新管理员服务、launchd 注册及服务内 Mihomo，并允许测试时关闭备用 Clash Verge。首次更新在 bootout 后立即进行所有权维护时遇到旧服务锁仍被占用，服务二进制尚未发布；退出旧 GUI、确认旧服务已停止后，同一受控安装器重试成功，公开协议实际回读为 epoch 2/revision 7，受保护 helper 摘要与测试 App 一致。

本次手动安装先使用了 App 内的签名内核，但应用默认选择用户目录的现有内核。两者同为 v1.19.32，摘要分别为 bf496136997d79e2cad176ff221734e6ba3ba6e986b438862d065131ada09a40 与 3a4005e58ec7cbe86f8a347c2897ca9d246a26dcdea7c60c3894872b073fc9a6；真实 `/installation` 对实际选择返回 digest_mismatch。用同一安装器的 core-only 流程将服务副本对齐到应用当前选择后，准入返回 ready，重新启动 GUI 后真实服务内核 PID 74530、控制器 9090 正常响应。未覆盖用户的现有内核或订阅，也未把未匹配的摘要检查放宽。

关闭 Clash Verge TUN 后，从 ZenClash GUI 开启 TUN，系统代理实际关闭，公网路由走 utun1024，macOS `scutil --dns` 首选 resolver 为 1.1.1.1、Supplemental、order 1。未用 `--resolve` 或显式代理：Google 两次 HTTPS 均为 HTTP 200，系统返回 198.18.0.9，TLS 0.173/0.166 秒；ChatGPT 两次为 HTTP 200，系统返回 198.18.0.7，TLS 4.115/4.995 秒。Safari 在该模式下重新加载 Google，页面正常显示搜索框。

Codex 禁用大小写 HTTP/HTTPS/ALL_PROXY、NO_PROXY 设置为 *，以已有 ChatGPT 登录与 gpt-6.1-sol 实际返回 OK，退出码 0。首次 31.471 秒，日志显示模型列表刷新超时；随后 GUI 完成 TUN→系统代理→TUN 的完整切换：关闭 TUN 后路由恢复 en0、临时 resolver 撤销、原局域网 IPv4/IPv6 DNS 恢复，系统代理实际为 127.0.0.1:7890，ChatGPT HTTPS 和模型回答通过（19.191 秒）。重新启用 TUN 后原始 HTTPS 探针再次通过、真实模型回答通过（18.774 秒），无目录刷新或传输错误。没有观测到 WebSocket 重连/回退信息，不能将历史等待归因于该路径。

最终保持 ZenClash TUN 开启、系统代理关闭、Clash Verge TUN 关闭。上述证据完成此台 macOS 的浏览器/模型连通性和 DNS 撤销、再接管验收；不代表 Windows/Linux 已验收，也不证明所有第三方 VPN、休眠或 DHCP 切换场景。新增 `scripts/diagnostics/codex_model_probe.py --route tun|proxy` 保留同一脱敏模型探针，默认不改变任何捕获设置。
