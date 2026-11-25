# ZenClash（`zenclash-core` & `zenclash-ui`）全面架构与代码深度审查报告

- **审查日期**：2026-11-21
- **代码基线**：`main` @ `13139cd`（新分支 `docs/architecture-review`）
- **审查范围**：`crates/zenclash-core`（284 个 `.rs` 文件，89,394 行：生产 39,321 行 / 测试 50,073 行）与 `crates/zenclash-ui`（235 个 `.rs` 文件，70,903 行：生产 36,570 行 / 测试 34,333 行），并联动核对 `crates/zenclash-i18n` 与 `crates/zenclash-service` 边界
- **参照基准**：
  - 领域词汇：[CONTEXT.md](../../CONTEXT.md)、[GLOSSARY.md](../../GLOSSARY.md)
  - 历史审计（本报告**不重复**其中已标记修复项，仅记录新发现或同类未覆盖路径/回归）：[zenclash-core-audit-2026-10-19.md](zenclash-core-audit-2026-10-19.md)、[zenclash-ui-audit-2026-10-19.md](zenclash-ui-audit-2026-10-19.md)
  - UI/GPUI 规范：GPUI Kit Coding Guides 与 Design Guides、[ui-design-implementation.md](ui-design-implementation.md)
- **配套新设计文档**：[software-design-v2-2026-11-21.md](software-design-v2-2026-11-21.md)

---

## 一、执行摘要与量化基线

### 1.1 总体评价

ZenClash 的工程底蕴显著高于同类开源代理客户端：测试代码占比超过 52%（测试/生产比 Core `1.27:1`、UI `0.94:1`），Workspace 启用了严格的 Clippy 门禁（`await_holding_lock`、`future_not_send`、`large_futures`、`perf`、`redundant_clone`、`deny(missing_docs)`），并建立了一套成熟的“代次隔离 + 观测身份（`CoreObservationIdentity`）+ 取消安全提交 + 崩溃恢复 WAL + 非侵入式系统代理所有权”工程理念。

但在快速迭代（v0.1 → v0.3，含特权服务化 TUN 集成、多轮 UI 视觉改版与三平台原生化）之后，代码库在**架构边界、并发锁层级、跨平台原生契约、性能算法、文件组织、视觉无障碍规范**六个维度积累了显著结构性债务，并存在若干**经代码路径实证的 P0/P1 级严重缺陷**。

### 1.2 核心量化指标

| 指标 | `zenclash-core` | `zenclash-ui` | 问题信号 |
| --- | ---: | ---: | --- |
| 生产代码行数 / 文件数 | 39,321 行 / 209 文件 | 36,570 行 / 173 文件 | 体量已达中大型桌面应用，单体 crate 边界吃紧 |
| 测试代码行数 / 文件数 | 50,073 行 / 75 文件 | 34,333 行 / 62 文件 | 大量 500–1,500 行测试内联在生产文件底部 |
| 顶层平铺模块数 / 根导出符号数 | 70+ 模块 / **329** 个平铺导出 | 16 模块 / 依赖 **142** 个 core 符号 | Core 缺乏二级子域收敛，UI 直接深入 core 内部类型 |
| 直接双向循环依赖对 | **6 对** | **2 对**（含 `components <-> pages`） | 分层方向被破坏，牵一发而动全身 |
| 互斥/门控层级 | 最多 **10 层**嵌套互斥 | `RuntimePage` 47 字段 / `ZenClashApp` 42 字段 | God Struct + 高死锁/锁竞争风险 |
| `#[recursion_limit]` | `256`（`lib.rs:5`） | 默认 | 嵌套配置事务 Future 类型过深 |
| 废弃/死代码体量 | 退化为单变体的 `CoreKind`/兼容别名 | 773 行死首页模块 + **415 条**废弃 i18n 键（20.9%） | 历史演进清理不彻底 |

### 1.3 发现问题分级统计

| 严重度 | 定义 | Core 数量 | UI 数量 | 关键代表 |
| --- | --- | ---: | ---: | --- |
| **P0** | 安全漏洞（RCE）、常见路径功能完全不可用或数据损坏 | 2 | 0 | Windows TUN PowerShell 命令注入兼必败；Linux GNOME `port` `uint32` 违背 Schema 导致系统代理不可用 |
| **P1** | 用户可见错误行为、高频卡顿/抖动、核心事务竞态或崩溃窗口 | 15 | 13 | 退出前不可逆判死 `CoreSession`；1Hz PAC 闪断与全壳重绘；`apply_config` 覆写链 TOCTOU；下载持全局租约冻结存储；覆写先删文件后写清单；Windows WinINET 预测失败；macOS `GET /configs` 特权写；设置说明静默丢弃；日志搜索 `"null"` 误匹配；代理网格 50ms 闪烁与重测回环 |
| **P2** | 边缘缺陷、显著性能/架构/视觉对比度/无障碍债务 | 23 | 22 | 4 组主题语义色不达 WCAG AA（最低 2.45:1）；单次保存 12 次 YAML 解析；空闲每 5 秒写盘 SQLite；PAC 5ms 忙轮询；God Struct 与循环依赖；嵌套滚动冲突 |
| **P3** | 命名漂移、兼容别名、死代码与样式细节 | 10 | 9 | 兼容别名、415 死 i18n 键、14 处违背 `CONTEXT.md` 术语、英文 Title Case 漂移 |

---

## 二、已核实的关键 Bug 与安全缺陷清单（按严重度排序）

> 以下所有条目均已追踪完整调用链与源码行号确认，不含推测项。

### 2.1 P0 级（必须立即修复）

