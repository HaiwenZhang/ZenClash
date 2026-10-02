# TUN 服务与 core 接入梳理

- 修订日期：2026-10-02。
- 对应计划：[三平台 TUN 服务开发计划](tun-service-plan.md) 的 P0、P3 与 P4 接入边界。
- 实现方向：优先移植上游服务代码并适配本节契约，具体来源与批次见 [上游代码移植计划](tun-service-upstream-migration.md)。`CoreSession`、配置事务、捕获协调和 GPUI 保留现有责任；移植尚未完成。
- 状态：共享传输、资源 bundle、完整配置事务、唯一 owner 生命周期和部分配置事务已有阶段验证。首次安装并开启的 Manager/页面/托盘批次与随后 main/UI 启动批次分别通过第二轮审查；启动首审 P1/P2 已修复。Windows 完整 core 534 项、Linux 完整 core 577 项通过，各 2 项忽略；Windows 完整 UI 库 294 项、binary 11 项通过。core/UI check、标准严格 clippy 和 Linux core 完整 CI lint 通过；UI 附加 lint 仍有五项问题。恢复界面、修复/卸载、高层缓存保存与三平台真实验收尚未完成。详细证据见 [实施记录](tun-service-progress.md)，阶段结果不代表完整应用验收。
- 当前验证环境：Windows，以及 WSL Ubuntu 26.04 普通用户下的 Linux ELF 测试。两平台真实 Mihomo 普通生命周期分别 2 项通过；macOS 及三平台原生高权限服务、授权、TUN 与退出验收仍待执行。

## 1. 已有所有者与不能丢失的行为

| 所有者 | 接入前基线与约束 | 服务接入后保留的责任 |
| --- | --- | --- |
| `CoreSession` | `client`、`Option<Arc<MihomoProcess>>`、`transition`、已提交 profile/overrides、generation、shutdown、network suspension、恢复状态 | 唯一业务切换与恢复策略；同一个 transition gate 串行配置、换后端、升级与退出 |
| `MihomoProcess` | 本地 child handle、固定 launch config、stdout/stderr、有界日志、exit reason；同步底层进程操作放在后台 | 继续作为本地进程实现；独立本地 API 不因服务接入而假装拥有远程进程句柄 |
| `MihomoClient` | HTTP client、endpoint/secret、mutation gate、validator、共享连接缓存与 delay semaphore | Direct/Service 两种通信入口；所有 clone 看见同一个当前通信绑定；服务模式不能获取高权限 HTTP secret/address |
| `ControlledConfigStore` | 用户目录的 override/runtime cache、listener fallback、写租约与提交事务 | 保持源订阅不变、持久化提交/回滚顺序和准确的 `attempted` 语义；服务资源包与用户缓存成对提交 |
| `TrafficCaptureSession` | 用户捕获意图、业务门锁、`ProductionCaptureBackend`；协调系统代理/TUN、失败回滚 | 仅显式 TUN 请求触发服务安装；安装、后端切换、配置开启和实际网卡/路由回读形成事务 |
| `OperationalStatus` | 后台采集 process/controller/config/TUN/路由，带 generation 的内存快照 | 页面只读已准备的观察；服务不通表示未知/失败，不能把旧 `running=true` 或已安装当作就绪 |
| `TrafficMonitor` / `LogMonitor` | 当前只持 HTTP endpoint，后台 WebSocket、watch generation、取消、有界历史 | 改为读取共享 client 的当前流入口；后端切换时重连，继续拒绝旧 generation 的帧 |
| `ZenClashApp` | 应用级 core/capture/status、Tokio runtime、页面、托盘、quit task | 持有安装/修复/卸载任务和待完成 TUN 意图；页面切换或窗口隐藏不销毁业务任务 |
| `zenclash-service` | 经原生校验的会话、受保护内核与 runtime、租约、子进程、受限 API | 只执行和报告事实，不与 `CoreSession` 竞争自动恢复策略；未知 IPC 结果不触发第二份内核 |

引用实现：[CoreSession](../../crates/zenclash-core/src/core_session.rs)、[本地进程](../../crates/zenclash-core/src/process.rs)、[客户端](../../crates/zenclash-core/src/client.rs)、[配置事务](../../crates/zenclash-core/src/controlled_config.rs)、[捕获协调](../../crates/zenclash-core/src/traffic_capture.rs)、[应用所有者](../../crates/zenclash-ui/src/app.rs)。

当前实际 owner 由 [ControllerBinding](../../crates/zenclash-core/src/client/transport.rs) 随通信绑定一起发布，`CoreSession` 持业务准入、generation 与生命周期，应用级 `ServiceManager` 持维护命令及待开启意图。上表中的 `Option<Arc<MihomoProcess>>` 是接入前字段；当前 Session 和 UI 页面不另存一份可替代绑定的进程 owner。

## 2. 本地进程调用清单

下表记录接入前的生产调用点与迁移要求，字段和函数描述属于规划基线。当前 `CoreSession` 旧进程字段及 UI 持久旧进程引用已移除，唯一 owner 阶段已有验证；表中的旧字段不能作为当前源码仍存在的证据。测试中的 fixture 构造不作为额外业务所有者，最新差异与未完成项见 [实施记录](tun-service-progress.md)。

