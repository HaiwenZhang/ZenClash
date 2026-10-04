# TUN 服务上游代码移植计划

- 修订日期：2026-10-03。

## v3 原子维护核心接线阶段

按用户确认的 §7.3，原生锁参考上游 `core/repair.rs` 与 `core/owner.rs`；SHA-256 与适配边界见 [UPSTREAM.md](../../crates/zenclash-service/UPSTREAM.md)。上游是排他锁，ZenClash 的会话共享准入、稳定文件身份、v3 能力握手和维护同连接宿主核验属于本项目适配，不能写成上游已提供的共享能力。

Linux 的有界固定 systemctl MainPID、macOS 系统框架 typed PID 与固定只读 helper 查询也属于项目适配，没有新增 busctl 要求、Rust 依赖或持久化结构。新宿主在提交前再次核对管理器 PID；旧 journal 不重新启动旧 helper。Windows 311、普通 Linux 270 项及三服务目标 check/CI lint 通过；首审 P1/P2 实际回归失败后修复，第二轮最终 PASS（2/2）。macOS SDK 链接/原生 API、完整 loaded/inactive 归属及包拥有旧服务迁移仍待完成，详细证据见 [实施记录](tun-service-progress.md)。以下保留此前日志和固定注册批次的 v2 历史证据。

## Service 日志流参数适配

2026-10-03 将 ZenClash 的日志等级与格式贯通具名 `SubscribeLogs` 协议操作、core transport 和内核 WebSocket。上游 `GetClashLogs` 读取 stdout 日志环，没有这条 WebSocket 参数透传实现，因此本项属于现有代码适配，不新增虚假的上游复制记录。协议保持 v2，旧 helper 拒绝新操作时日志失败，不重试旧操作或切换 Local。

普通用户真实 Windows Mihomo v1.19.30 验证暴露命名管道实例重建期间的 `ERROR_PIPE_BUSY`；生产路径仅对此码异步等待，打开、同句柄 PID 验证与握手共用五秒期限。五项管道行为测试及真实日志专项已通过；完整检查和独立审查结果见 [实施记录](tun-service-progress.md)。这不代表三平台高权限服务与 TUN 已验收。

## Linux 固定 unit 与 macOS Enable 恢复

2026-10-03 按用户指定继续参考本地上游。macOS 直接适配 `src/bin/install_service.rs` 的 `enable → bootstrap` 短路顺序；源文件 SHA-256 为 `e47937a3069bd821c6fabc49dc29e27b1c9442c9071ec67725713a3cb2554fed`，新增来源行见 [UPSTREAM.md](../../crates/zenclash-service/UPSTREAM.md)。固定 plist 已有 `RunAtLoad=true`，不额外移植旧 `launchctl start`；已加载路径保留 kickstart。Start 在查询前及每次副作用前要求完整已批准 plist，缺失、被替换、Enable 失败均阻止后续 bootstrap。

Linux 沿用上游及现有 `systemctl` 主线，不引入 busctl。新增 [磁盘 unit 门禁](../../crates/zenclash-service/src/platform/unix/linux_registration.rs) 复用项目已有保护、固定句柄及模板检查，不标记为上游直接复制。对 `/etc` 与包拥有的 `/usr/lib` 固定 unit 做 16 KiB 有界完整 LF/CRLF 比对，维护 effect 前重验，应用不删包文件；早期门禁在私有目录和 journal 恢复之前执行。