| 编号 | 模块 | 问题标题 | 精确位置 | 触发场景与根因证据 |
| --- | --- | --- | --- | --- |
| **BUG-01** | Core / TUN | **Windows TUN 运行时观测存在 PowerShell 命令注入（RCE）且常规调用必然失败** | `crates/zenclash-core/src/tun_runtime/windows.rs:5-30`、`tun_runtime.rs:51-60` | 使用 `powershell.exe -NoProfile -NonInteractive -Command SCRIPT device_name`。PowerShell 的 `-Command` 会把后续所有参数拼接成一条命令字符串（`... \| ConvertTo-Json -Compress <device_name>`），不会填充 `$args[0]`：① 常规场景 `$args[0]` 为空且 `ConvertTo-Json` 报 `ParameterBindingException`，Windows TUN 观测永远返回 `(Unknown, Unknown)`；② `device_name` 来自远端订阅 YAML 的 `tun.device`（仅 `.trim()`），恶意订阅写入 `; Start-Process ... #` 即可在刷新状态时执行任意 PowerShell 命令。 |
| **BUG-02** | Core / 系统代理 | **Linux GNOME 系统代理把 `port` 硬编码为 `uint32 ("u")`，与 GNOME Schema（`int32 "i"`）冲突，导致 Linux 系统代理完全不可用** | `crates/zenclash-core/src/system_proxy/linux/snapshot.rs:193-199, 429-433, 498-500, 650` | 真实 `gsettings-desktop-schemas` 定义 `org.gnome.system.proxy.{http,https}.port` 为 `type="i"`（`int32`），但代码强制校验 `self.kind == "u"` 并调用 `g_variant_get_uint32` / `g_variant_new_uint32`；自造测试夹具 L650 也误写成 `type='u'` 掩盖了该问题。真实 GNOME 桌面执行 `detect()`/`capture()` 必然报 `InvalidNativeSnapshot`。 |

### 2.2 P1 级（高优先级功能/稳定性/架构缺陷）

#### `zenclash-core`（15 项）

| 编号 | 子系统 | 问题标题 | 精确位置 | 根因与影响 |
| --- | --- | --- | --- | --- |
| **BUG-03** | 系统代理 / Windows | **WinINET 步进校验假设 `AutoConfigUrl("")` 写入空 `REG_SZ`，与真实 Windows 删除该键行为冲突** | `system_proxy/windows/snapshot/write_plan.rs:20-36, 145-166`、`connection_transaction.rs:63-74` | `Manual` 与 `Disable` 均调用 `AutoConfigUrl("")` 并预测注册表存在空 `REG_SZ`；真实 WinINET 删除 `AutoConfigURL`，触发字节级 `same_state` 失败 → 回滚同样失败 → 锁死 `RecoveryPending`。 |
| **BUG-04** | 服务会话 / macOS | **只读 `GET /configs` 在 macOS 下暗藏特权 DNS 写副作用并争用全局写锁** | `service/session.rs:348-384` | 任何只读查询 `GET /configs` 均在 macOS 加 `self.mutations` 写锁、调用 `zenclash_service::set_tun_dns` 并产生 5 次跨进程 IPC；DNS 写失败还会令只读接口报错。 |
| **BUG-05** | 内核启动 / 更新恢复 | **`startup_binary` 扫描 `PATH` 时遇到不可访问目录（`EACCES`/`ENOTDIR`）直接中止启动** | `managed_core_process/resources.rs:36-41`、`core_update/recovery.rs:290-304` | 2026-10-19 修复 #2 引入的回归：`recover_interrupted_update` 仅忽略 `NotFound`，`PATH` 中靠前若含无权目录（如 `/root/bin`）或普通文件路径，整条 `PATH` 回退扫描立即报错退出。 |
| **BUG-06** | 服务会话 | **`ServiceSession::stop()` 与 `confirm_stop()` 在未确认 `Start` 且服务未激活时状态脱节并泄漏心跳任务** | `service/session.rs:547-567`、`service_runtime_session.rs:423-438` | `start()` 超时且服务端实际 `!is_active` 时，`stop()` 返回 `StartUnconfirmed` 且不清理 `pending_start`/`pending_keepalive`，但 `confirm_stop()` 见 `!is_active` 返回 `Ok(())`，导致后续 `start()` 永久卡死在 `StartUnconfirmed` 且心跳任务泄漏。 |
| **BUG-07** | 本地内核进程 | **`ManagedCoreProcess` 重启取消/启动失败或意外退出后泄漏全局单内核互斥锁 `CoreExecutionGuard`** | `managed_core_process.rs:255-273, 511-554` | 停止旧进程并 `reserve_execution` 后，若触发 `cancelled` 或 `spawn_child` 失败，未释放 `self.execution`；`is_running()`/`try_snapshot()` 回收已退出子进程后也未释放，导致后续无法启动新本地内核或切 Service。 |
| **BUG-08** | 配置 / 订阅 | **`ProfileApplication::prepare_change` 在耗时网络下载期间持有 `DataWriteLease`，结合恢复优先队列可冻结全应用存储长达 10–20 分钟** | `profiles/application.rs:318-334`、`data_coordinator.rs:276-295` | 申请 `DataWriteLease::shared` 后执行最高 600s×2 的远程下载；若期间并发触发备份恢复（`exclusive`），`restore_precedes` 立即阻断所有偏好/覆写/配置的读写。 |
| **BUG-09** | 配置 / 受控层 | **`ControlledConfigStore` 的 `mutation_gate` 与 `session_listener_fallbacks` 按实例私有，多实例破坏互斥与端口一致性** | `controlled_config.rs:231-245`、`core_session/backup_restore.rs:85`、`zenclash-ui/.../backup_restore/{state,recovery}.rs` | 仅 `transaction` 按路径共享；生产备份恢复流程多次 `ControlledConfigStore::new(...)`，导致互斥失效且回退端口变更不回传主 store。 |
| **BUG-10** | 配置 / 覆写 | **`YamlOverrideStore::delete_checked` 先删文件后更新清单，崩溃窗口导致仓库永久不可打开** | `yaml_overrides.rs:348-357, 398-422, 608-610` | L349 `remove_file` 与 L350 `save_unlocked` 之间崩溃后，`overrides.json` 指向不存在文件，下次启动 `load_unlocked` 报 `NotFound`，而 `quarantine_invalid_manifest` 未匹配 `NotFound` 导致无法自愈。 |
| **BUG-11** | 备份 / 覆写 | **备份解压未扣减剩余总预算且无按类型上限（2× Zip-Bomb 放大至 511 MiB），且缺失大小写不敏感查重** | `backup/archive/import.rs:81, 96-100, 150`、`yaml_overrides.rs:603` | 单条目均可读满 256 MiB（超预算 2 倍）；macOS/Windows 大小写不敏感文件系统上同名异大小写条目通过内存哈希校验后落盘互相覆盖。 |
| **BUG-12** | 配置 / 服务启动 | **`prepare_service_startup_payload` 在服务启动确认前即覆写 `session_listener_fallbacks`** | `controlled_config/service_configuration.rs:276-309`、`core_session/service_startup.rs:130-179` | 服务启动失败/取消后 `cache.rollback()` 仅还原 `effective.yaml`，已写入的临时回退端口永久泄漏至后续会话。 |
| **BUG-13** | 配置 / Diff & 局部更新 | **将 YAML 强转 `serde_json::Value` 且未展开 `<<` 合并键；服务补全的 `effective_delta` 绕过覆写冲突校验** | `config_diff.rs:74-75`、`controlled_config/source_composition.rs:195-212`、`partial_configuration_application.rs:95-125` | 含 `<<: *anchor` 的订阅产生大量误报 Diff；含非字符串键或 `.nan`/`.inf` 的合法订阅无法切模式/看 Diff；服务侧自动补全字段绕过 YAML 覆写冲突检测。 |
| **BUG-14** | 控制器客户端 | **`MihomoClient::shared_connections` 缓存命中路径绕过 `pinned_binding` 校验** | `client/connections.rs:27-37` | 缓存检查位于 `self.operation_binding()?` 之前并使用全局最新 `binding.generation()`，过期 pinned 客户端在 1s 内会静默读到新内核连接数据。 |
| **BUG-15** | 控制器客户端 | **节点/组测速与 Provider 更新未传传输超时，IPC 模式 12s 硬截断正常请求** | `client/api.rs:144-167, 193-226`、`client/request.rs:95, 151` | 传入 `timeout = None` 回落到 12s 硬超时，大组测速/长 `timeout_ms`/大规则集更新必然被 IPC 层误杀。 |
| **BUG-16** | 遥测 / 运行状态 | **`OperationalStatus` 缺 `Drop` 泄漏重资源、代次切换后 TUN 卡死 `Loading` 30 秒、`CaptureStatus::is_active()` 未校所有权** | `operational_status.rs:427-435, 526-565, 825-846` | ① 未调 `stop()` 时后台任务强持 `TrafficMonitor`/`LogMonitor`/WebSocket；② 切配置重置 `capture.tun = Loading` 后需苦等最多 30s 才触发下一次平台刷新；③ 第三方系统代理被误判为 ZenClash 已接管。 |
| **BUG-17** | 日志 / 捕获 | **日志压缩遇任意非 UTF-8 字节直接 `bytes.clear()` 销毁 50 MiB 历史日志；`enable_service_tun_with_bundle` 清理失败用 `?` 覆盖原始错误** | `logs/file.rs:364-414`、`traffic_capture.rs:536-542` | 日志截断既未按 `\n` 对齐又会因单字节损坏清空全量日志；服务 TUN 启用失败后的 `acquired.stop().await?` 会吞掉携带回滚细节的原始 `TrafficCaptureError::Rollback`。 |