| 文件 | 函数或字段 | 现有进程调用和接入要求 |
| --- | --- | --- |
| [process.rs](../../crates/zenclash-core/src/process.rs) | `spawn`、`restart`、`restart_with_write_lease` | 本地创建/停止 child、预检配置、附加日志；保持本地实现 |
| 同上 | `restart_and_wait_until_with_lease`、`wait_until_ready_until` | 后台重启，轮询 `/version`，取消或未就绪时停止；服务 facade 提供等价异步行为 |
| 同上 | `endpoint`、`launch_config`、`config_validator`、`write_scopes` | 仅表示真实本地元数据；不能用假的 HTTP endpoint、假的本地 binary 或 GUI 直读服务私密目录填空 |
| 同上 | `snapshot`、`recent_logs`、`is_running`、`stop_async`、`Drop` | 本地观察与回收；服务版观察读缓存/后台刷新，显式释放待 service 确认 |
| [main.rs](../../crates/zenclash-ui/src/main.rs) | `bootstrap_core` | 外部 controller 分支 `process=None`；本地发现、生成缓存、预检、分配普通 controller、`spawn`、`wait_until_ready`、失败 stop |
| 同上 | main 启动构造、`remember_working_core` | 创建 client/validator、session、traffic/log monitors、permission manager；记录真实源 binary。服务存在且已授权时应直接构造服务执行绑定，不能先开启旧 setuid 内核 |
| [core_session.rs](../../crates/zenclash-core/src/core_session.rs) | `open`、`open_with_config`、`start_supervisor_with_policy` | 当前 owned 判定由 `process.is_some()`；新 facade 仍保留 external=`None` 与 managed=`Some` 区分 |
| 同上 | `stage_profile_application`、`apply_patch`、`activate_profile` | 热载不确定且为 managed 时使用重启回退；必须区分明确服务拒绝与提交结果未知 |
| 同上 | `maintain_with_timeout`、`require_managed_restart` | stop/restart、清 network suspension、复位 lifecycle、推进 generation；服务 stop 不等于 session release |
| 同上 | `install_release` | 下载在 transition 外；执行激活、版本回读、回滚在 transition 内；下一节列服务副本授权差异 |
| 同上 | `write_scopes`、`acquire_process_write_lease` | 只约束用户进程的本地写入；服务端 staging/revision 不能依赖这个进程内租约 |
| 同上 | `shutdown`、`snapshot`、`managed_process_snapshot` | 先 shutdown flag 再等 transition；stop/reap 后才能结束。服务 shutdown 应 stop + release，并保留失败重试语义 |
| 同上 | `supervise_managed_core`、`recover_managed_core` | 有界 3 次恢复、capture release/reconcile、取消；服务 IPC 未知状态不得被当作 child 确实退出 |
| [automatic.rs](../../crates/zenclash-core/src/core_session/automatic.rs) | `request_shutdown`、`suspend_for_network`、`resume_after_network`、`restart_changed_source` | 网络恢复、源变化 generation/revision 校验、串行重启；所有路径使用同一 facade 与已提交后端，不自动退回普通本地内核 |
| [controlled_config.rs](../../crates/zenclash-core/src/controlled_config.rs) | `RuntimeApplicationRecovery::Restart`、`RuntimeApplicationTransaction::rollback` | 回滚启动缓存后再重启；服务回滚还需要上一已接受资源 revision |
| 同上 | `apply_json_update_with_restart[_for_session]`、`restart_with_overrides[_for_session]`、`stage_profile_restart` | 当前参数为 `Arc<MihomoProcess>`；迁移到同一 managed facade，不另造服务专用复制事务 |
| 同上 | `accept_runtime_payload_with_restart_for_session`、`restore_exact_runtime_payload`、`rollback_cache_and_restart` | 使用 validator、write scopes 和当前 applied bytes；服务重新准备 bundle，禁止重算已被删除或更改的原订阅来伪装恢复 |
| [core_update/workflow.rs](../../crates/zenclash-core/src/core_update/workflow.rs) | `install_prepared`、`stop_process`、`restart_process`、`restart_after_activation_failure`、`rollback_rejected_core` | 现有用户 binary 文件事务 + 真实 `/version`；服务升级必须经过授权副本部署，不能直接让 root 执行下载目录 |
| [operational_status.rs](../../crates/zenclash-core/src/operational_status.rs) | `ProcessStatus::from_snapshot`、后台 process 采样 | 通过 session 取得观察，不直接在 render 做 native IPC |
| [app.rs](../../crates/zenclash-ui/src/app.rs)、[runtime.rs](../../crates/zenclash-ui/src/pages/runtime.rs) | `AppServices.mihomo_process`、`RuntimePageServices.process`、`RuntimePage.process` | 迁移展示与操作到同一 facade/session 快照，避免页面继续持已被替换的旧本地 process |
| [runtime/mihomo.rs](../../crates/zenclash-ui/src/pages/runtime/mihomo.rs) | render 的 launch metadata、managed 判定、进程信息 | 展示真实执行方式；源配置路径与服务实际运行路径含义分开，服务控制器只显示语义状态 |
| [runtime/lifecycle.rs](../../crates/zenclash-ui/src/pages/runtime/lifecycle.rs) | 内核来源/路径读取 | 用可展示源 metadata；不能把服务私密 runtime 目录当成用户可写配置 |
| [mihomo/maintenance.rs](../../crates/zenclash-ui/src/pages/runtime/mihomo/maintenance.rs) | managed 检查与维护命令 | 同一 `CoreSession::maintain`，服务模式不伪装 external，不从页面单独启动另一份内核 |
| [system_proxy/actions.rs](../../crates/zenclash-ui/src/pages/runtime/system_proxy/actions.rs) | listener 失败日志提取 | 使用 facade 有界日志快照，不能保留旧本地日志引用 |

`MihomoProcess` 的生产类型引用集中在上述入口，以及 [core 的 re-export](../../crates/zenclash-core/src/lib.rs)。核心安装文件 [core_installation.rs](../../crates/zenclash-core/src/core_installation.rs) 的 `validate_core_binary` 是用户候选来源的检查，不是管理员批准副本的最终授权。

### 当前生命周期调用点补充（2026-10-02）

以下为排除测试后核对的当前入口，补充上面的历史基线；函数内的细分读写以源码为准。

| 场景 | 当前入口与实际 owner |
| --- | --- |
| 启动及绑定 | [main.rs](../../crates/zenclash-ui/src/main.rs) 的普通 Local bootstrap 在构造前 spawn、等待就绪、失败 stop；[client/transport.rs](../../crates/zenclash-core/src/client/transport.rs) 随绑定发布实际 owner；正式 Mihomo 的服务优先路由见 §5.4 |
| 绑定退休 | [client.rs](../../crates/zenclash-core/src/client.rs) 的退休 completion 在后台停止并释放 retired owner；生产切换由 [core_session/binding.rs](../../crates/zenclash-core/src/core_session/binding.rs) 的 `switch_runtime` 准入，不由页面持有旧进程 |
| 配置及日常维护 | [core_session.rs](../../crates/zenclash-core/src/core_session.rs) 从 pinned binding 取得当前 Local 后，进入 [controlled_config.rs](../../crates/zenclash-core/src/controlled_config.rs) 的应用、重启和回滚；Stop/Restart 前后读取同代进程事实。公开本地更新 API 尚在，仓库内未找到绕 Session 的生产调用者 |
| 内核更新 | `CoreSession::install_release` 调用 [core_update/workflow.rs](../../crates/zenclash-core/src/core_update/workflow.rs) 的停止、用户文件替换、重启和回滚；此入口不是服务保护副本升级实现 |
| 首次服务交接 | [service_tun.rs](../../crates/zenclash-core/src/core_session/service_tun.rs) 在同一 Capture/Session 准入中停止 Local A，发布 Service；失败恢复仍使用被冻结的 A 描述与实际进程 |
| 权限与授权后重启 | [permissions.rs](../../crates/zenclash-core/src/core_session/permissions.rs) 动态区分当前 Local 与 Service。Local 的旧 `request_grant` 及授权后 restart 仍在；首页统一服务接线见 §5.1，不能据 Session 准入就认定所有配置生命周期均已使用 ServiceManager |
| 快照、页面与恢复 | [operational_status.rs](../../crates/zenclash-core/src/operational_status.rs) 后台转换当前 Session 进程事实；页面使用 `runtime_descriptor` 的内存元数据或当前 owner 的后台日志；[automatic.rs](../../crates/zenclash-core/src/core_session/automatic.rs) 的监督及源变化恢复继续使用同一 Session |
| 退出 | `CoreSession::shutdown`、[app/system_proxy.rs](../../crates/zenclash-ui/src/app/system_proxy.rs) 显式退出、[app/bootstrap.rs](../../crates/zenclash-ui/src/app/bootstrap.rs) 原生 quit observer 和 main event-loop 返回后的收尾共同覆盖正常出口；[process.rs](../../crates/zenclash-core/src/process.rs) 的 Drop 只作底层兜底 |

## 3. 客户端、热载与流入口

### 3.1 HTTP 请求需要统一的异步 seam

[client/request.rs](../../crates/zenclash-core/src/client/request.rs) 的 `get_json`、`patch_json`、`put_json`、`send_empty` 以及返回 `reqwest::RequestBuilder` 的 `request` 是主要入口。直接 API 调用还集中在 [client/api.rs](../../crates/zenclash-core/src/client/api.rs)：

- `proxy_group_delay`、`proxy_delay_with_provider`：query `url`、`timeout`。
- `dns_query`：query `name`、`type`。
- `reload_exact_payload`：`PUT /configs?force=...` 加 `{payload}`。
- `healthcheck_proxy_provider`、`patch_rule_disabled`：直接 builder/send。
- `send_long_operation`：请求级 120 秒 timeout；涉及升级/GeoData/Web UI。
- `validate_config_payload_unlocked`：实际 target `-t` 的本地后台路径。
- [client/connections.rs](../../crates/zenclash-core/src/client/connections.rs) 的 `shared_connections`：仍经 `get_json`，保留并发合并及缓存失效。

最小改动是内部异步 `send_api(method, path, query, body, timeout)`，返回可解码 status/body。Direct 分支保留现有 reqwest 行为；Service 分支调用受限 `ServiceClient::api`。保留 mutation gate 和现有 `ensure_success` 的有界错误语义，避免为 service 伪造 `reqwest::Response` 或 HTTP URL。