首次审查发现原有“磁盘 unit 缺失便跳过 Stop”的危险边界：卸载、部署与恢复可能继续删除或替换仍被运行服务使用的文件。修复后，两个文件均缺失的 Stop/Unregister 先通过已有固定 `systemctl show` 查询简单属性；唯成功退出、无 stderr、唯一完整 `LoadState=not-found`、`ActiveState=inactive`、`MainPID=0` 允许继续，其他结果保留部署。查询清除环境覆盖，stdout/stderr 共用 64 KiB 预算，截止 30 秒；超时终止自有子进程并最多等待 5 秒回收。`show` 的机器解析属性依据 [systemd 官方文档](https://github.com/systemd/systemd/blob/main/man/systemctl.xml)，不解析复合 ExecStart 或诊断输出。

参数与维护行为回归先失败再通过；首次审查 P1 已修复，第二轮最终 PASS。本批完整测试及跨平台检查见 [实施记录](tun-service-progress.md)。磁盘模板和缺失分支的观察不证明已加载服务的实际执行归属；管理员并发修改不是由这次查询锁定，原子维护准入仍未完成。真机 launchd/systemd、授权和 TUN 由用户后续环境验收，不以交叉编译冒充。
- 对应 [三平台开发计划](tun-service-plan.md)；当前进度与验证见 [实施记录](tun-service-progress.md)。
- 开发方式：直接移植有用的上游实现及对应行为测试，再按 ZenClash 当前实现适配。以下是实施清单；分批记录验证结果，不能把单批移植作为完整功能验收。

## 1. 来源与复用单位

本地源码位于 [examples/clash-verge-service-ipc](../../examples/clash-verge-service-ipc/)，[Cargo.toml](../../examples/clash-verge-service-ipc/Cargo.toml) 声明包版本 `2.7.5`、edition `2024`、许可 `GPL-3.0`。现已核对本地仓库 HEAD 为 `89acd8d8cd9da5134e886abc4b04c33c6946691a`，SELinux 源文件及清单在该提交上没有本地修改；发布 tag 尚未核实。复制文件另记录内容摘要，不能把清单版本当作提交证明。本项目清单声明 `GPL-3.0-only`；移植保留上游版权、许可文本和来源记录，不改变项目许可。实际复制及适配记录见服务 crate 的 [UPSTREAM.md](../../crates/zenclash-service/UPSTREAM.md)。

以能够独立验证的模块或函数为单位复制到 `crates/zenclash-service`，同步移植适用的行为测试。复制前比对现有实现：已有同等能力时复用现有模块；有缺口时优先使用上游可用代码，避免再实现一套同责模块。替换后的冗余在该批修改中清理。`examples` 保留参考源码用途，不作为正式产物运行路径。

## 2. 优先移植清单

下表描述各批移植范围；完成及验证状态单独记录，具体目标函数在开始该批修改时确定。

| 上游入口 | 可复用内容 | ZenClash 接入与必要适配 |
| --- | --- | --- |
| [service.rs](../../examples/clash-verge-service-ipc/src/bin/service.rs)、[install_service.rs](../../examples/clash-verge-service-ipc/src/bin/install_service.rs)、[uninstall_service.rs](../../examples/clash-verge-service-ipc/src/bin/uninstall_service.rs) | 三平台服务注册、状态/停止回调、安装卸载；Windows `configure_windows_service_recovery` 和删除完成等待 | 映射到现有 `platform`、`installer` 和服务入口；保留固定批准路径、安装事务、包管理文件归属与失败恢复。SCM 恢复的是服务待命进程，不自动恢复失去所有者的内核 |
| [management.rs](../../examples/clash-verge-service-ipc/src/management.rs)、[channel.rs](../../examples/clash-verge-service-ipc/src/channel.rs) | 三平台授权与身份常量的组织方式 | 对照现有授权入口和平台路径集中定义，移植缺失部分；替换所有 Clash 名称、IPC、应用标识和内核名，保留实际连接身份验证及用户取消语义 |
| [paths/windows.rs](../../examples/clash-verge-service-ipc/src/core/paths/windows.rs)、[trusted_core_location.rs](../../examples/clash-verge-service-ipc/src/core/trusted_core_location.rs) | `registered_executable`、`check_service_registration` 和服务注册的 `review_security` | 复用原生参数解析、own-process/LocalSystem 与可信 owner/危险 ACE 判断；限定固定 ZenClash helper、唯一 `run` 参数及无依赖，同一 SCM 句柄先鉴别再修改，不复制 alternate state directory 或卷根目录例外 |
| [installer/selinux.rs](../../examples/clash-verge-service-ipc/src/bin/installer/selinux.rs) | `ensure_executable_label`：检测 SELinux 并对执行目录标记，标记失败单独传播 | 接到 Linux 受保护副本暂存/安装流程，限定 ZenClash 执行目录；在 enforcing/permissive/未启用及命令失败场景验证，不关闭 SELinux |
| [install_service.rs](../../examples/clash-verge-service-ipc/src/bin/install_service.rs) | `classify_launchd_service_probe` 及四项分类测试 | macOS start/stop/unregister 共用明确不存在/已加载分类；未知结果零平台修改，以并发有界 Unix 管道代替上游无界输出，不以 label 存在代替实际注册归属 |
| [runtime_generation/staging.rs](../../examples/clash-verge-service-ipc/src/core/runtime_generation/staging.rs) | `plan_stage`、`declared_remote_providers`：差异计划、URL 变化使缓存失效、未知文件保留规则 | 输入改为普通权限准备并上传后已持有的资源信息；保持 immutable 资源快照、内容身份和预算。时间戳仅作优化，不能授权 root 读取用户源路径 |
| [runtime_generation/assets.rs](../../examples/clash-verge-service-ipc/src/core/runtime_generation/assets.rs)、[staging.rs](../../examples/clash-verge-service-ipc/src/core/runtime_generation/staging.rs) | 路径/Windows 别名规则、`runtime_cleanup_retry_delay` 和 `while_the_core_lets_go` 等有界重试 | 合并到现有资产路径、保护目录、原子替换和显式清理所有者；保留 active/candidate 共用资源引用及有界退休槽，不删除仍被使用的资源 |
| [runtime_generation/readback.rs](../../examples/clash-verge-service-ipc/src/core/runtime_generation/readback.rs) | `read_runtime_file`、`read_chunk`：manifest 声明范围内的 provider 缓存分块回读 | 接现有 ServiceClient 会话、revision/generation、帧及总字节预算；复用当前固定句柄、无链接/重解析点校验，拒绝配置秘密和任意系统文件回读 |
| [assets.rs](../../examples/clash-verge-service-ipc/src/core/runtime_generation/assets.rs)、[runtime.rs](../../examples/clash-verge-service-ipc/src/core/runtime.rs)、[process.rs](../../examples/clash-verge-service-ipc/src/core/process.rs) | 稳定所有者运行目录、准备/落盘分离、残留进程记录及进程出生身份核验 | 接当前会话所有者、稳定 home 与退出回收；配置密钥、上传快照和内核可写状态分开。Windows 保留现有真实句柄和 Job 回收，Unix 保留原生身份及停止确认 |

上游资源目录代码可以帮助保留已生成的缓存和节点状态，但本地源码中尚未找到旧 Tailscale/ZeroTier 身份导入实现。旧身份授权导入、HTTP provider 更新中相对 TLS/SSH 文件映射，以及服务可写目录的动态预算仍需单独实现和验证，不能因移植稳定目录或缓存回读而勾选完成。

## 3. 保留的边界与行为差异

- `CoreSession` 继续拥有配置业务提交、后端切换、网络恢复和退出策略；GPUI、捕获协调及现有用户数据存储继续沿用。[core 接入文档](tun-service-core-integration.md) 是适配契约。
- 保留已验证连接上的身份校验、会话凭证、generation、请求序号、私密内核控制器和受限 API。上游协议 DTO 经适配接到当前接口；不将其整套协议和依赖清单直接替换进工作区。
- 上游 `stage_runtime` 会修改活动资源，不能原样替代本项目 prepare/apply/业务保存/commit 的事务。差异规划可以直接移植，资源落盘必须保持上一 accepted 配置和资源可恢复，见 [部分配置事务](tun-service-partial-config.md) 与 [资源布局](tun-service-resource-layout.md)。
- 上游 `desired::restore_desired_state` 的自动恢复策略需调整：系统启动或服务重启后先恢复待命及清理残留，内核启动要求有效应用会话；正常退出后内核必须停止。配置持久化失败不能仅记录告警后报告提交成功。
- 不带入针对 Clash Verge 的 legacy repair/cleanup、测试执行文件放行或服务入口失败后的无授权备用运行行为。所有安装/卸载只操作明确属于 ZenClash 的文件和注册。
- 不整体引入 `kode-bridge`、`windows-service`、上游 Git 依赖或新持久化结构。优先适配已有依赖；确有必要时先说明现依赖不足、维护与体积成本、迁移影响并确认。服务私有协议或元数据变化需记录兼容和修复路径。

## 4. 实施顺序与验收

1. **收尾业务接线**：服务端 partial PATCH、唯一 owner 生命周期及 core 部分事务已完成阶段回归和两轮独立审查；首次 Start 与同一捕获准入中的后端交接核心阶段已通过首审。首次开启的 Manager/页面/托盘收据分类批次和随后 main/UI 启动批次分别结束第二轮审查，启动 P1/P2 已修复。Windows 完整 core 534、Linux 完整 core 577 项通过，各 2 项忽略；Windows 完整 UI 库 294 项及 binary 11 项通过，core/UI check、标准严格 clippy 和 Linux core 完整 CI lint 通过。Unix 更新回滚夹具同步修复通过独立首审，Linux 启动相关 29 项及真实普通 Mihomo 自动生命周期 2 项复验通过。UI 附加 lint 五项问题、无 owner 恢复界面及修复/卸载资源物化方案仍待收尾，维护仍需高层事务；真实高权限服务和 TUN 尚未验收。
启动后的 Managed Local Mihomo 最终有效配置 TUN 准入尚未实现；完整应用、PATCH 与重启链缺口已作只读核对，处理策略与新增行为回归待确认，见 [core §5.6](tun-service-core-integration.md#56-managed-local-配置的-tun-准入缺口)。不能把首页统一服务命令或启动拒绝结果当作这些链路已受保护。

2. **形成移植记录**：逐批列出源文件/函数、版本及可确认提交、目标模块、保留的许可、适配差异和预期行为。根据当前缺口选择最小完整批次，不为凑复用比例复制无关代码。
3. **资源基础复用**：复制纯差异计划及其行为测试、路径规则和有界重试，先接同一会话内的 accepted 缓存继承。用已完成上传的 SHA-256/长度和 provider URL 判断身份；Validate 不提前冻结上传，formal 后不可变。相同 URL 的缓存只在可安全映射已批准资源时继承，新候选独立落盘，旧 accepted 不受影响；超预算或不能安全映射时走已有受限下载与失败恢复。验证 URL 改变、重复目的地、Windows 别名、共享资源不误删和总重试预算耗尽。
4. **平台补缺与验收**：SCM 恢复/删除确认和 SELinux 标记首批适配已完成，继续核对三平台安装维护中的缺失部分；保留现有鉴权、原子安装和失败恢复。真实安装、授权及停止在目标系统验收。
5. **资源与生命周期接入**：增加声明范围内缓存回读及跨 revision 状态保留，接 core 配置事务和应用级服务操作；身份首次导入、动态资源和部分修改分别覆盖成功、取消、响应丢失及恢复路径。
6. **完整验收**：按主计划完成 Windows、macOS、Linux 的真实服务、Mihomo、TUN、退出、升级和卸载矩阵；再更新完成标记。

每批先运行最近的行为测试和 `cargo check`；Rust 改动超过三十行按项目规约做独立审查，同一修改最多两轮。适用的 fmt、test、严格 clippy 与 workspace 检查沿用 [主计划验证命令](tun-service-plan.md)。源码复制、上游 mock 测试、交叉检查和打包替身通过均不能替代 ZenClash 真实内核或三平台实机验收。

## 5. 移植批次记录

### Linux SELinux 标记

已将上游 `ensure_executable_label` 的检测、permissive 标记及固定 `chcon` 回退逻辑适配到 [platform/unix/selinux.rs](../../crates/zenclash-service/src/platform/unix/selinux.rs)。使用现有 `io::Result` 和有界原生命令，不新增依赖。ZenClash 安装根目录同时包含元数据和运行数据，因此注册前只标记固定 helper 与内核文件；bootstrap 在执行固定摘要的临时 helper 前也进行标记。

Windows 上的独立 Rust 行为测试 **8 项通过**，覆盖 enforcing/permissive、未启用、检测失败、未知模式、固定命令回退及拒绝权限/命令失败回退；实际 bootstrap 字符串通过 Git Bash 语法检查。合并后的 Windows service 完整测试 **160 项**、check、严格 clippy 与 fmt 通过。2026-10-02 重跑 Linux GNU x64 和 macOS Intel 的 service all-targets/all-features 交叉检查均通过，Linux 专属标记代码已编译；这些结果不包含目标系统链接或运行。本批首次独立审查未发现新增功能性 bug 或重大漏洞；受保护文件标记失败进入已有维护恢复，恢复失败保留待处理事务。首次包 helper 的直接授权执行、enforcing 下服务及 Mihomo 启动仍待实机验收，本批暂不标记验收完成。

### Windows SCM 恢复与删除确认

已将上游 `configure_windows_service_recovery` 与卸载等待逻辑适配到 [platform/windows/scm.rs](../../crates/zenclash-service/src/platform/windows/scm.rs)，复用已有 `windows-sys`，不新增依赖或持久化格式。恢复动作采用 5/10/30 秒重启、24 小时重置及非崩溃失败标志；恢复服务待命进程，内核启动仍要求有效应用会话。

维护停止先禁用服务自动启动，再查询并固定宿主身份、停止服务及等待真实宿主退出；显式启动或重新注册恢复自动启动配置。卸载在删除后释放本调用方服务句柄，再有界轮询消失；marked-for-delete 保持待处理，超时和原生错误不能报告成功。保留现有安装事务、Job 回收和失败回滚，不复制上游针对 Clash 的旧路径清理。

新增 **6 项行为先失败**，修复后 SCM 模块 **13 项通过**；Windows service check、严格 clippy、相关格式与差异检查通过，首次独立审查未发现功能性 bug 或重大漏洞。该批不包含真实 SCM/UAC 安装、升级、删除或网络改动，仍待平台验收。

### Windows SCM 注册身份

移植来源、文件摘要与适配差异已记入 [UPSTREAM.md](../../crates/zenclash-service/UPSTREAM.md)，对应上游 `paths/windows.rs` 和 `trusted_core_location.rs`。目标为 [registration.rs](../../crates/zenclash-service/src/platform/windows/registration.rs)、[security.rs](../../crates/zenclash-service/src/platform/windows/security.rs) 及现有 SCM 维护路径；沿用 `windows-sys`、安全描述符和 SID 工具，不新增依赖或持久化格式。

注册配置只接受固定保护目录中的 helper 与唯一 `run` 参数，own-process、LocalSystem、无依赖且无 load-order group。原生 `QueryServiceConfigW` 两次读取使用对齐且至多 8 KiB 的已初始化缓冲区，指针、UTF-16 终止和参数边界均校验；成功不依赖 API 未保证的所需字节输出。相同 SCM 句柄先核对可信 owner 与 DACL，再允许已有注册更新、启动、停止或删除；新建后也先核对，再配置恢复。普通用户的配置/删除/ACL 修改权、null DACL、未知 ACE 和非固定路径均拒绝，只有原生 1060 允许新建。既有固定保护根目录中缺失 helper 的情况可进入原有维护，根目录缺失则拒绝，不发现或接管其他安装目录。

原样复制的原生参数解析断言实际失败，适配固定 helper 加 `run` 后新增 **11 项行为通过**。完整 Windows service **209 通过、0 失败**，all-targets/all-features check、默认及服务端完整 CI 严格 lint、fmt、差异检查和首次独立审查通过（**1/2**）；最新 Linux GNU x64/macOS Intel service 交叉 check 通过。实际 Windows release helper 构建及 `--version` 退出码 0 已确认。原生参数解析、SDDL、回调和交叉检查不等于 SCM 安装、UAC、卸载或 TUN 验收，后续按 [原生验收入口](tun-service-native-validation.md) 执行。

### macOS 服务查询分类

已直接移植上游 `classify_launchd_service_probe` 与四项适用测试到 [launchd_probe.rs](../../crates/zenclash-service/src/platform/unix/launchd_probe.rs)，并接入 [macos.rs](../../crates/zenclash-service/src/platform/unix/macos.rs) 的 start/stop/unregister。退出码 0 表示已加载；只有 113 加明确 `Could not find service` 诊断才表示不存在。其他退出码、读取失败、无效 UTF-8、超限或超时均拒绝操作；未知状态不触发 kickstart、bootstrap、bootout 或删除注册，bootout 失败保留 plist。输出不进入用户诊断。

生产查询仅用于 Unix 维护 worker：并发读取 stdout/stderr，各限 64 KiB，查询限 30 秒；取消管道读取后终止直接子进程，最多等待 5 秒回收，不等待继承管道的后代结束。没有新增依赖、持久化结构或授权操作。来源与文件摘要已核对并写入 [UPSTREAM.md](../../crates/zenclash-service/UPSTREAM.md)。

实际可编译行为红 **2 通过、5 失败**，修后 Windows 分类/生产调度回调 **10 项通过**。普通 WSL Linux 用户下整组 **16 项通过**（含测试子进程入口，其中 **5 项**实际捕获覆盖双管道、超限、编码、超时和继承管道）；Windows check、macOS Intel 交叉 check、两目标完整 CI 严格 lint、相关 fmt 和差异检查通过，首次独立审查 **PASS，1/2**。Windows 不运行 Unix 生产捕获，macOS 交叉结果不等于 launchd 执行。真实目标系统验收及已加载 label 的实际程序/参数归属仍待完成。

### macOS 固定 plist 维护准入

[macos_registration.rs](../../crates/zenclash-service/src/platform/unix/macos_registration.rs) 复用本项目既有保护路径和固定句柄读取；上游没有可直接复制的完整 macOS 注册归属门禁，来源说明保留该差异，不伪称复制。当前项目模板历史内容未变，只认可完整 LF 或完整 CRLF 字节，不解析或改写未知定制。

预检位于 `run_maintenance` 取得固定路径之后、创建私有目录之前，早于锁、journal 恢复及部署交换；register 和每个 launchd 维护效果前再次检查。真实缺失必须通过父目录保护及再次观察；存在的文件采用无链接、非阻塞固定句柄，有界读取 16 KiB，并核对文件身份及元数据前后保持一致。读取失败、链接/FIFO、超量或未知内容均保留文件并拒绝效果。

共享生产调度的普通文件回归修前 **3 通过/5 失败**，修后 Windows **11 项**、普通 WSL Linux **14 项**通过；最新完整 Windows service **230 项通过**。Windows/macOS Intel service all-targets/all-features check、完整 CI 严格 lint、相关 fmt/diff 通过，首次独立审查 **PASS（1/2）**。文件与回调测试不是管理员 CLI/launchd 实测，磁盘模板一致也不能证明已加载 job 的实际 program/args 归属；该完整门禁与目标平台验收继续保留待办。

### 后续批次状态

资源基础首批已将上游 `plan_stage`、`declared_remote_providers` 及有界重试模型适配到 [runtime_manifest.rs](../../crates/zenclash-service/src/runtime_manifest.rs) 和 [runtime.rs](../../crates/zenclash-service/src/runtime.rs)，来源摘要已记录在 [UPSTREAM.md](../../crates/zenclash-service/UPSTREAM.md)。manifest 只保存在内存，以已完成上传的 SHA-256 和长度代替用户路径、mtime；准备时复核固定文件内容。同 URL 缓存按实际字节计数后复制到独立候选，只映射声明且内容身份相同的批准资源；旧 accepted 和共享 partial 资源不变。

上传篡改与缓存未继承的三项行为先失败后通过；runtime **34 项**、Stage→校验→释放的实际生产调度 **1 项**通过，首次独立审查通过。该合并阶段 service 完整测试 **179 项**、默认/服务端严格 clippy 和 Linux/macOS service 交叉 check 通过，早于后续授权分类根修；这些计数不代表当前工作区全部差异已验证。最多三次等待（25/50/100 ms）由整个 stage 共用；未知路径、资源改变或解析预算不足时保留受限下载，不以管理员权限读取被引用的任意文件。

| 批次 | 当前状态 | 收尾条件 |
| --- | --- | --- |
| Windows SCM 恢复与删除确认 | 源码适配、13 项行为测试与首次独立审查完成 | 真实 SCM 安装、异常恢复、维护替换、外部句柄阻止删除及失败回滚验收 |
| Windows SCM 注册身份 | 上游适配、新增 11 项及完整 Windows service 209 项通过；check/完整 CI lint、交叉 check 和首次独立审查通过 | 真实注册、受保护安装修复及 native SCM 权限验收；GUI 卸载桥接仍需实际 owner 准入和共享账户策略 |
| macOS 服务查询分类 | 上游分类及三维护入口已接入；Windows 10 项、Linux 整组 16 项、check/完整 CI lint、相关 fmt 与独立首审通过 | 真实 launchd 查询/停止/卸载和 loaded label 实际执行归属验收 |
| 资源差异规划、路径规则与重试 | 内存 manifest、同会话缓存继承首批行为与独立首审通过 | 真实 Mihomo 下载/校验及目标系统文件竞争待验收；继续保持共享资源与上一 accepted 配置可恢复。该批不宣称跨会话稳定 home 已完成 |
| provider 缓存回读 | 修后相关行为 19 项、完整服务 198 项、check/完整 CI lint 和 Linux/macOS service 交叉 check 通过，两轮独立审查结束 | 高层普通权限保存及启动/恢复和真实 Mihomo 验收待完成 |
| 跨会话稳定 home 与节点身份 | 尚未完成 | 稳定 home 与私密配置分离；旧节点身份导入、相对 TLS/SSH 文件映射及动态预算单独验证 |

缓存回读已按上游“先验证声明，再查找缓存、按块读取”的模型写入 [provider_readback.rs](../../crates/zenclash-service/src/provider_readback.rs)，具名 `BeginProviderCacheRead`、`ReadProviderCache`、`FinishProviderCacheRead` 已进入当前未发布协议源码。接口不自动停止内核；高层事务必须先取得新鲜的停止确认及准确的已提交 revision。固定版本 Mihomo 会原地覆盖 provider 缓存，运行期间的文件元数据稳定不足以证明写入结束，不能宣称得到一致快照。

当前源码采用一次一个快照、每块 256 KiB、单快照 128 MiB、proxy 解析 4 MiB，以及整批 15 秒、256 MiB、256 次尝试的预算。finish、过期、revision 改变和会话释放回收快照，finish 不重置整批预算。服务调度与取消已完成下述行为测试及两轮审查，高层普通权限保存、启动/恢复和真实内核验收仍待完成，详见 [资源布局](tun-service-resource-layout.md)。

2026-10-02 本批首审前回读相关测试 **16 项通过**，服务完整测试 **195 项通过**；Windows service all-targets/all-features check、默认与服务端的 CI 全部额外严格 lint、Linux GNU x64/macOS Intel service 交叉 check 及局部 fmt 通过。覆盖排队取消零准入、已准入调用方取消后保持状态门栓直到文件复制结束、停止/revision/candidate 限制、不可变分页、偏移重放、结束/释放及总预算。这些存储/调度 fixture 不证明原生传输、真实 Mihomo、缓存持久化或 TUN 验收。

首审发现 **1 项 P2**：Start 在前置检查前清除回读预算，拒绝的请求也能重新获得整个时间和字节预算。新增 **3 项实际 State 行为测试先失败再通过**，覆盖未知 revision、未校验配置和待处理维护记录下的拒绝，分别核对耗尽与过期预算保持不变。清理改到真实 `Kernel::start` 成功返回后；启动失败不重置预算。修后回读 **19 项通过**、all-targets/all-features check 和全部 CI 额外严格 lint 通过，第二轮最终审查通过。随后最后增量的完整服务回归 **198 通过、0 失败**，Linux GNU x64/macOS Intel service all-targets/all-features 交叉 check 均通过；真实成功启动/停止后缓存回读仍待 Mihomo 验收。

## Windows runtime 删除竞争重试

2026-10-02 适配上游 `runtime_generation/assets.rs::remove_runtime_directory` 与 `runtime_cleanup_retry_delay`，接入现有 [安全递归删除](../../crates/zenclash-service/src/installer.rs) 与 [retired 清理](../../crates/zenclash-service/src/server.rs)。源文件 SHA-256 为 `49f710ffb4dee32980f91b1f126d2afa39fe805c00a99d4d9b5453eb7eca8abd`，完整来源见 [UPSTREAM.md](../../crates/zenclash-service/UPSTREAM.md)。本批没有移植原子替换重试或上游脱离所有者的后台清理。

仅 Windows 原生删除的 `ERROR_SHARING_VIOLATION`、`ERROR_DELETE_PENDING`、`ERROR_USER_MAPPED_FILE` 可以重试。一次 `cleanup_retired` 的全部目标目录共用三次等待（25/50/100 ms）；整个启动残留 stage 扫描也共用一次预算。175 ms 是一个 wave 请求的累计等待量，不包含保护检查、枚举和原生文件操作耗时，不是请求或清理的总截止时间。Release、Stage 可能依次执行多个清理 wave；Release 还会清理会话目录，因此整个 RPC 的累计等待可能超过 175 ms。

每次原生重试前重新验证保护边界；保护校验、权限拒绝、链接与未知错误不进入重试，即使校验错误的原生码恰好是 32。重试不重复枚举或增加条目计数，保留原深度 24 层、条目 32768 项限制。失败继续保留 retired owner，accepted/candidate 仍引用的共享资源不删除。Unix 删除不增加等待，不新增依赖、目录、用户数据格式或协议。

Windows 普通临时文件的实际 no-delete-share 句柄取得 **2 项行为红、1 项原有语义绿**，随后新增及原用例 **7 项通过**。覆盖释放句柄后成功、持续占用预算耗尽、跨目录共用预算、计数不增加、真实 ACL 删除拒绝、重试前保护失败，以及 State 的 retired/shared 资源保留。修后 Windows 完整 service **237 项通过**，all-targets/all-features check、默认与服务端完整 CI 严格 lint、所属 Rust 文件格式与 diff 检查通过；首次独立审查通过（本批 1/2）。Root 另执行包含本批增量的 Linux GNU x64、macOS Intel service all-targets/all-features 交叉 check，两项退出 0。这些实际文件系统测试和交叉编译不包含管理员安装、真实 Mihomo 退出后的文件竞争或 TUN 验收。

## Windows runtime 原子替换竞争重试

独立于上一删除批次，本批调整上游 `staging.rs` 的 `while_the_core_lets_go`、`replace_staged_file`，接入 [runtime_replace.rs](../../crates/zenclash-service/src/runtime_replace.rs) 与既有 [资源及配置准备](../../crates/zenclash-service/src/runtime.rs)。来源 SHA-256 为 `2f26a6903cb3574c604d1855ff0790dd24bdb85c3ede156fba4e16ca14c6eee6`；完整归属及适配说明见 [UPSTREAM.md](../../crates/zenclash-service/UPSTREAM.md)。不新增依赖、目录、持久化结构、协议或资源所有者。

普通 Windows 文件基线实际观察：源文件被 no-delete-share 句柄占用时，`MoveFileExW` 返回 32；目标占用时返回 5；纯 DELETE/DELETE_CHILD 权限拒绝也返回 5。只读目标、目标 ACL、源 ACL、父目录 ACL 分别与目标占用同时存在时，Move 都返回 5，而目标 DELETE-only 探测均返回 32。这些基线不算行为红，也不能证明此前 Move 失败只有共享占用一个原因。

原生替换失败为 32 或 1224 时允许有限等待；失败为 5 时，只有紧随该失败的固定目标 DELETE-only、share READ/WRITE/DELETE、`OPEN_EXISTING`、`OPEN_REPARSE_POINT` 探测也返回 32，才允许等待。探测不带 DELETE_ON_CLOSE，探测成功不代表替换成功；仅实际 Move 成功才返回成功。明确的保护、链接、非普通文件或只读检查失败在原生错误分类器外拒绝。纯权限失败且无目标共享证据不等待；混合权限与目标共享障碍可以有限等待，耗尽后原样失败，绝不修改 ACL、属性或权限。不能将这项行为描述成任意 ACL 错误均零等待。

临时文件仅 create-new、写入和 sync 一次，保留拒绝其他 WRITE-open、允许自身 DELETE/rename 的写句柄，重试不重新复制或散列大型资源。每次原生尝试前复核同目录的受保护命名空间、普通文件/只读属性及原句柄 FileID。目标保护查询只把明确 NotFound 视为缺失，Unknown 保留错误并零 Move、零等待。下层失败保留完整临时新字节与目标旧字节；外层沿原有 best-effort 临时清理契约，清理失败仍由既有 runtime/retired 所有者负责。

单次 materialize 的所有 asset、provider/cache 和末尾配置替换共用三次等待（25/50/100 ms）。尚未正式物化的 validation 资源准备同样共用一份预算；GeoData 四个固定文件也共用一份，partial 配置 fork 使用自己的配置准备预算。一次准备中的缓存打开与替换预算独立，各自最多请求 175 ms 睡眠，累计最多 350 ms；多次准备调用可能再累计。上述数字不含保护检查及原生 I/O 时间，不是 wave 或 RPC 的总截止时间。Unix 保持一次 rename 和一次父目录 sync；rename 后 sync 失败传播原错误，不重命名、不重新写临时文件，也不承诺此时目标仍为旧字节。

修前释放源/目标占用的两个真实行为测试分别以 32/5 失败，过滤命令编译成功，**6 项通过、2 项失败**（通过项含 2 个 journal 近邻）。最终替换相关 **15 项通过**，包括生产临时写句柄拒外部写入但允许自身替换、跨两个目标共享预算、耗尽后的完整字节、纯 ACL 与混合障碍、保护变化、临时身份替换及目标保护查询 Unknown。Windows 完整 service **252 项通过**，all-targets/all-features check、默认及服务端完整 CI 严格 lint、所属 Rust 文件 fmt/diff 检查通过。首审发现目标 `exists()` 吞掉 Unknown 的 P2，已修复；第二轮最终审查通过，本批 **2/2 封闭**。Root 对本批 Linux GNU x64、macOS Intel service all-targets/all-features 交叉 check 均退出 0。这些普通权限文件测试与交叉编译不证明 SYSTEM 保护目录、管理员安装或真实 Mihomo 占用场景验收。

## Unix rename 后失败边界回归

2026-10-02 新增 [普通权限回归](../../crates/zenclash-service/src/runtime_replace_unix_tests.rs)，仅添加测试和 Unix test module hook，没有修改生产替换路径，也没有重开已封闭的 Windows 原子替换两轮审查。WSL Ubuntu 26.04、UID 1000、`TMPDIR=/tmp` 下，目录仅保留 owner write/search 权限，真实 rename 成功后父目录打开返回 PermissionDenied；源文件已消失，目标保持完整新字节，原错误返回。guard 在正常及 panic 路径恢复原权限并清理唯一自建临时目录。

实际专项 **1 项通过**；此前错误过滤器执行 0 项，不作为验证证据。Windows/macOS Intel service all-targets/all-features check、完整 CI 严格 lint、所属 fmt/diff 通过，新验证批次独立首审 **PASS（1/2）**。随后 Root 运行当前 Linux service 完整原生 ELF，**207 通过、0 失败、0 忽略**。沙箱内初次 WSL 启动返回 E_ACCESSDENIED；获准后通过相同已核对 runner 以普通用户 zen 执行完整测试，退出 0，没有安装服务或修改网络。

该回归覆盖 rename 后的父目录 open 失败，不证明 sync_all 系统调用故障、崩溃持久性或 macOS 原生运行；Linux 完整库结果也不能代替 systemd/Polkit、管理员安装和真实 Mihomo/TUN 验收。