#### `zenclash-ui`（13 项）

| 编号 | 子系统 | 问题标题 | 精确位置 | 根因与影响 |
| --- | --- | --- | --- | --- |
| **BUG-18** | 应用退出 | **`begin_quit_mode` 在释放系统代理/历史刷盘前不可逆地关闭 `CoreSession`，退出受阻后应用永久僵尸化** | `app/system_proxy.rs:145-186`、`core_session/automatic.rs:5-12` | L149 先调用 `core_session.request_shutdown()`（无恢复入口），当后续 `release_owned()` 或 `history.shutdown()` 失败进入 `QuitState::Blocked` 时，应用虽弹窗保持存活，但所有 Core 操作均永久报 `ShuttingDown`、自动循环退出、托盘队列卡死。自带测试绕过了 `begin_quit_mode` 掩盖了此问题。 |
| **BUG-19** | 后台自动循环 | **`StableObservation` 电平触发导致每秒调用 `resume_after_network`，引发 1Hz PAC 闪断、全局锁争用与全壳重绘** | `app/automatic.rs:13-19, 126-135`、`core_session/automatic.rs:96-115` | `observe()` 稳定后每秒都返回 `true`，而 `resume_after_network` 在检查 `network_suspended` 之前先 `pause_pac_availability()` + 取 `DataWriteLease` + 取 `transition` 锁，并触发两次 `run_state` 广播，导致空闲时每秒 PAC 闪断与 `ZenClashApp`/`RuntimePage` 全量重绘。 |
| **BUG-20** | 启动编排 | **启动取消被 `.or_else` 吞噬错误执行离线启动，且 20s×3 启动等待链不可取消** | `main.rs:414, 521-534, 1061, 1158, 1216` | Pending 阶段退出时 `ErrorKind::Interrupted` 被 `.or_else` 吞掉，继续创建 `CoreSession`/`LogMonitor`/SQLite 后再销毁；内核卡死时退出会阻塞主进程 20–60 秒以上。 |
| **BUG-21** | 命令编排 | **`apply_config` / `reapply_with_overrides` 绕过 `begin_data_application()`，保留与 2026-10-19 Audit #1 同源的覆写链 TOCTOU 竞态** | `application_commands/configuration_application.rs:73-124`、`pages/runtime/profiles/workflow.rs:145-156`、`pages/runtime/lifecycle.rs:1150-1165` | Audit #1 仅修复了 `ApplicationCommands::apply`。受控配置保存与配置重载仍在准入外提前快照 `overrides`，并发覆写变更后旧链直接覆写运行配置。 |
| **BUG-22** | 表单组件 | **`settings_switch_disabled` / `settings_input_row` 静默丢弃全部 `description` 参数** | `pages/runtime/settings/forms.rs:69-110`、`settings.rs:134-144`、`dns/sniffer/tun/mihomo/resources.rs` | 参数带下划线 `_description` 直接丢弃，导致“自启动指向旧可执行文件”的关键告警和 20+ 项设置说明全部对用户不可见。 |
| **BUG-23** | 日志搜索 | **日志搜索把空结构化字段 `Value::Null` 匹配为 `"null"`，输入 `n`/`u`/`l` 命中全部普通日志** | `pages/runtime/logs.rs:557-576` | 普通日志 `entry.fields` 默认为 `Value::Null`，`(!entry.fields.is_object() && json_value_matches(&entry.fields, query))` 匹配 `"null"`，输入任意 `n`/`u`/`l` 子串即令搜索失效。 |
| **BUG-24** | 规则筛选 | **Rules 页“仅看已禁用”筛选忽略用户刚确认的 `confirmed_disabled`** | `pages/runtime/rules/projection.rs:73-74` vs `rules.rs:204-207` | 切换规则后只写 `confirmed_disabled` 不改快照，投影过滤只读 `rule.extra.disabled`，导致界面状态与筛选结果自相矛盾。 |
| **BUG-25** | 列表筛选 UX | **Connections / Rules 切换标签或筛选时手动置空 `projection = None`，整页闪空 80ms+ 并销毁键盘焦点** | `pages/runtime/connections/dashboard.rs:44-54, 263, 315`、`rules/dashboard.rs:16-33, 83, 121` | `projection = None` 叠加 `sleep(80ms)` 触发整页降级为空状态，卸载所有卡片、图表与控件并破坏焦点。 |
| **BUG-26** | 代理页性能/算法 | **搜索中整组测速每 50ms 清空 `search_projection`，80% 时间闪烁 `"Loading..."` 并重置滚动与焦点** | `pages/proxies/actions.rs:46-125`、`measurement.rs:454-456`、`node_grid.rs:388-405` | `prepare_search` 先 `cancel_search()` 置 `None` 再 `sleep(40ms)`，每 50ms 测速批次反复触发，清空所有卡片焦点并跳回顶部。 |
| **BUG-27** | 代理页性能 | **`ProxiesPage::observe_self` 盲目全量失效列表测量，与 `apply_height_hint` 形成冗余重测死回环** | `pages/proxies.rs:184-185`、`node_grid.rs:118-145, 350-355, 552-568` | `apply_height_hint` 调用 `cx.notify()` → `observe_self` 设 `measurements_stale = true` → 下帧 `remeasure_items(0..N)`；任何微小通知（进度条、单节点延迟）均触发全量重测。 |
| **BUG-28** | 连接页性能 | **`ConnectionTimeline::observe` 在 UI 主线程执行 $O(N^2)$ 关闭连接合并与重复时间解析** | `pages/runtime/connections/timeline.rs:60-71` | 批量关闭 1,000 条连接时在渲染主线程执行最多 $10^6$ 次 `Vec::retain` + `insert(0)` 搬移与 1,000 次时间解析。 |
| **BUG-29** | Mihomo 设置 | **`Page::Mihomo` 的“取消”按钮 100% 失效（No-op）** | `pages/runtime/mihomo.rs:572`、`config_inputs.rs:205-214` | `ConfigInputs::reset_page` 的 `match page` 遗漏 `Page::Mihomo` 分支，点击取消完全无效果。 |
| **BUG-30** | 无障碍 / 导航 | **侧栏主题按钮可见文本与 Tooltip/无障碍名语义相反；Traffic 排行/明细行不可键盘访问** | `components/sidebar.rs:254-271`、`pages/runtime/traffic/view.rs:396-409, 539-552` | 深色态按钮视觉显示“深色模式”而 tooltip/a11y 却是“浅色模式”（违背 WCAG 2.5.3）；Traffic 列表行用普通 `div` + `.cursor_pointer().on_click`，无焦点与键盘激活。 |