新增结构化 `MihomoError::Service` 或等价类型，区分：明确拒绝、未提交的连接失败、已发送但结果未知、会话结束。`should_restart_after_hot_reload`、`RuntimeMutationError.attempted` 不能仍只匹配 `MihomoError::Http`，也不能把每个 service error 都归为“应该重启”。

### 3.2 配置完整热载不能走 generic API 旁路

现有链路为 `CoreSession::apply` → `ControlledConfigStore` 准备与预检 → `accept_runtime_payload_for_session` → `MihomoClient::reload_payload/reload_exact_payload` → HTTP `PUT /configs`。缓存提交失败后 `RuntimeApplicationTransaction::rollback` 回放上一份确切 applied bytes。

服务分支改成受限 bundle 事务：

1. 普通用户后台读取合并 payload、已使用的 provider/TLS/GeoData 资源；来源读取仍在用户权限下完成。
2. 生成规范相对资源名并重写 payload。上传只携带 `assets/...` 或服务明确允许的具名资源，不发用户绝对路径让 root 读取。
3. `Stage(config)` 得 revision，分块 `UploadAsset`，完成预算与内容校验。
4. 新增具名 `Validate { revision }`：批准的真实 Mihomo 副本对安全 materialized 配置执行 `-t`，失败不改现运行内核。
5. 新增具名 `Reload { revision }`：服务把它自己生成的安全 payload 交给私密 controller；客户端不可指定 controller、`path`、任意执行文件。
6. 等待配置/实际 TUN 回读，提交本地缓存与用户 patch。必要时增加 `CommitRevision` / `RollbackRevision` seam，使服务保留 active 与 candidate 两个有界槽到业务提交完成。
7. 失败时恢复上一确切配置和它的资源 revision，推进业务 generation；回滚无法证明成功时显示 runtime unknown。

普通 `Api PUT /configs` 保持拒绝。部分修改也不能只通过白名单 PATCH 改内核而漏掉正式配置；v2 的具名部分事务与资源复用约束见 [部分配置事务](tun-service-partial-config.md)。完整订阅/override 更新统一用上述 bundle 流程，不能为快速兼容暂时放行未经服务安全规范化的 `{payload}`。

仅保存上一份 YAML 字符串不够：file provider 或 TLS 文件可能已变化、被删除，必须保留已接受资源的对应 revision。服务至少要有 active + candidate 两个受限目录，操作成功提交后清理旧目录，失败清理候选。GeoData 和 provider 更新不能删除仍被 active revision 使用的文件。

### 3.3 流监视必须跟随后端

[websocket.rs](../../crates/zenclash-core/src/websocket.rs) 当前返回 TCP/TLS `MihomoSocket`；[traffic.rs](../../crates/zenclash-core/src/traffic.rs) 的 `TrafficMonitor::start/run_monitor`，以及 [logs.rs](../../crates/zenclash-core/src/logs.rs) 的 `LogMonitor::start/run_monitor/connect_log_stream` 直接保存 endpoint。

改为由 `MihomoClient` 或小型共享 `ControllerTransport` 提供具名 event stream。内部 `CoreEventStream` 分为 HTTP WebSocket 与 `ServiceSubscription`，统一产生 JSON data；不需要向 UI 暴露 socket 或 HTTP authorization。

保留现有 generation watch、旧帧拒绝、取消、退避、日志 128 KiB 帧限额和历史 byte budget。日志当前支持 level 与 `format=structured` 的兼容回退；service 的 `Subscribe(Logs)` 必须补充同等具名 level/structured 策略，不能静默丢失用户等级选择。Traffic/Connections/Memory 按服务明确支持的 stream kind 连接。

## 4. 运行绑定与身份

保持现有本地 `MihomoProcess`，由唯一控制器绑定同时持有真实通信后端和进程所有权。实现使用 crate 私有 `OwnedCore::Local/Service`；External 不持有受管进程。`ManagedCore` 在前期梳理中表示这一边界，不要求再新增一层同名结构。

`CoreSession` 和 `MihomoClient` 读取同一个运行绑定，更换后端由 session 的串行入口发布；旧的 client/page/history/monitor clone 读取同一当前绑定。不能只改 `CoreSession.client` 而把现有 clone 留在旧 controller，也不能由每个页面各自切换。session 移除独立的旧进程字段，维护和退出按实际 owner 分派；本轮仍在迁移和验证中。

构造和初始类型配置必须拒绝类型不匹配。`CoreSession::open/open_with_config` 与 `MihomoClient::with_core_kind` 改为返回错误，由启动入口和测试调用方处理；该调整只影响 Rust 调用接口，不改变用户数据。不能在已经共享的 binding 上，通过同步 builder 偷偷改类型而绕过串行发布。类型和执行后端必须属于同一 binding generation。

UI 使用 `CoreSession::runtime_descriptor` 读取已准备的类型、执行方式和本地路径，不持有旧进程 Arc，也不在渲染中等待 child 或 IPC。Service 的受保护副本不提供假的本地 binary/config/home 路径。`CoreSessionSnapshot.running` 使用 `Option<bool>`，无法核实的运行事实和生命周期单独表示 Unknown；权限按当前 owner 动态回读，管理员服务权能不能代替网卡与路由事实。

下面保留前期接入职责地图；名称是梳理建议，实际接口以源码和本轮验证为准：

| seam | 用途 |
| --- | --- |
| `ManagedCore::from_local(Arc<MihomoProcess>)` | 兼容当前 bootstrap、本地测试与显式实验 meow |
| `ManagedCore::from_service(Arc<ServiceClient>, source_metadata)` | 只为已验证的 Mihomo 服务会话构造，不创建假的本地 child |
| `ManagedCore::observe().await` | 后台刷新状态与日志；IPC 失败形成未知观察，不当作真实 exit |
| `ManagedCore::snapshot()` | 仅读内存；服务 DTO 缺失不能解释成 `running=false` |
| `ManagedCore::restart_and_wait(..., cancelled)` / `stop_async()` / `release()` | 与当前事务/退出调用对应；服务明确 stop 后再 release，Stop 可保留会话 |
| `MihomoClient::direct_endpoint() -> Option<...>` / `controller_identity()` | Direct 允许既有真实地址；Service 返回语义身份和协议状态，绝不返回私密地址或占位地址 |
| `MihomoClient::send_api(...)` / `subscribe(...)` | Direct 与 Service 的实际请求、具名流分派 |
| `MihomoClient::prepare_runtime_bundle(...)` / `validate_revision` / `reload_revision` | full reload 的受控配置入口 |
| `CoreSession::switch_to_service(...).await` | 已授权且服务 ready 后，串行预检、停止旧内核、启动候选、回读、切换绑定、generation；失败恢复旧方式 |

只为展示保留源 binary/profile/cache 路径。必须明确它们是源路径，不声称是 service 实际执行路径。服务真实 PID、授权摘要、执行方式、健康状态可以进入快照，私密 controller 和配置 secret 不进入 UI/client metadata。

`MihomoClient::endpoint()` 当前的 UI 读取位于 [runtime/mihomo.rs](../../crates/zenclash-ui/src/pages/runtime/mihomo.rs) 与 [runtime/settings.rs](../../crates/zenclash-ui/src/pages/runtime/settings.rs)，需要同时迁移为语义 controller 身份。现有本地测试里 `process.endpoint()` 继续表示真实本地 controller，不必为了服务模式修改这个 API 的含义。

## 5. 安装、捕获、升级与退出

### 5.1 第一次开启的接入点

[traffic_capture.rs](../../crates/zenclash-core/src/traffic_capture.rs) 的 `ProductionCaptureBackend::ensure_tun_permission` 当前调用 `TunPermissionManager::request_grant`，Unix 新授权后重启本地内核；`set_tun` 通过 `CoreSession::apply` 持久化 TUN/DNS patch。这是服务工作流替换入口，不能保留隐式 setuid fallback。

