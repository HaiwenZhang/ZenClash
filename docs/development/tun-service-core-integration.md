# TUN 服务与 core 接入梳理

- 日期：2026-10-01。
- 对应计划：[三平台 TUN 服务开发计划](tun-service-plan.md) 的 P0、P3 与 P4 接入边界。
- 实现方向：优先移植上游服务代码并适配本节契约，具体来源与批次见 [上游代码移植计划](tun-service-upstream-migration.md)。`CoreSession`、配置事务、捕获协调和 GPUI 保留现有责任；移植尚未完成。
- 状态：共享 client transport、monitor、普通权限资源 bundle 与完整配置 revision 事务已有分阶段检查和行为证据。所有权、部分配置 revision 同步与 UI 消费接口正在继续迁移；最新阶段的验证范围见 [实施记录](tun-service-progress.md)，不把此前通过结果当作当前工作区或完整应用验收。
- 当前验证环境：Windows。macOS、Linux 实机安装、授权、真实 Mihomo/TUN 与退出验收仍待执行。

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

## 2. 本地进程调用清单

下表列生产路径；测试中的 fixture 构造不作为额外业务所有者。

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

应用级 `ServiceOperation` 所有者记录待开启意图、业务 generation、操作阶段和当前未知结果。先读真实服务状态；未安装时交给 UI 的安装对话框确认，执行授权后回读，再调用 `switch_to_service`；最终仍经当前 capture coordinator 应用 TUN 和设备/路由事实。

安装程序超时不能当作已取消：Windows 提权进程可能仍在运行。保持未知/等待回读状态，禁止并行重复安装或卸载。授权明确取消才结束意图并恢复原捕获模式。

外部 controller 不接管；meow 不升级为 Mihomo 服务或伪装 TUN 能力。其他账户/实例的 `Occupied` 是独立状态，不应调用 Stop/Release 试图清除对方。

### 5.2 内核更新需要两份产物事务

[core_update/workflow.rs](../../crates/zenclash-core/src/core_update/workflow.rs) 已有候选下载、预检、停止、用户目录替换、启动、版本核对和回滚。服务模式先下载到用户 staging，再经过固定授权安装入口，复制到受保护目录并更新摘要。现有用户文件激活不能直接成为 root 的执行授权。

授权取消或失败时不改当前已批准内核。受保护副本替换和服务重启失败时保留上一授权副本、metadata 与 active runtime；真实 `/version` 与预期匹配后才能提交成功。`CoreSession` 继续协调版本切换、capture 释放/恢复、generation 和 shutdown 取消。

本地 `DataWriteLease` 只管理用户缓存/源产物；跨进程提交依靠服务 revision、串行状态和安装事务。GUI 不通过 controller `/upgrade` 或 `/upgrade/ui` 绕过安装授权。

### 5.3 退出与恢复入口

- [app/system_proxy.rs](../../crates/zenclash-ui/src/app/system_proxy.rs) 的 `begin_quit`：先 `request_shutdown`，再 capture release、history shutdown、core shutdown；失败保持窗口与 owned 状态，不能先结束 runtime。
- 同文件 `stop_core_after_capture_release`：捕获释放失败不进入成功退出；服务流程沿用这条顺序。
- [main.rs](../../crates/zenclash-ui/src/main.rs) 的 GPUI event loop 返回后：Tokio runtime 上再次等待 core shutdown，即使历史持久化失败也停止 owned kernel；仅成功才启动重启后的 GUI。
- [automatic.rs](../../crates/zenclash-core/src/core_session/automatic.rs)：网络 suspend/resume 与 source watcher 必须检查 shutdown/generation；服务模式也不得创建 late child。
- 服务异常断线后：先观察会话/内核事实。租约负责有界回收，但不能拿它代替正常 Stop/Release 确认。未知状态不能静默切回本地 TUN，也不能产生两个恢复所有者。
- 正常服务停止应覆盖 TUN disable/网络释放及真实 child reap；Windows Job 强制清理与 Unix cgroup/launchd 回收只证明异常路径的进程约束，真实网络恢复仍需原生验收。
- 窗口隐藏到托盘不等于退出，不 release 会话，不丢弃维持业务的心跳。

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

此处只记录 binding 与调用点验证。CoreSession 现有独立 process 字段向唯一 ManagedCore owner 的迁移、retired Local 显式停止的追加行为验证、partial PATCH revision 及真实服务/TUN 验收仍按后续阶段处理，不能据本次 check 报告这些生命周期工作已经完成。