---

## 三、分维度全面分析

### 3.1 软件架构设计分析

#### 1. `zenclash-core`：扁平化大一统 crate + 10 层互斥锁 + 网状循环依赖
- **6 对直接双向循环依赖**（基于解析 `lib.rs` 329 个平铺重导出后的真实引用图）：
  - `client <-> endpoint`、`client <-> managed_core_process`
  - `core_session <-> operational_status`、`core_session <-> proxy_operations`、`core_session <-> traffic_capture`
  - `preferences <-> system_proxy`
- **“客户传输层”与“运行态所有权”错位**：`MihomoClient` 名为 HTTP/IPC 客户端，却通过 `ControllerBinding` 持有 `Arc<ManagedCoreProcess>` 与 `Arc<ServiceRuntimeSession>`（`client/transport.rs:19-25`），导致所有上层（`CoreSession`、`ServiceManager`、`TrafficCaptureSession`）必须通过客户端反查和切换内核所有权。
- **10 层互斥门控链路**：
  `ServiceManager::command` → `RunStateStore::operation_running` → `data_application_gate` → `capture_publication_gate` → `DataWriteLease` → `CoreSession::transition` → `ControlledConfigStore::service_tun_mutation` → `MihomoClient::mutation_gate` → `ServiceRuntimeSession::state` → `ServiceSession::mutations`。
  其中还存在显著冗余：`ServiceManager::command` 与 `RunStateStore::begin_operation` 职责重叠（`service_manager.rs:462-518` 竞争时误将第二实例标为 `Failed`）；`service/runtime_bundle.rs` 与 `service_runtime.rs` 并存两套资源包收集器（并已导致 Linux 下 `geoip.dat`/`GeoSite.dat` 大小写漏采）。
- **四层 Service 抽象叠床架屋**：`service/*`（GPL Fork IPC 适配）+ `service_runtime/*`（资源冻结）+ `service_runtime_session/*`（候选/回滚状态机）+ `service_manager/*`（维护命令），学习与排障成本极高。
- **同步阻塞渗入 Tokio 异步线程**：`ManagedCoreProcess::stop_running_child` 持有 `self.child.lock()` 同步 `sleep` 最长达 3 秒，而 `run_state.rs`、`managed_core_process.rs`、`core_session/{updates,automatic,supervision}.rs`、`traffic_capture/backend.rs` 均在 `async fn` 中直接加锁；`data_coordinator.rs:295` 持全局 `Mutex` 执行递归 `canonicalize`/`read_link`；`controlled_config` 在异步函数内执行同步 YAML 解析与 `socket2` 端口扫描。