当前应用级 [ServiceManager](../../crates/zenclash-core/src/service_manager.rs) 记录待开启意图、binding/generation、操作阶段和未知结果。页面/托盘共用 `enable_tun`：后台检查服务状态，缺失时安装、停止时启动、健康时连接；授权后重新核对原意图，再进入 `TrafficCaptureSession::enable_service_tun` 的单一准入事务。首次服务使用 Stage→Validate→Start→新鲜状态回读→保存→Commit；已有服务内核仍沿用相应配置事务。对话框和收据分类修复已通过相关行为验证及第二轮独立审查，随后 Windows 完整 UI 库 294 项和 binary 11 项通过；UI CI 附加 lint 和真实设备/路由验收尚未完成。

上述已审查批次覆盖 TUN 页面与托盘。P0 核对发现首页原来的 `home-tun` → `apply_home_capture_plan` 直接调用 `capture.apply(Tun)`，绕过 Manager，在 Local 分支可进入 Linux/macOS 旧 setuid 授权及重启；核对未执行授权。新的 [home.rs](../../crates/zenclash-ui/src/pages/runtime/home.rs) 已复用既有服务命令，同时使用首页已有 `action_error` 保留刷新后的失败提示。首页新增三项真实点击回归覆盖确认/取消与键盘、旧 binding 拒绝及即时错误刷新后保留，均有修前失败证据；相关 UI/ProfileService 整组 **7 项通过**，UI all-targets/all-features check 与 fmt 通过。该段记录首页首轮增量结果；随后 P2 修复、第二轮审查与导航尾修结果见下文。真实窗口及平台授权未验收，未标记整体完成。

首页新批次首次审查发现 P2：`Unconfirmed` 的共用完成回调跳过错误展示，而首页没有 TUN 页的服务状态区域，造成操作结果未知时缺少可见提示。修复复用首页消息区域读取 Manager phase、ProfileService 待确认收据与 warning，详情按钮提供已有 TUN 页确认入口；保留业务收据及 generation，不新增提交所有者。真实 typed 保存收据的首页回归修前 **0 通过/1 失败**，修后反馈 **2 项通过**、完整 Windows UI **299 项通过**，UI all-targets/all-features check 与标准严格 lint 通过，第二轮最终审查 **PASS（2/2）**。第二项使用私有 prepared 事实渲染验证未知状态与 warning，不代表原生维护授权超时已执行；完整 CI 附加 lint 仍为原有五项问题。

作者收尾另确认详情原先仅调用 `RuntimePage::switch_to`，没有同步 `ZenClashApp.current_page`。四行生产尾修已改用现有 `NavigateTun`，由应用 `on_navigate_tun` → `navigate` 同步外层选中页与 RuntimePage；Root 核对该既有调用链。尾修后 Home **5 项**、相关 service_tun **7 项**及 UI check、标准严格 lint、fmt/diff 通过；键盘测试证明真实导航 action 发出、TUN 页面可达及返回首页收据保留，fixture 不含完整应用侧栏。前述 299 项完整回归早于尾修，不替代最新全应用验收；没有新增第三轮同批子代理审查，真实侧栏与授权仍需实机验收。

安装程序超时不能当作已取消：Windows 提权进程可能仍在运行。保持未知/等待回读状态，禁止并行重复安装或卸载。授权明确取消才结束意图并恢复原捕获模式。

外部 controller 不接管；meow 不升级为 Mihomo 服务或伪装 TUN 能力。其他账户/实例的 `Occupied` 是独立状态，不应调用 Stop/Release 试图清除对方。

2026-10-02 补充只读核对：正式 `ZenClashApp::new` 始终注入 Manager，TUN 页和托盘开启的 Manager 分支失败后返回，首页已统一；未找到正常主应用直接触发旧授权的调用者。但独立页面构造仍有无 Manager 的 `capture.apply(Tun)` fallback，公开 core 捕获/权限 API 也可到达 Unix `chown`/`chmod 4755`。该能力不能据 GUI 接线完成而标记为已退休。Local 权限分支还支持实验 meow，接口收尾应单独明确兼容行为，不能顺带收窄实验后端。

启动拒绝 setid/file capabilities 与旧权限清理是两件事。现有服务安装复制批准的 bytes，不清除源内核权限；§7.1 所要求的所属文件识别、服务验证、停止旧内核和权限清理仍未实现。本轮未调用任何旧授权或修改权限。

### 5.2 内核更新需要两份产物事务

[core_update/workflow.rs](../../crates/zenclash-core/src/core_update/workflow.rs) 已有候选下载、预检、停止、用户目录替换、启动、版本核对和回滚。服务模式先下载到用户 staging，再经过固定授权安装入口，复制到受保护目录并更新摘要。现有用户文件激活不能直接成为 root 的执行授权。

授权取消或失败时不改当前已批准内核。受保护副本替换和服务重启失败时保留上一授权副本、metadata 与 active runtime；真实 `/version` 与预期匹配后才能提交成功。`CoreSession` 继续协调版本切换、capture 释放/恢复、generation 和 shutdown 取消。

本地 `DataWriteLease` 只管理用户缓存/源产物；跨进程提交依靠服务 revision、串行状态和安装事务。GUI 不通过 controller `/upgrade` 或 `/upgrade/ui` 绕过安装授权。

### 5.3 退出与恢复入口

- [app/system_proxy.rs](../../crates/zenclash-ui/src/app/system_proxy.rs) 的 `begin_quit`：先 `request_shutdown`，再 capture release、history shutdown、core shutdown；失败保持窗口与 owned 状态，不能先结束 runtime。
- [app/bootstrap.rs](../../crates/zenclash-ui/src/app/bootstrap.rs) 的 `on_app_quit` 在既有 Tokio runtime 依次等待 history 与 core shutdown；history 错误会记录但不跳过内核停止。它覆盖原生 quit 观察，与显式退出及 event-loop 返回后的再次收尾分别记录，不能据 callback 存在宣称原生 TUN 退出验收通过。
- 同文件 `stop_core_after_capture_release`：捕获释放失败不进入成功退出；服务流程沿用这条顺序。
- [main.rs](../../crates/zenclash-ui/src/main.rs) 的 GPUI event loop 返回后：Tokio runtime 上再次等待 core shutdown，即使历史持久化失败也停止 owned kernel；仅成功才启动重启后的 GUI。
- [automatic.rs](../../crates/zenclash-core/src/core_session/automatic.rs)：网络 suspend/resume 与 source watcher 必须检查 shutdown/generation；服务模式也不得创建 late child。
- 服务异常断线后：先观察会话/内核事实。租约负责有界回收，但不能拿它代替正常 Stop/Release 确认。未知状态不能静默切回本地 TUN，也不能产生两个恢复所有者。
- 正常服务停止应覆盖 TUN disable/网络释放及真实 child reap；Windows Job 强制清理与 Unix cgroup/launchd 回收只证明异常路径的进程约束，真实网络恢复仍需原生验收。
- 窗口隐藏到托盘不等于退出，不 release 会话，不丢弃维持业务的心跳。

### 5.4 应用启动接入：生产接线与阶段回归已通过

[main.rs](../../crates/zenclash-ui/src/main.rs) 已通过私有 [startup.rs](../../crates/zenclash-ui/src/startup.rs) 路由选择执行方式：显式外部控制器与实验后端沿用原入口；Mihomo 服务 Ready 时直接建立服务绑定，失败不进入 Local 自动恢复。服务缺失时，只有确认有效配置关闭 TUN 才允许普通 Local；保存了 TUN、配置事实不明、服务占用或状态未知时阻止 Local 自动启动。managed Mihomo 启动只读校验 profile、controlled config 和启用的 override 三层；坏字节保留供明确恢复，重复启动仍被阻止，不通过隔离损坏层后使用默认值决定启动。该批已通过阶段回归与第二轮审查，真实服务启动尚未验收。