#### 2. `zenclash-ui`：无头业务编排误植 UI + 双上帝实体 + 壳/组件依赖倒置
- **4,000+ 行零 GPUI 依赖的业务编排层寄生在 `zenclash-ui`**：`application_commands/*`（~2,300 行生产 + ~1,800 行测试）、`outbound_mode/coordinator.rs`（373 行）、`app/traffic_history.rs`（700 行）、`startup.rs`（395 行）、`app/automatic/source_scanner.rs`（179 行）完全不依赖 `gpui_kit`，却全部塞在 `zenclash-ui` 中，拉慢增量编译并阻碍无头测试/复用。
- **`main.rs` 极度膨胀（1,610 行）**：混合日志初始化、单实例锁、备份恢复、11 处 `runtime.block_on`、Windows 30s 轮询、3 级核心降级启动机、GPUI 窗口与退出重启。
- **双上帝实体违背 GPUI Kit 分层**：
  - `ZenClashApp`（42 字段、15 个 `impl` 文件、6 个 `.detach()` 常驻循环——其中 `start_profile_updates` 与 `start_service_handoff` 存在实体销毁后泄漏，且 3 处 `cx.spawn` 直接 `tokio::time::sleep` 依赖隐式主线程 `runtime.enter()`）。
  - `RuntimePage`（47 字段，把除 `Proxies` 外的 **15 个页面**绑进同一个 `Entity`，启动即迫切创建 36 个配置输入框 + 12 个 Profile 输入框 + 3 个子实体 + 12 个订阅；每 500ms Home 流量刷新触发 4 个 `observe_self` 全量同步隐藏页面并重算反馈列表）。
- **依赖方向倒置（2 对双向循环）**：`components/sidebar.rs` 与 `pages/{proxies,runtime}` 直接 `use crate::app::{SetRuleMode, Navigate*, ToggleSidebar, ...}` 向上依赖应用壳，同时 `app` 又向下依赖 `components` 与 `pages::runtime::*`，直接违背 GPUI Kit Coding Guides（“Do not make a reusable component depend on an application screen; a feature must not depend on the shell”）。
- **依赖注入形同虚设**：5 处后台代码（`app/profile_updates.rs:18`、`app/tray/commands.rs:282`、`app/tray/directories.rs:15`、`app/automatic/source_scanner.rs:48`、`main.rs:1128`）绕过注入的 `ProfileStore`/`YamlOverrideStore` 重新调用 `::discover()`，测试期间会污染宿主机真实用户目录。
- **启动交接脆弱**：`adopt_startup`（`app.rs:489-522`）通过 `*self = Self::new(...)` 整体替换实体，导致 `preferences_reconciliation_read` 丢失、Pending 阶段页面状态销毁、隐藏窗口时焦点与释放顺序颠倒；`finish_startup` 遇到错误时永久卡死在 `initializing = true`；Pending 阶段还针对无效地址 `127.0.0.1:0` 白白拉起 `TrafficMonitor`、`LogMonitor` 及独立日志写线程。

---

### 3.2 代码文件组织分析

1. **Core 顶层目录过度扁平（76 项平铺）**：
   - TUN 拆为 4 个极小顶层文件：`tun_admission.rs`（97L）、`tun_config.rs`（168L）、`tun_permissions.rs`（48L）、`tun_runtime.rs`（193L）。
   - 单复数模块并存：`profile.rs`（332L，仅 `lib.rs` 导出 1 个函数）与 `profiles.rs` + `profiles/` 割裂；通用底层 I/O（`atomic_write`、`read_profile_bytes`）寄生在 `profiles/storage.rs`，通用 Windows 私有文件 ACL 寄生在 `service/owner_identity.rs`，迫使无关领域交叉引用。
   - `controlled_config/` 将单一 `ControlledConfigStore` 横切为 10 个子文件且大量 `use super::*`，实质是掩盖后的 God Object。
   - 4 个 Store 重复实现 `quarantine_invalid_*`；`network/probe.rs:396` 重复实现已有 `http_body::read_limited`。
2. **超大单文件与内联测试混编**：
   - Core：`traffic_capture.rs`（2,593L，含 1,538L 测试）、`operational_status.rs`（1,887L，含 805L 测试）、`system_proxy.rs`（1,399L）、`service/runstate/mod.rs`（1,305L）、`logs.rs`（1,205L）、`core_session/service_tun.rs`（单函数 `enable_service_tun_admitted` 达 346L）。
   - UI：`pages/runtime/home.rs`（2,488L）、`main.rs`（1,610L）、`lifecycle.rs`（1,495L）、`proxies/view.rs`（1,252L）、`network.rs`（1,133L）、`config_inputs.rs`（1,084L）、`profiles/view/catalog.rs`（1,052L）、`connections/dashboard.rs`（1,022L）、`rules/dashboard.rs`（985L）。
3. **UI 同能力水平撕裂与历史遗留**：
   - 偏好分散在 `preference_previews.rs` / `app/menu_preferences.rs` / `app/preferences.rs`；出站模式分散在 `outbound_mode/` / `app/outbound_mode.rs`；原生 OS 托盘适配器错放在 GPUI 组件目录 `components/tray/`。
   - 残留空目录 `crates/zenclash-ui/src/app/controllers/wifi/`；`project_root()` 硬编码 `env!("CARGO_MANIFEST_DIR")` 在 4 个文件重复定义。
   - 残留 773 行仅 `#[cfg(test)]` 的已废弃首页代码（`home/flow.rs` 467L + `home/history.rs` 306L），且牵连 `HomeChartState::connection_samples` 在生产环境每 500ms 白白克隆 `operational_status` 快照。
   - `lifecycle.rs:224-344` 硬编码重复维护 19 个配置输入框的本地化占位符清单（已与 `config_inputs.rs` 漂移）。

---

### 3.3 代码命名与领域词汇对齐分析

| 类别 | 具体问题 | 位置 | 建议 |
| --- | --- | --- | --- |
| **同概念多套叫法（违背 `CONTEXT.md`）** | “托管内核”同时出现 `Local` / `Sidecar` / `Managed`；“外部内核”同时出现 `Direct` / `External`；`app.yml` 有 13 处“当前配置”和 1 处“临时内核”（`CONTEXT.md` 明确要求“有效配置”/“托管内核”） | `service/running_mode.rs:5-12`、`owned_core.rs:21-23`、`core_session.rs:218`、`app.yml` 14 处 | 统一领域词汇：`ManagedLocal` / `ManagedService` / `External`，清理禁用词 |
| **易混淆双生名称** | `ServiceSession` vs `ServiceRuntimeSession`；`ServiceRuntimeBundle` vs `zenclash_service::RuntimeBundle`；`ServiceHealth` vs `ServiceHealthKind`；`SystemProxySummary = SystemProxyStatus` | `lib.rs:17-30`、`service_manager.rs:26`、`system_proxy/summary.rs:44` | 按职责重命名并消除等价双名 |
| **名不副实的文件/类型/字段** | `app/system_proxy.rs` 实际是全应用退出状态机；`RuntimePageServices::profile_service` 类型实为 `ApplicationCommands`；`ReplyBody::Service` 同时承载 Local IPC；`ProfileStoreError::InvalidYaml` 承接 Cron/URL/目录错误导致“Clash YAML 无效：Cron 必须 5 字段”；`ProfileApplyOutcome::source_version` 注释强调它不是版本 | `app/system_proxy.rs`、`pages/runtime.rs:141`、`client/request.rs:183`、`profiles/store.rs:22`、`profiles/application.rs:31` | 修正模块/字段/错误变体命名，令名称直述语义 |
| **历史兼容别名与死字段** | `MihomoProcess`/`MihomoLaunchConfig`/`MihomoProcessSnapshot`（仅 3 个集成测试用）、`ProfileService = ApplicationCommands`（0 调用者）、`CoreBackupAdmission`、`ZenClashApp::tray_core_running`（只写不读） | `zenclash-core/src/lib.rs:73-78`、`zenclash-ui/src/lib.rs:30`、`app.rs:128` | 在重构窗口统一移除 |
| **GPUI 命名与 Action 样板** | `can_switch`/`can_delete` 违背 Coding Guides 避免新 `can_` 谓词；16 个零尺寸 `Navigate*` Action 产生 4 处 16 路重复样板；`common::metric` 用本地化翻译文本当 `ElementId`；`logs/dashboard.rs:305` 用 `Arc::as_ptr` 当 `ElementId` | `home.rs:321`、`app.rs:80-112`、`common.rs:140`、`logs/dashboard.rs:305` | 统一 `is_`/`has_` 谓词、稳定域 ID、单调序号 ID、收敛导航 Action |
| **中英文错误与单位混杂** | Core 错误文本混用硬编码中文、硬编码英文与 `zenclash_i18n`；`tray/menu.rs:134` 硬编码中文全角括号 `（{delay} ms）`；`traffic.rs:253` 在 `1000..=1023` 混入十进制 `KB/s`；`home.rs:1829` 用十进制 `MB/GB` 而 `common.rs:195` 用二进制 `MiB/GiB` | `client/api.rs`、`logs.rs`、`tray/menu.rs:134`、`traffic.rs:253`、`home.rs:1829` | 统一错误 i18n 策略与 IEC 二进制单位标准 |

---

### 3.4 UI 设计、组件与无障碍分析（对照 `gpui-kit-design-guides`）

1. **色彩对比度与主题令牌缺陷 (`design.rs`)**：
   - **4 组语义状态色对未达 WCAG AA 4.5:1**（`design.rs:111-125`）：
     - 浅色 `success_foreground` on `LIGHT_SUCCESS` = **3.51:1**
     - 浅色 `warning_foreground` on `LIGHT_UPLINK` = **3.61:1**
     - 浅色 `danger_foreground` on `LIGHT_DANGER` = **3.57:1**
     - 深色 `info_foreground` on `dark_info` = **2.45:1**（连大字 3.0:1 都未达标！）
   - **跨平台与层级违规**（`design.rs:187-195`）：无条件硬编码 macOS 字体名 `".SystemUIFont"` 与 `"SF Mono"`；`theme.radius == theme.radius_lg == 8px` 破坏同心圆角层次；`theme.shadow = false` 让 Dialog/Popover/Menu 浮层失去阴影分隔；4 处组件借用图表色 `theme.chart_3` 充当交互强调色；`sidebar.rs:288-311` 仅在设置页改变侧栏在线指示点颜色。
2. **布局、滚动与表格对齐断裂**：
   - **嵌套垂直滚动冲突**：`RuntimePage` 外层无条件包裹 `runtime-body-scroll`，与 `Connections`/`Rules`/`Logs` 内部表格滚动及 `Overrides` 预览滚动冲突，且切换页面不重置 `body_scroll`。
   - **五页表格对齐脊线断裂**：`Connections` / `Logs` / `Rules` / `Network` / `Resources` 表头固定列普遍缺少 `.flex_shrink_0()`，行尾条件渲染的 `Retry`/`Delete` 缺少固定宽度动作槽，窄窗或出现失败行时表头与列错位。
   - **弹窗缺陷**：`open_network_options` 与 `manage-core-binary` 与背后主页面共享相同 `ElementId`；`open_network_options` 超高无滚动且叠层弹出 `AlertDialog`；`logs/dialogs.rs:69` 详情无高度上限和滚动；`profiles` 远程编辑弹窗遗漏渲染 `request_timeout_seconds` 和 `update_cron` 两项已绑定字段，且缺忙锁/重入/切页关闭保护；10+ 处重复手算弹窗垂直居中 `margin_top`。
3. **组件重造与交互不一致**：
   - 大量重造已有 `gpui-kit` 标准组件：`MintSwitch`（缺 hover/pressed 与 `Size`）、`ChoicePopover`（175L 手写下拉替代 `Select`）、手写 `TabBar`（Settings/Connections）、两套风格不一致的手写分页（Rules vs Connections）、五套手写 `div` 表格、三套无复制反馈的剪贴板按钮、五套卡片容器函数（`panel` 复制 3 份 + `setting_card` + `network_card`）。
   - 交互规范漂移：`.small().h_10()` 混用；多处关闭按钮滥用 `.primary()`；`IconName::Globe` 被错误用作“清空流量历史”和“跟随系统主题”；`connections/dashboard.rs` 链式 `.outline().danger()` 与 `.danger().outline()` 顺序写反；破坏性确认在 `AlertDialog` / 行内二次按钮 / 普通 `Dialog + Primary("Confirm")` 间分裂；`system_proxy/view.rs` 点击模式按钮意外弹出编辑器；托盘非可选组尾部多出悬空分隔线（`tray/menu.rs:123`）。