main/UI 接线复用以下 core 入口，不改变用户数据库、订阅 YAML 格式或依赖方向：

- [MihomoRuntimeResources::prepare](../../crates/zenclash-core/src/process/discovery.rs) 在后台选择原配置/home 并准备普通权限 GeoData，不发现、复制、校验或启动 Local binary。
- [CoreSession::initialize_service_runtime](../../crates/zenclash-core/src/core_session/service_startup.rs) 在已有已验证服务绑定的同一 session 上执行首次配置事务。`CoreInitializationOutcome` 分别保留 `saved`、`failure`、`commit_pending` 与 `listener_fallbacks`；保存完成后 Commit 确认失败不能丢失收据。取消外层等待不丢弃已准入任务，Start 未知时保留 owner 供 shutdown 收尾。
- [MihomoClient::connect_service](../../crates/zenclash-core/src/client.rs) 建立经过身份校验的服务执行绑定；main 将初始化结果传给 App/ProfileService，启动和退出继续持有同一个 CoreSession。普通 Local 启动另检查执行文件，拒绝 Unix setuid/setgid 与 Linux file capabilities；WSL 普通用户下执行文件检查 4 项通过，包括实际权限位拒绝及 xattr 读取失败，不代表旧权限迁移已完成。

main/UI 首审发现自动网络暂停 Stop 破坏 Finalizing 提交，以及自动隔离损坏层后第二次启动误判默认 TUN=false 两项问题。均取得实际先失败再通过的回归证据：同 owner 在暂停前确认 Finalizing，实际 Stop 再复核；确认失败不 Stop、不释放捕获、不改变 phase、generation 或停止意图，退出仍可 Release。损坏三层配置保留原字节，两次启动均阻止 Local。修后初始化 9 项、ProfileService 14 项、双语文案 4 项通过，第二轮审查结束。

随后 Windows 完整 core **534 通过、0 失败、2 忽略**，完整 UI 库 **294 项**与 binary **11 项**通过；WSL 普通用户下 Linux 完整 core **577 通过、0 失败、2 忽略**、启动相关 **29 项**通过。core/UI check、标准严格 clippy、core 附加 lint 及 Linux core 完整 CI lint 通过。Linux 两项 Unix 更新回滚夹具的同步问题也已修复并通过独立首审，更新模块 19 通过/2 忽略。UI 完整 CI 附加 lint 在 proxy/logs/profile 文件仍有五项失败；可见恢复界面和原生高权限服务启动仍待完成。

服务没有可用 owner 时，当前启动返回明确错误，尚未接通可见恢复界面；独立恢复窗口与完整离线主界面的选择待用户确认。以上传输夹具和普通文件测试不能作为真实服务、窗口交互或 TUN 验收，也不把此前 Local→Service 测试作为“重启即可直接使用服务”的证据。

启动需先核对服务身份、协议、健康和授权内核版本，普通权限准备资源后直接建立 Service owner。服务缺失、损坏、占用或状态未知时准确反馈；不能先启动含 TUN 的本地配置，也不能静默依赖旧 setuid。后续在真实服务环境验收健康服务零本地 child、缺失/不兼容/占用、Start 响应丢失及启动与 shutdown 并发。

### 5.5 修复与卸载接线：待确认方案

底层维护授权和结果分类已有验证，Manager 的 Repair/Uninstall 命令类型不代表完整业务流程已实现。建议第一次 Local→Service 时只保存不可变本地启动描述，不永久保留旧进程 Arc；维护时先冻结最新 accepted bundle 和捕获意图，普通权限准备并校验关闭 TUN 的本地配置，再确认服务 B Stop/Release，创建并发布新的真实 Local owner，最后请求维护授权。全部阶段归同一个 capture 完成任务，shutdown 等待该任务；B 停止/释放未确认时不得拉起 Local 或执行维护。

恢复资源只能来自已持有的 bundle，不能重新读取已被删除或改变的订阅/TLS 源。候选生成目录为 `ControlledConfigStore.root()/local-runtime/{slot0,slot1}`，保留原 home；GeoData 固定缓存名需有界原子替换及失败恢复。目录、写入范围与失败策略尚待确认，当前没有实施或新增持久化 schema，不能把提案记为完成。

卸载授权取消后，保留服务安装和已恢复的 Local；修复取消只有在新鲜服务健康、同一意图且未退出时才尝试恢复捕获。维护结果未知禁止重发或自动启动服务。保存成功后的维护/清理错误保留保存收据；重新建立服务须新会话与新 revision，不能复用已 Release 的 owner。provider 最新缓存同步另在确认 Stop 后、Release 前接入，未接入时安全冷启动不能宣称缓存已保留。

### 5.6 Managed Local 配置的 TUN 准入缺口

2026-10-02 只读核对确认：启动入口拒绝含 TUN 的 Local 配置，并不覆盖启动后的配置生命周期。实际 owner 为 Local、后端为 Mihomo 时，完整应用、直接 PATCH 和缓存重启尚无统一 TUN 准入；最终有效配置启用 TUN 可进入这些执行路径。该结论是源码调用链证据，不证明普通权限成功创建网卡。本轮没有修改生产代码、运行新增回归或执行提权。

| 入口 | 当前路径 |
| --- | --- |
| 订阅导入、激活、更新及编辑 | [ProfileApplication](../../crates/zenclash-core/src/profiles/application.rs) → [CoreSession](../../crates/zenclash-core/src/core_session.rs) → [ControlledConfig](../../crates/zenclash-core/src/controlled_config.rs) → Local 完整热载；传输结果未知时可转本地重启 |
| 完整 apply 与 PATCH | Session 沿 pinned owner 应用；[client/api.rs](../../crates/zenclash-core/src/client/api.rs) 的直接 PATCH 发送前也缺少该门禁 |
| 备份导入 | [backup/workflow.rs](../../crates/zenclash-ui/src/pages/runtime/settings/backup/workflow.rs) → ProfileService → 同一完整 apply；失败恢复依赖确切旧快照 |
| 更新、监督及显式重启 | [core_update/workflow.rs](../../crates/zenclash-core/src/core_update/workflow.rs) 和 [process.rs](../../crates/zenclash-core/src/process.rs) 沿当前启动缓存恢复 |

判断必须基于最终 effective 配置：源 profile → controlled patch → 有序 YAML overrides。源 `tun.enable=true` 可以被后层覆盖为 false，源 false 也可以被覆盖为 true；不能只检查订阅源。Mihomo normalizer 不关闭 TUN，`-t` 只验证配置，不能代替运行时服务准入。这些配置链未调用旧 `request_grant`；遗留公开 `TrafficCaptureSession::apply(Tun)` 的授权路径是另一项待迁移边界。

建议复用一个 core 私有政策函数，只对实际 Local owner + Mihomo 生效：完整 payload 在验证、缓存和发送前检查最终 YAML；PATCH 在发送前检查启用请求；ControlledConfig 重启在最终合并后、写缓存前检查，Process 重启再检查真实启动缓存。现有单点不能同时覆盖 HTTP 与进程启动，不能仅按 endpoint 或 core kind 推断所有权。Service 保持受控 revision 事务，External 不获得本地控制能力，meow 保持实验后端边界。

产品行为待确认：拒绝并提示用户显式开启服务，或将配置操作纳入获授权的服务交接事务。前者改变 Local 含 TUN 配置的应用结果；后者扩大配置操作的事务和交互范围。尚未实施，不自动改写源配置、不自动安装服务，也不依赖 OS 报错决定准入。

后续行为回归应复用普通 HTTP/子进程 fixture：有效 false→true 时零发送、零重启，原 PID、缓存、持久化与 generation 保持；覆盖 override 正反覆盖、PATCH、备份、更新及自动恢复，并证明 Service/External/meow 不误拒。既有缓存回滚和确切备份恢复测试不能替代这些新增准入证据。

## 6. 行为测试与基线证据

本轮根代理已报告在 Windows 运行既有基线：`core_session` 相关 19 项通过、`traffic_capture` 相关 11 项通过。本子任务只读 core/UI，未重复执行 Cargo；原始输出由根代理持有。这组结果仅证明既有本地/外部与捕获状态机基线，不证明 service integration。

优先保留的既有行为测试：

| 测试入口 | 行为 |
| --- | --- |
| [core_session.rs tests](../../crates/zenclash-core/src/core_session.rs) | queued mode/patch/override 使用前一事务提交的 profile；rollback 推进 generation；source 删除后恢复 applied bytes；关闭期间拒绝 late restart；external 不获得 managed restart |
| [automatic.rs tests](../../crates/zenclash-core/src/core_session/automatic.rs) | suspend/resume 不控制外部进程；source revision/generation 边界仍需服务接入回归用例 |
| [traffic_capture.rs tests](../../crates/zenclash-core/src/traffic_capture.rs) | 权限拒绝不写捕获；权限只能显式 TUN 触发；失败恢复与 reconcile needed；外部系统代理不覆盖；退出释放所有权但不清用户意图 |
| [controlled_config/tests.rs](../../crates/zenclash-core/src/controlled_config/tests.rs) | 源文件保持、缓存/patch 原子事务、失败回滚与 unsupported core |
| [core_update tests](../../crates/zenclash-core/src/core_update) | 产物激活、版本检查、回滚与 shutdown 取消；需要另补服务授权副本用例 |
| [websocket.rs tests](../../crates/zenclash-core/src/websocket.rs)、[logs.rs](../../crates/zenclash-core/src/logs.rs)、[traffic.rs](../../crates/zenclash-core/src/traffic.rs) | 128 KiB 日志限额、消息分帧累计限额、日志等级、generation 与有界退避 |
| [app/system_proxy.rs tests](../../crates/zenclash-ui/src/app/system_proxy.rs) | capture release 失败阻止 core shutdown，成功才退出 |

新增服务接入测试按可观察行为分层：

1. local、service 与 external 三种执行方式给出一致所有权；external/meow 始终不会触发服务安装或切换。
2. 初次 TUN 授权取消、未就绪、bundle/target `-t` 拒绝时，原本地 PID/配置/捕获保持；停止旧内核后候选失败时恢复上一真实方式。
3. 每个已有 clone 在切换后请求新的 service transport；旧 Direct response、旧 event frame 不更新当前 generation。
4. file provider/TLS/GeoData 被源端删除或更改后，rollback 仍恢复上一 accepted revision 与资源；原订阅和 bundle 不被 root 原地改写。
5. 明确 `InvalidConfiguration` 不重启；提交响应丢失进入 unknown/readback，不盲目重试 mutation 或重设 sequence。
6. service 不通不会把缓存 `running=true` 视为就绪，也不会把 unknown 视为已退出来自动启动第二内核。
7. 正常 stop 可再次启动同 revision；release、租约失效、真实所有者 PID 消失后回收内核；页面隐藏不 release。
8. 首次安装、修复、内核更新、卸载在同一应用所有者串行；超时结果未知不重复授权；另一用户 `Occupied` 不会被 stop。
9. exit 与 Validate/Reload/Start/install 并发：shutdown flag 永远抑制候选与 rollback restart；最终等待内核消失且网络恢复才完成。
10. 服务控制器绕过、原始 path reload、未暂存资源、伪 service、native PID 复用等负向测试使用实际平台接口；共享事务可使用可控替身，实机验收不能用替身冒充。

完成 core 接入后运行最近测试，再按原计划执行 workspace fmt/check/test/clippy。涉及真实服务与 Mihomo 的新增 ignored integration tests 应在隔离机器上显式运行，记录安装授权、GUI/服务/内核 PID、网卡/路由、停止/退出与资源释放。Windows、macOS、Linux 分别记录；跨编译和普通 CI 不构成 TUN 实机结果。

## 7. 推荐实施顺序

1. 收尾当前所有权、UI 消费和 partial PATCH 差异，补齐测试模块；先得到当前 source 的编译、相关行为测试和必要审查结果，不用前一阶段绿结果代替。
2. 将上游 `runtime_generation` 中适用的差异规划、路径规则、有界重试及声明范围内缓存回读直接移植并适配；接当前受保护资源、会话、revision 和预算，保留本地实现和 external=`None`。
3. 比对并移植平台安装维护及进程管理中的缺口，保持同一通信绑定、真实 owner、鉴权和私密地址隔离；安装和内核恢复策略符合本项目退出约定。
4. 收尾配置缓存、完整与部分 runtime bundle、回滚及核心更新，全部使用相同 session gate；跨 revision 缓存与旧持久身份导入分别验收，不以目录复用代替身份迁移。
5. 替换旧 TUN 授权入口，增加应用级维护任务所有者，接首页/TUN/托盘同一命令；monitor 的 level/structured 与后端切换语义继续保留。
6. 验证 exit/network/source-change 所有分支，运行审查与完整检查，完成三平台真实内核和网络验收后再勾选主计划 P3/P4/P6。

实施不得把“Stage/Start 通过”替换为 full hot reload 的完成条件；不得把本地 facade 构建通过替换为服务后端、授权副本、配置资源和退出路径的完整证据。

## 8. 完整配置事务接入记录（2026-10-01）

核心服务绑定现在共用 [service_runtime_session.rs](../../crates/zenclash-core/src/service_runtime_session.rs) 的 revision 所有者。候选在普通用户权限下只准备一次 immutable bundle，Stage/Upload/Validate 后的 token 直接应用同一 revision。Standalone `MihomoClient::reload_payload` 完成 apply 和 commit；受管业务事务延迟服务 commit，直到配置 patch 或 profile catalog 保存成功。

保存失败时回载上一 accepted revision，保留其资源快照，不重新读取订阅、provider、TLS 或 GeoData 文件。缺少上一 accepted snapshot 时明确停止 owned kernel 并报告无法恢复，不能用现在的用户源文件伪造旧配置。备份恢复的 `CoreRestoreSnapshot` 也保留 immutable service bundle。具名 `prepare_recovery` 拒绝 Finalizing；其 apply 先恢复同一 owner 的 validated active revision，成功后才允许 Stage 退休未保存候选。同一个快照直接回载旧 revision，不重复上传；若更早 revision 已退休，则只重新上传所持快照中的字节。

业务保存成功后先保留启动缓存和 committed profile/generation，再等待 `CommitRuntime`。确认失败产生 `CommittedButRuntimeUnknown`，owner 保留 pending finalize。`reconcile_service_runtime` 只读取 Status 并在必要时幂等提交，不再次 Stage/Reload；这种状态拒绝 rollback。未保存的候选允许通过 `rollback_service_runtime` 回到旧 accepted revision。后端切换也先确认 pending finalize，避免丢弃它的所有者。

持久化 worker 不受外层等待 future 的取消影响，因此已应用 runtime 后的保存与 finalize/rollback 必须由同一个 completion task 完成。`ProfileApplication` 的任务持续持有 `CoreProfileApplication` 的 transition、store gate 和写租约；`CoreSession::apply/set_mode` 在获取 owned transition 后才移交任务，排队阶段取消不创建后台事务，已有 task 的数量受串行 gate 限制。正常 shutdown 等待这一 gate，再停止 owned kernel。门栓回归测试验证 worker 已进入保存后取消外层等待，DB/override/cache/committed profile/generation 最终一致，且 shutdown 不提前越过未完成事务；最终真实 kernel reap 仍属实机验收。

本阶段仍未完成的边界：