4. **i18n 与无障碍**：
   - `app.yml` 存在 **415 条废弃未用键**（占 1,984 条的 20.9%）与三套过渡命名空间（`redesign.*`/`settings_redesign.*`/`unified.*`），**252 条英文标签使用 Title Case** 与 Sentence Case 混杂；`zenclash-i18n` 单测漏检 `ai_network.yml`。
   - 侧栏主题按钮文案与 a11y/tooltip 相反、侧栏展开态冗余 tooltip、侧栏宽 `9rem`（144px）窄于设计文档约定 224px；Traffic 列表行不可键盘聚焦；`log-draft-file-enabled` 与 `network.rs` 删除图标按钮缺失 `accessibility_label`；关闭单连接确认框未指明目标主机/进程。

---

### 3.5 算法与性能分析

| 领域 | 位置 | 现状与复杂度/开销 | 优化方向 |
| --- | --- | --- | --- |
| **配置合成管线** | `controlled_config/configuration_application.rs` 等 | 单次受控设置保存执行最高 **12 次全量 YAML 解析 + 11 次全量序列化**，且同步阻塞 Tokio 线程 | 统一为单次 Parse → `Value` 态 Merge/Normalize/Project → 单次 Serialize，整段放入单次 `spawn_blocking` |
| **YAML Diff** | `config_diff.rs:74, 94, 136` | 每层递归重复 `source == effective` 产生 $O(D \cdot N)$ 比较；`value_preview` 全量序列化大数组后截取 240 字符 | 直接在展开 `<<` 后的 `serde_yaml::Value` 上 Diff，仅叶/截断判断相等，流式有界预览 |
| **覆写加载** | `yaml_overrides.rs:438` | 每次开关/排序元数据操作均全量解析全部覆写 YAML（上限 1,024×4 MiB） | 分离元数据加载与内容校验 |
| **SQLite 流量历史** | `app/traffic_history.rs:260`、`traffic_history/storage.rs:103-248` | 空闲时每 **5 秒**无条件新开连接 + 4×`fchmod` + 7×DDL + 百万行 `COUNT(*)` + 2×DELETE + WAL checkpoint（每天 17,280 次空写盘）；5 个二级索引写放大 | 空批次跳过刷盘；连接复用；低频/节流裁剪（用 `OFFSET` 或累计计数替代每次 `COUNT(*)`）；精简为覆盖索引 |
| **PAC 服务** | `system_proxy/pac/http.rs:24-60` | 非阻塞 `accept()` + `sleep(5ms)` 导致空闲 200Hz 唤醒 CPU，单线程串行易被慢连接阻塞 | 改为事件驱动阻塞 `accept` + 关闭自唤醒或 Tokio `TcpListener` |
| **日志落盘与内存估算** | `logs/file.rs:180-362`、`logs.rs:47-58` | 线程无条件创建且未 `join`；每 1 条日志执行 6 次系统调用；每帧为算长度重新 `serde_json::to_vec` | 懒启动 + `try_recv` 批写 + `O_NOFOLLOW` + 直接使用原始帧长 |
| **运行态轮询** | `operational_status.rs:787-832` | 每 5s 丢弃 83% 的 `/configs` 响应；每 5s 反复建拆 `/memory` 与 IPC `/connections` WebSocket | 条件拉取 `/configs`，复用 `connections.memory` 或长连接 |
| **端口回退探测** | `listener_fallback.rs:194, 305` | `ipv6` 默认 `true` 与 Mihomo 矛盾；遇到 `EADDRNOTAVAIL`/`EACCES` 盲扫 64,511 端口（25.8 万次 syscall） | `ipv6` 默认 `false`；非冲突错误立即短路 |
| **UI 热路径** | `connections/timeline.rs:60`、`rules/dashboard.rs:220`、`rules/projection.rs:66`、`logs/projection.rs:68`、`home.rs:154`、`proxies/node_grid.rs:350` | 主线程 $O(N^2)$ 关闭合并；每帧扫万条规则；后台数万次无谓 `String::clone`；Home 每 500ms 3 次快照克隆 + 600 个 `SharedString`；按钮点击强加 80ms debounce；虚拟列表重测死回环 | 主线程 $O(N)$ 去重；后台预聚合计数与 `BTreeSet<&str>`；区分输入防抖（80ms）与按钮即时响应（0ms）；按数据版本缓存图表点；按需局部 `remeasure_items` |
| **偏好与托盘刷新** | `app/preferences.rs:146`、`app/menu_preferences.rs:178`、`app/tray/refresh.rs:22-103` | 10ms 无上限递归重试；改主题走全量 `Restore` 副作用风暴；托盘无 `PartialEq` 每次销毁重建原生菜单且 macOS 右键被 HTTP 阻塞 | 指数退避 + 窄 `PreferenceScope` + `TrayMenuState` diff + 优先弹出缓存菜单 |