- Partial `PATCH /configs`（尤其 mode/TUN/listener）成功后，full bundle/revision 需要同步持久化其配置事实；现有 mode 仍保持 partial 更新语义，不能直接改成完整 reload。下一阶段应增加受控 patch revision 事务并复用原资源，覆盖先改 mode 再 full reload 失败的回滚用例。
- Tailscale、ZeroTier 的 state-dir 身份目录迁移尚未实现。不能静默重建已有身份，也不能把用户目录交给服务以 root 读取或写入；在专门迁移完成前，服务端需要明确拒绝这些 stateful 配置（包括隐式默认目录）。
- ManagedCore 启动和后端迁移仍在后续阶段：真实 service Start/Commit 成功后用 `adopt_service_runtime` 移交原 bundle，核验 applied/committed revision；不创建假的 `MihomoProcess`。
- 本阶段行为测试验证 shared owner、候选资源快照和提交确认丢失恢复，不构成 Windows/macOS/Linux 真实服务、TUN、路由或退出实机验收。

本阶段 Windows 验证：`cargo check -p zenclash-core --all-targets --locked` 和 `cargo clippy -p zenclash-core --all-targets --locked -- -D warnings` 通过；最新完整 `cargo test -p zenclash-core --lib --locked --quiet` 为 426 通过、0 失败、2 忽略。此前完整运行中既有 `direct_patch_rollback_invalidates_reads_of_the_temporary_runtime` 有一次等待测试 HTTP 请求的五秒超时，单项和完整重跑均通过，未放宽 timeout 或断言。新增 7 项 service revision 所有者行为测试、2 项持久化后确认失败/取消的启动缓存测试和 2 项真实文件持久化取消门栓测试。相关 Rust 文件已格式化，文档 45 个本地链接和 `git diff --check` 检查通过。首审发现的持久化取消窗口与 unknown candidate 阻塞快照恢复问题已修复，第 2 轮独立审查确认通过；其同时指出备份外层事务与 Finalizing 组合的另一处 P1，收尾如下，同阶段不再开启第 3 次子代理审查，由主代理手工复核。


### 备份外层事务收尾

备份的文件回滚与普通 profile 提交属于不同的事务边界。普通 `prepare_recovery` 和 `rollback_service_runtime` 继续拒绝撤销 Finalizing 的已保存 profile。只有持 `CoreBackupAdmission` 的备份恢复会先确认服务 candidate 的 Status，并在必要时幂等 `CommitRuntime`；确认失败不 Stage、不误报恢复成功。crate 私有 `RuntimeRestoreAuthority::Ordinary/Backup(admission)` 从 CoreSession completion 传过 controlled store 和 client 到 service owner，不能根据 `Some(bundle)` 自行推断备份权限。普通 public `restore_snapshot` 始终传 Ordinary，并拒绝消耗待恢复的备份记录。确认成功后，用仍持有的旧 bundle 恢复更早配置；即使旧 revision 已退休、用户 YAML/provider 已删除，也只上传内存快照中的原字节。

`CoreSession::begin_backup_restore` 返回 opaque `CoreBackupAdmission`。备份工作流激活前获取 admission，把激活、应用、页面回读、提交或回滚整个过程交给一个 completion task。外层 UI 等待 future 被取消只放弃等待；任务继续持有 admission 和磁盘写租约。Shutdown 先设置拒绝新工作的 flag，再按 backup gate → runtime transition 顺序等待，避免回读与清理间隙提前结束。已 admission 的显式回滚允许在 shutdown flag 后完成收尾，不能借此开始新的备份。

`restore_backup_snapshot` 自身也移交 owned transition/写租约，结果发布和恢复记录更新在同一个 transition 内完成。失败时 CoreSession 保存一份有界 immutable snapshot、普通 store root 和当前 generation；`pending_backup_restore` 仅读内存，`retry_backup_restore` 在同 gate 下恢复精确快照。重试成功或接受完整配置/profile 业务提交会释放旧记录，避免旧恢复覆盖新配置；失败或 unknown 请求仍推进 generation 来失效旧读取，但保留快照并更新恢复 token，不能把版本推进当作恢复上下文已被取代。ProfileApplication 的 generation 发布与 CoreSession 共用同一更新规则。备份重试界面已在独立 UI 阶段接入；它不使用普通 profile activate，也不混同已保存 profile 的提交确认。

原先怀疑 commit 后 escaped store 的 weak permit 失效会阻止写入，真实写入测试否定了这一判断：现有 DataWriteAccess 在 permit 到期后会获取普通写租约。保留这个安全语义，不增加重开 stores 的 I/O。

新增证据：lost CommitRuntime ack → 实际备份文件回滚 → 原 YAML/provider 删除 → held bundle 恢复测试先失败于 OutcomeUnknown，修复后通过；确认请求失败时 Stage 数量不增加，后续同快照可重试。UI 取消测试先失败于 shutdown 在备份回读/清理间隙提前完成，修复后验证正常等待；另有 commit 后三个返回 store 实际写入的行为测试。核心失败恢复测试验证控制器响应丢失后保留快照，源与运行缓存文件删除后仍可重试。

Windows 收尾验证：`cargo test -p zenclash-core --lib --locked --quiet` 428 通过、0 失败、2 忽略；`cargo test -p zenclash-ui --lib --locked backup::workflow::tests:: -- --nocapture` 4 通过；`cargo check -p zenclash-core -p zenclash-ui --all-targets --locked` 和相同范围的 strict Clippy `-- -D warnings` 通过。此前取消红测试 fixture 曾二次 poll 已完成 JoinHandle，已修 fixture 并重新取得 shutdown 过早完成的真实行为红；没有放宽断言。这些是普通文件/可控传输行为证据，仍不构成真实服务/TUN 实机验收。


父代理手工复核进一步发现 `Some(bundle)` 无条件走 backup 恢复以及失败 mutation 推进 generation 时丢弃唯一快照的边界，已用三项真实行为红/绿修复：生产 client 使用的 shared owner restore 路由在 Ordinary/Finalizing 时不产生 Status/Commit/Stage，并保留 active/candidate；持真实 opaque admission 的 Backup 分支先确认再恢复；public ordinary snapshot 不能消费待恢复备份；public mode GET→PATCH 响应丢失→rollback PATCH/readback 后保留快照，删除源文件与生成缓存后可重试。最后一项使用实际 HTTP 调度，不构造枚举错误作为证据。完整 native ServiceClient→public CoreSession 的权限链仍需 P6 验收，此阶段用 production 路由与 public HTTP 入口分层验证，没有新增生产测试后门。

最新 Windows 核心回归为 439 通过、0 失败、2 忽略（包含并行 ManagedCore binding 阶段的 9 项测试），备份相关 21 项通过；core alltargets check 与 strict Clippy 通过。新增测试曾因 MutexGuard 的词法作用域跨 await 触发 Clippy，已收窄作用域，不放宽 lint。core/UI alltargets 的后续合并检查曾被下一 UI 阶段尚未修好的私有 snapshot fixture 调用阻断，该阶段需通过 public BackupRestoreTransaction 获取快照；不将该中间态报告为 UI 全绿。备份 P3 仍由父代理手工复核，不再增加第三次子代理审查。


### 操作期绑定与数据授权

Controller binding 以同一 snapshot 发布 Local 的真实进程 owner 与 endpoint、Service 的共享 runtime owner，或 External 的 endpoint。现有 clones 共享 binding 更新。一次配置操作先调用 crate 私有 `pin_binding`，用该 snapshot 计算数据写 scopes，取得数据租约后再授权 validator；`with_write_lease` 会检查全部 scopes，拒绝错代或不足的权限，不依赖 panic。

CoreSession 的配置应用、模式修改、profile staging、精确恢复及维护/更新/自动恢复入口在 Data 等待前固定 binding，Data 和 Transition 等待后重新核验。ControlledConfig 的配置入口继续按 Data → Transition（由 session 持有时）→ store Mutation 顺序工作，并在取得 Mutation 后核验；client 的 controller Mutation 等待后也再次核验。传入的 pin 从 scope 计算、validator 预检直到发送都保持同代，不能把旧目录租约交给新内核。