### 3.6 其他已验证中低优先级问题索引（P2/P3 精选）
- **跨平台隐患**：`autostart/windows.rs:17-70` 用 UTF-8 解码 `schtasks`/`reg.exe` 的 UTF-16/GBK 输出，中文用户名自启动校验必败；`network/probe.rs:261` & `ai_network.rs:148` IPv6 代理 URL 漏方括号；`network/reachability/windows/uplink.rs:44` 未排除 IPv6 链路本地地址；`profiles/validation.rs:17, 151` 遗漏 Windows 多点保留名（`CON.foo.yaml`）且 `slug` 按字符截断可能超 255 字节；`backup/mod.rs:29` 向上扫 `/` 遇 `PermissionDenied` 阻断全应用写入；`ruleset.rs:216` 在 `/tmp` 建文件缺 `0o600` 权限；`profiles/storage.rs:102` `atomic_write` 在 `rename` 前未 `sync_all()`；`profiles/storage.rs:43` & `backup/archive/import.rs:66` 未防 FIFO 阻塞。
- **并发与生命周期**：`supervise_managed_core` 在 `pin_binding()` 失败时 100% CPU 空转（`core_session/supervision.rs:78`）；`service_runtime_session/cache.rs:23` 对 2 秒内刚写入的 provider 缓存直接报错无等待重试；`websocket.rs:48` 非 `/logs` 流未限消息体积且无心跳/读超时，`/traffic` 断线后不归零速度；`logs.rs:479` 遗漏 `borrow_and_update()` 触发多余重连；`OperationalStatus::record_path:581` 进程未就绪默认代次 0 误放行；`ProxyOperations` 部分方法漏校 `observation_identity`；`NetworkTrayIcon::drop` 无条件清空全局 `NATIVE_EVENT_SENDER`；`ProfileCatalogView` 用裸指针地址查重存在 ABA 风险；`Home` 捕获开关绕过 `MutationState`；`Home` “更多节点”在当前组为 LoadBalance 时错误禁用其他可切组且误显示 `controller_name()`；`Proxies` 测速写入空时间戳致历史图时间全为 `"—"`；`ConfigInputs` 无法清空 `tun.mtu` / `dns.fallback-filter.geoip` 且读取失败后每秒重复解析；`report_automatic_runtime` 错误被 `refresh(cx)` 清空；`Connections` Closed 态饼图中心仍显示“活跃连接”。
- **微小健壮性**：`core_update/workflow.rs:301` `unwrap_err()` 潜在 panic；`pages/proxies/view.rs:1064` `details_focus[&...]` 直接索引风险；`data_coordinator.rs:169` 生产 `assert!`；`app_update.rs:252` API 重定向 URL 仍大小写敏感；`startup_service_health()` 误用 `current_dir()`；`Profiles` 控制器全败时静默吞错。

---

## 四、下一步行动指引

关于目标架构蓝图、模块边界重划、锁层级收敛、新配置合成管线、UI 能力自治拆分、设计系统修正与分阶段演进路线，请直接阅读配套设计文档：[software-design-v2-2026-11-21.md](software-design-v2-2026-11-21.md)。


---

## 五、复核意见（2026-11-21 第二轮，人工逐行核对）

> 第一轮发现大多由并行子代理产出。本轮逐行核对了下列条目，其余条目**尚未独立复核**，修复前须先复现。

### 5.1 已复核结论

| 编号 | 复核结论 | 修正后严重度 | 说明 |
| --- | --- | --- | --- |
| BUG-01 | **成立** | P0（需 Windows PoC） | `tun_runtime.rs:55` 只做 `trim()`。`tun_config.rs:29` 只在缺省时补 `"Mihomo"`，订阅可以自定义设备名。`powershell -Command` 会把后续参数拼进脚本，所以正常设备名会让 `ConvertTo-Json` 报错，返回 `Unknown`；含 `;` 的设备名则可能执行任意命令。 |
| BUG-02 | **代码层成立** | **降为 P1** | `snapshot.rs:128` 读取的是 GVariant 的真实类型；测试夹具 `:650` 写成了 `type='u'`。上游 GNOME schema 为 `i`，需在实机确认。读取阶段就会失败、不会写入，属于 fail-closed，没有数据损坏，因此不定为 P0。 |
| BUG-04 | **属有意设计，不是缺陷** | 改列为 P2 架构债 | `session.rs:370-376` 与 `tun_config.rs:5-9` 明确注释，这是 2026-10-10 macOS DNS 回归的防护措施。拆分时必须保证“每次接纳配置后都同步 DNS”，不能简单删除。 |
| BUG-18 | **成立** | P1 | `request_shutdown`（`core_session/automatic.rs:5-12`）没有任何恢复接口；在 `app/system_proxy.rs:149` 它先于释放执行。 |
| BUG-19 | **部分成立** | **降为 P2** | 电平触发属实：稳定后每秒都会调用 `resume_after_network`，并且在检查 `network_suspended` 之前已取租约和 transition 锁、暂停 PAC。但“全壳每秒重绘”**不成立**：模式不变时 `reconcile_run_state` 只走 `core_start_settled`（`run_state.rs:212-218`），不会 `publish`。实际影响是 PAC 每秒短暂不可用；若有长事务持锁，不可用期会被拉长。 |
| BUG-22 | **成立** | 降为 P2 | `forms.rs:71,104` 丢弃了 `_description`，`settings.rs:134-144` 传入的“自启动已过期”提示因此不可见。 |
| BUG-23 | **部分成立** | **降为 P3** | 只在查询串恰好是 `"null"` 的子串（`n`/`u`/`l`/`nu`…）时误命中，属于输入过程中的瞬时现象。“搜索实质失效”属于夸大。 |

### 5.2 对新设计文档的修正

1. **对比度数值**：深色 `info` 改用 `DEEP_INK` 后，对比度是 **7.42:1**，不是 8.15:1。浅色三组改白字后的 5.38 / 4.93 / 5.32 核对无误。
2. **“10 层锁 → 4 层”方案待重新论证**：这组锁大多守护不同对象，持有时长也不同；文中列出的并不是一条真实的嵌套链。例如 `data_application_gate` 要跨完整的数据应用期持有，`mutation_gate` 只守护控制器写操作。合并成单一 `SessionGate` 可能把读操作串行化，甚至引入死锁。正确做法是先画出实际的持锁图、给出全局获取顺序，再逐把评估能否合并。
3. **BUG-04 的修复建议**：改成“抽出显式的 `sync_tun_dns` 步骤，并在每一个接纳点调用”，同时补回归测试，保证“只读查询不触发特权写”与“DNS 必然同步”两条同时成立。
4. **BUG-03（WinINET）**：结论依赖对 Windows 行为的推断，应视为**未验证假设**。必须先在实机抓取 `InternetSetOptionW` 前后的注册表差异，再决定是否修改。

### 5.3 尚未复核的条目（修复前需先复现）

- Core：BUG-03、05–17，以及 3.6 节所列 P2/P3。
- UI：BUG-20、21、24–30，以及 3.6 节所列 P2/P3。
- 1.3 节的分级计数沿用第一轮结果，要待全部复核完成后更新。

建议按以下顺序推进：先修 BUG-01 并补 Windows PoC 与回归测试，再修 BUG-18。BUG-02 在 GNOME 实机确认类型后修复。其余条目逐条复现，确认后再进入修复。