发送前拒绝使用 `StaleBinding`，明确没有应用本次请求；发送后发现后端更换仍使用结果可能未知的 `StaleTransport`。无 committed profile 的模式操作也不会因 `StaleBinding` 推进 runtime generation。四项调度测试覆盖排在 Data、Transition、store Mutation 及 client Mutation 后的旧操作：新控制器没有请求，已有运行缓存字节保持不变，模式 generation 保持不变。前三项通过 public 配置入口，最后一项直接驱动生产的已 admission 模式尾部，避免把枚举构造当作调度证据。

本次 Windows 调用点验证：`cargo test -p zenclash-core --lib --locked --quiet` 为 445 通过、0 失败、2 忽略；上述四项调度测试通过。`cargo check -p zenclash-core -p zenclash-ui --all-targets --locked` 和相同范围 strict Clippy `-- -D warnings` 通过。Clippy 初次发现两处新测试没有处理读取字节数，已修复并重新验证，不放宽 lint。此前 UI 私有 snapshot fixture 的合并检查阻断已由公开备份事务路径修复；本次合并检查包含独立备份重试 UI 阶段。

此处只记录该阶段的 binding 与调用点验证。该次检查尚未包含 CoreSession 独立 process 字段向唯一 owner 的迁移、retired Local 显式停止的追加行为验证、partial PATCH revision 及真实服务/TUN 验收。

### 唯一 owner 的生命周期阶段结果

CoreSession 从 controller binding 取得当前 Local/Service owner，移除独立的旧 process 字段。Rust 调用接口改为 `CoreSession::open(kind, client)` 与 `open_with_config(kind, client, profile, overrides)`，均返回 `Result`；本机启动先用 `MihomoClient::from_process(actual_process)` 构造。外部控制器的 kind 必须在唯一且未使用的 client 上显式配置，`with_core_kind`、`with_config_validator` 同样返回 `Result`，共享后不同 kind 的 builder 请求明确拒绝。调用接口迁移不改变数据库、YAML 或磁盘数据格式。

后端切换统一经过 CoreSession 的 `switch_to_process`、`switch_to_service`、`switch_to_direct`，底层 client switch 只在 crate 内使用。维护、退出、网络暂停恢复和监督器读取同代 owner；Service 重启指定已接受 revision，并持有其 immutable bundle，不能选择较新的未保存 candidate。Stop 回执丢失后必须通过原生 Status 确认 stopped，再发送 Start。Release 关闭新业务准入；未知停止或释放结果不能触发另一进程启动。

`runtime_descriptor()` 仅读短 watch borrow 中的已准备元数据，不克隆实际进程 owner、不等待进程锁或执行 IPC。Local 提供真实 binary/config/home，Service 与外部不伪造本机路径。运行观察使用可选 running 与 Unknown，状态查询失败不会覆盖显式 Stopped、NetworkSuspended 或 ShuttingDown 意图。TUN 权限按当前 Local binary 或新鲜的认证 Service Status 后台观察；Service authority 只证明管理员 helper 权限，设备和路由仍独立核验。旧本机 setuid 来源不能被误用于 Service 保护副本。

流量接管与 owner publication 已共享 CoreSession 的 capture gate。切换顺序为 Capture → Data → Transition → Mutation；退出先等待既有备份 admission，使用 Backup → Capture → Transition → Mutation。Capture 的内部配置应用与权限操作不再次获取 Capture 或 Backup，保留 Data → Transition → Mutation。监督器与网络恢复调用 capture release/reconcile 前先释放 Data/Transition；当前备份工作流不在持有这些锁时调用 capture，禁止后续新增反向锁序。

行为测试已验证：Capture 排队时只等待 admission，取消排队不会启动后台 completion。取得 owned guard 后，native 写入、回读和必要回滚由一个 completion task 持有，外层等待被取消也不能提前退休当前 owner。该 gate 仅保证 A 的已准入接管操作收尾后才发布 B；B 的端口或后端不同后，仍需 P4 的高层切换事务在同一 capture admission 下 release/reapply 接管意图。不能把 publication gate 当作跨后端网络恢复已完成，也不能在持 gate 时重入普通 reconcile。

生命周期快照用内存字段 `stop_requested` 独立保存显式停止意图。Status 失败可令观察为 Unknown，但不能清除该意图使监督器、网络恢复或权限授权重新启动内核。维护在确认已停止后重启失败会推进 generation 并报告准确未知状态；排队取消、停止前失败和确认零发送则不伪报内核运行事实已改变。未知 Stop 必须先用新鲜 Status 确认停止，不能盲发第二次 Start。

本阶段 Windows core 完整回归 **483 通过、0 失败、2 忽略**；owner 生命周期 **18 项**、ServiceRuntimeSession **20 项**、capture 门栓 **15 项**通过。all-targets/all-features check、严格 clippy 和两轮独立审查完成。回归使用真实 Rust child、独立 HTTP readiness fixture 与可控服务传输，证明进程退休、请求顺序、取消和失败恢复；不证明 Mihomo、管理员授权、真实 TUN 或完整应用验收。Linux/macOS core 交叉编译分别被缺少 `x86_64-linux-gnu-gcc` 与 `cc` 阻断，尚无目标平台编译结果。

### core 部分配置业务接入（阶段验证完成）

新增部分候选沿用同一个 ServiceRuntimeSession 所有者；支持局部修改的字段经过 prepare/apply、业务保存及 commit，失败时执行逆 PATCH 和回读。候选从已接受 YAML 推导，并共享原资源字节；缓存与备份目标合并最终实际应用的 delta，不重新读取变化中的用户源。业务已保存但提交确认未知时仍保留已保存 delta；未保存的失败不得合并。固定版本不支持的字段继续走完整配置事务。

三项实际失败的行为测试修复后通过：模式修改混入变化后的源配置、取消等待造成业务保存与启动缓存分离、备份重试覆盖后来保存的模式。生产 effective delta、保存收据和备份合并已接线；停止后未保存候选通过 Restore 清理，保存后 Finalizing 不倒退。该阶段完整 core 500 项、服务 182 项及 check/严格 clippy 通过，两轮独立审查结束；其详细证据见 [部分配置事务](tun-service-partial-config.md)。

### 首次服务启动与捕获准入（阶段验证完成）

`TrafficCaptureSession::enable_service_tun` 由应用级 ServiceManager 发起，同一个 completion 持有 Capture → Data → Transition → Mutation 的准入与必要恢复。首次 Full 事务使用 Stage → Validate → Start → 新鲜状态及配置回读 → 业务保存 → Commit；已有已接受内核的 Full 事务继续使用 Reload。Start 响应丢失只观察准确 revision 的状态，不盲目重复启动。

本地 A 确认停止后，在 B Start 前发布同一个准备好的 ServiceRuntimeSession Arc，使退出能够看见并停止真实 owner。失败后必须先确认 B 停止/释放且恢复缓存，才允许恢复 A；B 结果未知时保留其绑定，禁止启动第二个内核。应用退出期间不恢复 A。

待重试的备份目标在 A 停止前，以普通权限冻结其资源 bundle；资源与当前 held payload 相同则共享 Arc。只有真实保存成功才合并 delta 和发布 generation。保存后的 Commit 或旧 owner retirement 失败保留保存收据及当前绑定，显示准确的待确认状态；未保存的 typed error 不被后续清理错误覆盖。

本阶段初次启动 3 项、捕获交接 3 项、备份目标 2 项及 retirement 2 项行为通过，其中初次启动和 retirement 有实际先失败再通过证据。完整 core **514 通过、0 失败、2 忽略**，all-targets/all-features check 与完整 CI 额外严格 lint 通过，首次独立审查通过。这些状态机、传输 fixture 和真实普通 Local 子进程证据，不替代管理员服务、真实 Mihomo、网卡和路由验收。

