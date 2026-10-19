# zenclash-ui 逐行审计报告

审计日期：2026-10-19。审计方法：`ponytail-audit`。

审计对象是 `crates/zenclash-ui` 的工作区，基于提交 `c906cfe`，包含审计时已有的未提交修复。以下保留审计时的问题描述与编号；后续代码落实和验证结果见末尾“修复记录”。原定位行号以审计时快照为准。

## 范围与结论

`zenclash-ui` 负责 ZenClash 的桌面界面、配置操作、监控和托盘。审计按单用户长期运行、连接频繁变化、后台更新与界面操作可能重叠的负载进行。

共逐行检查 **237 个 Rust 文件、69,863 行**，包括构建脚本、测试及平台条件分支；审计前后的源码 SHA256 一致。分区覆盖如下：

| 分区 | Rust 文件数 | 行数 |
| --- | ---: | ---: |
| 启动、应用外壳、托盘、组件及共享运行时 | 79 | 19,010 |
| 代理、业务命令、资源、规则及模式协调 | 54 | 17,712 |
| 配置输入、订阅、设置、覆写及代理/TUN/内核设置 | 58 | 15,801 |
| 首页、连接、流量、日志及网络诊断 | 46 | 17,340 |
| 合计 | 237 | 69,863 |

确认 **9 项 P2 功能问题、1 项 P3 可删减代码建议**。优先处理覆写链一致性、DNS 映射删除及过期状态发布。

P2 表示需要修复的功能缺陷；P3 表示较低优先级的维护或性能改进。本报告记录静态分析及隔离复现的结论，不代表真实平台交互已验收。

## Must fix

### 1. [P2] 配置应用可能覆盖刚启用的覆写链

定位：[application_commands/profile_application.rs:133](../../crates/zenclash-ui/src/application_commands/profile_application.rs#L133)，同文件的订阅更新入口也使用相同顺序。

- **作用：** 激活配置、更新订阅前，读取已启用的覆写文件链。
- **问题：** 读取覆写链与取得配置操作准入之间存在空隙。另一入口在此期间启用或重排覆写后，旧命令可以使用新运行时版本和旧覆写链成功应用配置。
- **最小修复：** 在同一操作准入期间读取并校验覆写目录版本，再应用配置；不能先读取旧路径，再单独采样新的运行时版本。
- **不修后果：** 目录显示覆写已启用，实际运行配置却可能未包含它。

**触发与证据：** 自动订阅更新、托盘激活配置与覆写页面不共用一个界面忙锁。在临时副本中，仅于读取覆写链后加入调度暂停：A 读取空链后暂停；B 启用 `mode: direct` 覆写并成功提交；A 恢复后激活配置，发送的内容又变成 `mode: rule`，磁盘目录仍显示覆写已启用。该隔离复现已通过。

**回归验证：** 延迟旧配置命令，完成一次覆写变更，再恢复旧命令；旧请求应被拒绝，或应用最新覆写链。

### 2. [P2] 删除 DNS 映射后，旧条目仍然生效

定位：[runtime/config_inputs.rs:606](../../crates/zenclash-ui/src/pages/runtime/config_inputs.rs#L606)、[映射输入构造:860](../../crates/zenclash-ui/src/pages/runtime/config_inputs.rs#L860)。底层合并逻辑位于 [core/profile.rs:162](../../crates/zenclash-core/src/profile.rs#L162)。

- **作用：** 保存 Hosts 和 Nameserver Policy 编辑器内容。
- **问题：** 编辑器提交的是剩余的完整映射，但底层递归合并映射，缺失的键不会被删除。删除单项和清空映射都会保留旧键。
- **最小修复：** 为这两个完整编辑的映射提供明确的替换语义。
- **不修后果：** 保存提示成功，但旧域名解析或 DNS 分流规则仍然生效。

**触发与证据：** 初始映射含 `old.example` 和 `keep.example`。提交空映射，或提交仅含 `keep.example` 的映射，调用现有 `ControlledConfigStore::prepare_json_update` 后生成的 YAML 均仍含 `old.example`。该配置合成复现已通过。

**回归验证：** 分别测试 Hosts 和 Nameserver Policy 的删除单项、清空及保存后重新读取；最终有效配置中不应出现已删除键。

### 3. [P2] 后台模式同步会接受过期配置

定位：[app/outbound_mode.rs:34](../../crates/zenclash-ui/src/app/outbound_mode.rs#L34)、[outbound_mode/coordinator.rs](../../crates/zenclash-ui/src/outbound_mode/coordinator.rs)。

- **作用：** 每五秒同步运行模式及日志级别。
- **问题：** 只校验模式操作序号，没有校验配置版本。配置应用等操作可以推进运行时版本，而不推进该模式操作序号。
- **最小修复：** 使用已有 `CoreObservation` 保留读来源，并在发布模式和日志级别前验证来源仍然有效。
- **不修后果：** 界面模式与实际运行状态不一致，日志监听级别也可能被旧值覆盖。

**触发与证据：** 延迟一条返回 Rule 的旧 `/configs` 请求；内核成功提交 Global，并推进到 generation 1；释放旧请求后，当前协调器仍接受它，将显示模式改回 Rule。该隔离复现直接使用当前协调器实现，已通过。

**回归验证：** 配置或运行时切换后，旧模式轮询结果不得更新显示模式或日志监听级别。

### 4. [P2] 全部网络探测失败，仍显示“正常”

定位：[runtime/network/dialogs.rs:13](../../crates/zenclash-ui/src/pages/runtime/network/dialogs.rs#L13)、[runtime/network.rs:93](../../crates/zenclash-ui/src/pages/runtime/network.rs#L93)。

- **作用：** 显示诊断项状态及通过数量。
- **问题：** 使用 `outcome.is_ok()` 判断健康，但 core 会把公网 IP 和延迟请求的失败保存在 `Ok(snapshot)` 内。取得结构化结果不等于探测成功。
- **最小修复：** 根据结果内实际成功和失败的信息判定，并让行状态与汇总共用规则。
- **不修后果：** 故障网络被标记为 Healthy，并增加“正常”检查数量，影响排障判断。

**触发与证据：** 控制器可达，但某条网络路径的公网 IP 请求及所有延迟请求均失败。当前界面仍按外层 `Ok` 显示“正常 / Healthy”。已核对 core 的结果语义、界面文案，并通过隔离断言复现。

**回归验证：** 至少覆盖全部探测失败、部分成功、全部成功；不能仅以任务是否返回 `Ok` 作为健康结论。

### 5. [P2] 启动完成会丢失偏好保存及重试状态

定位：[app.rs:439](../../crates/zenclash-ui/src/app.rs#L439)、[应用状态替换:469](../../crates/zenclash-ui/src/app.rs#L469)、[app/menu_preferences.rs:59](../../crates/zenclash-ui/src/app/menu_preferences.rs#L59)。

- **作用：** 启动准备完成后，重建正式应用状态。
- **问题：** 整体替换 `Self` 会清空偏好写入任务链和保存凭据。旧任务稍后完成或失败时，因找不到对应凭据而忽略其结果。
- **最小修复：** 保留任务链、保存状态及未完成的预览，并将预览合入重建后的偏好。
- **不修后果：** 设置选中值可能与磁盘不同；保存失败时也失去错误提示和重试入口。

**触发与证据：** 用户在启动等待期间修改主题或托盘偏好，写入尚未完成或已经失败时，启动完成执行 `Self::new`。该路径会重置 `preferences_save_task` 和 `menu_preference_saves`；随后旧完成事件无法触发偏好刷新。此项经调用链确认，未单独运行 GUI 复现。

实际全局主题由操作即时修改，因此不能据此断言画面一定恢复旧主题；确认的问题是偏好状态、保存结果与重试状态丢失。

**回归验证：** 沿用现有菜单偏好测试的可控任务队列，在释放写入前完成启动；验证预览、最终选中值和失败重试均保留。

### 6. [P2] 切到已关闭连接会清零指标并清空趋势

定位：[runtime/connections.rs:263](../../crates/zenclash-ui/src/pages/runtime/connections.rs#L263)、[指标历史更新:298](../../crates/zenclash-ui/src/pages/runtime/connections.rs#L298)、[卡片渲染:553](../../crates/zenclash-ui/src/pages/runtime/connections/dashboard.rs#L553)。

- **作用：** 在活动连接和已关闭连接之间切换列表。
- **问题：** 已关闭列表快照仅填充连接列表，其流量、内存字段默认为零，却同时被用于全局卡片和指标历史。历史逻辑把计数下降视为重置。
- **最小修复：** 卡片和趋势始终使用实时或暂停的原始快照，已关闭快照仅用于列表。
- **不修后果：** 切换标签就会显示零流量、零内存，并丢失近期趋势；已关闭连接数量还会被显示为活动连接数。

**触发与证据：** 在已有非零流量和内存数据时切到 Closed。引用实际 timeline 和历史实现的隔离复现确认：全局计数归零，原两点历史被清空为一点。

**回归验证：** 切换 Active/Closed 只改变列表，不改变全局计数和已有趋势。

### 7. [P2] 连接减少后，“上一页”需要反复点击

定位：[runtime/connections/dashboard.rs:516](../../crates/zenclash-ui/src/pages/runtime/connections/dashboard.rs#L516)，下一页计算同样需要核对。

- **作用：** 连接列表分页。
- **问题：** 显示使用夹紧后的页码，按钮却修改未夹紧的旧 `self.page_index`。
- **最小修复：** 从当前显示的 `page.index` 计算上一页和下一页，或在接收新列表时同步夹紧保存的页码。
- **不修后果：** 正常连接变化就会让分页看起来失效。

**触发与证据：** 浏览 501 条连接的第 11 页，再将连接数缩减为 51。界面正确显示第 2 页，但点击上一页把保存索引从 10 改为 9，显示仍被夹紧为第 2 页。该隔离复现已通过。

**回归验证：** 列表缩减后，点击一次上一页就应从当前显示页返回前一页。

### 8. [P2] 选中组测速把显示名当作控制器标识

定位：[proxies/view.rs:615](../../crates/zenclash-ui/src/pages/proxies/view.rs#L615)、[测速调用:695](../../crates/zenclash-ui/src/pages/proxies/view.rs#L695)。

- **作用：** 顶部“测速选中组”按钮。
- **问题：** 保存的是 `group.name`，后续却按 `controller_name()` 查组。测速状态也使用了显示名查找。
- **最小修复：** 保存并传递 `controller_name().to_owned()`，测速状态使用相同标识。
- **不修后果：** 两个名称不同时按钮无动作；如果显示名碰巧对应另一组的控制器标识，则测错组。

**触发与证据：** 控制器 key 为 `wire/key`、显示名为 `Visible label`。当前按钮将后者传给按控制器 key 查找的 `test_group`。已沿 core 构造和界面调用核对，两种名称允许不同；此项为调用链确认。

**回归验证：** 构造显示名与控制器 key 不同的组，验证顶部按钮、测试状态和最终请求均属于原选中组。

### 9. [P2] 托盘操作错误没有用户可见的展示路径

定位：[app/tray/commands.rs:324](../../crates/zenclash-ui/src/app/tray/commands.rs#L324)、[app/view.rs](../../crates/zenclash-ui/src/app/view.rs)。

- **作用：** 保存托盘命令和刷新错误。
- **问题：** `tray_error` 和 `tray_command_error` 在生产代码中被写入，却没有渲染或通知消费者；读取它们的只有测试。部分系统代理回滚失败甚至只写日志。
- **最小修复：** 将已确认的错误接入已有通知或错误展示，并传递回滚失败原因。
- **不修后果：** 节点切换或代理操作失败后，用户只看到操作没有生效，重新打开主窗口也看不到原因。

**触发与证据：** 托盘节点切换返回 API 错误，或代理操作失败。错误保存后调用 `cx.notify()`，但完整渲染路径不读取该状态。已核查全树引用；现有测试只验证字段值，没有验证用户可见反馈。

**回归验证：** 模拟一次托盘命令失败，断言实际通知或错误组件可见，并继续保留过期结果不得覆盖新反馈的校验。

## Should fix

### 10. [P3] 两份缓存持续维护，但生产代码从不读取

定位：[proxies/presentation.rs:12](../../crates/zenclash-ui/src/pages/proxies/presentation.rs#L12)、[GroupOrders.current:103](../../crates/zenclash-ui/src/pages/proxies/presentation.rs#L103)、[presentation/catalog_state.rs](../../crates/zenclash-ui/src/pages/proxies/presentation/catalog_state.rs)。

- **作用：** `NodeSummary` 和 `GroupOrders.current` 缓存节点统计及当前成员。
- **问题：** 刷新、测速和选择都会维护它们，但读取入口 `counts()` 和 `current_node()` 仅供测试使用。
- **最小修复：** 删除这两份缓存、关联维护代码及对应测试；保留实际使用的排序、搜索和失败状态缓存。
- **不修后果：** 节点较多时继续进行无用建表和更新，也增加后续维护成本。

**触发与证据：** 每次目录刷新都会建立缓存，每次延迟或选择变化又会维护缓存；全树引用核查未发现生产读取者。

**回归验证：** 删除后运行代理页面测试及 Clippy，确认真实排序、搜索和选择行为保持正确。

**Lean：** 预计可删除约 180 行，减少 0 个依赖。行数是删除关联维护与测试后的估算值。

## 已运行的验证

使用本机已安装的 Rust 1.99；项目指定的 Rust 1.98.1 未安装。

| 验证 | 结果 |
| --- | --- |
| `cargo +stable test -p zenclash-ui --lib --bins --all-features --locked --offline` | UI 库 531 通过、1 忽略；启动入口 29 通过 |
| `cargo +stable clippy -p zenclash-ui --all-targets --all-features --locked --offline -- -D warnings` | 通过 |
| 模式过期读回隔离复现 | 通过，确认问题 3 |
| 覆写链并发隔离复现 | 通过，确认问题 1 |
| 映射删除配置合成复现 | 通过，确认问题 2 |
| 连接与诊断隔离断言 | 5 项通过，其中包含 2 项原 timeline 测试，支持问题 4、6、7 |

上述“复现通过”表示成功观察到报告中的缺陷，不表示缺陷已修复。隔离复现及逐文件覆盖记录在审计时保存在 `/tmp`，属于临时证据；本文保留了触发条件、调用链和结果，不依赖临时文件长期存在。

## 未验证范围

- 真实系统代理、TUN 和权限写入。
- 系统注销、Windows/Linux 原生窗口及托盘交互。
- 忽略的原生截图测试、视觉验收及资源图片效果。
- 性能压测及真实订阅的大规模负载。
- 完整 core/服务代码逐行审计；本轮只沿 UI 相关调用链核对依赖实现。

平台条件分支已做静态阅读，不能替代各平台实际运行验证。初次审计没有修改产品代码，也没有将未证实的候选列为修复项。

## 修复记录（2026-10-19）

本轮按上述 10 项意见完成实现和对应验证，保留原有未提交修复；没有新增依赖。以下状态只针对本报告列出的行为，不表示所有平台均已实机验收。

| 编号 | 状态 | 实际改动 | 回归证据 |
| --- | --- | --- | --- |
| 1 | 已修复并验证 | `ApplicationCommands::apply` 在共享 data application admission 内读取覆写链，并由独立完成任务持有准入直至配置应用结束。导入、激活、更新和 YAML 编辑共用该路径。 | `profile_activation_waits_for_override_admission_before_reading_the_chain`：另一覆写事务持有准入时激活必须等待，释放后发送最新启用链。 |
| 2 | 已修复并验证 | 为受控层的 `hosts` 与 `dns.nameserver-policy` 提供完整映射替换语义；从原始配置合成及保存受控层都使用该语义。显式 YAML 覆写文件仍递归合并。 | `controlled_dns_mapping_edits_replace_deleted_entries_after_commit_and_reload`：删除单项、清空、提交、重开仓库、保存其他字段后旧键均不恢复，原始 YAML 不变；显式覆写的合并行为保留。 |
| 3 | 已修复并验证 | `synchronize_runtime_mode` 捕获 `CoreObservation`，只在原配置/运行时仍有效时发布模式与日志级别；删除不再使用的应用 client 字段。 | `mode_sync_publishes_fresh_facts_but_discards_changed_configuration_or_runtime`：新鲜结果正常发布；延迟结果遇到配置或运行时变化均不能覆盖当前状态。 |
| 4 | 已修复并验证 | 诊断行、重试按钮与通过计数共用实际结果判定；网络路径至少有公网 IP 或一个延迟探测成功，才显示正常。路径观察也复用同一成功条件。 | `network_health_uses_probe_results_and_offers_retry_after_total_failure`：全部失败、仅 IP 成功、仅延迟成功、全部成功；检查真实行状态与失败重试入口。 |
| 5 | 已修复并验证 | 启动服务交接放入 `adopt_startup`；保留菜单偏好保存任务链、凭据及预览，并应用到正式应用和页面偏好。原生托盘构建仍由启动完成处理负责。 | `startup_adoption_preserves_queued_preferences_and_failed_save_retries`：交接后排队写入继续完成，预览不丢失；失败提示与重试保留，重试写入最新值。 |
| 6 | 已修复并验证 | 全局指标历史在列表投影前观察原始 live/paused 快照；卡片也读取该快照。Closed 快照只用于连接列表；在 Closed 暂停时冻结真实全局快照。 | `closed_list_and_pause_preserve_global_metrics_and_history`：通过真实标签及暂停按钮检查 Closed、暂停、新快照到达、恢复后的全局数据和趋势。 |
| 7 | 已修复并验证 | Previous/Next 从已夹紧的当前显示页 `page.index` 计算目标。 | `previous_page_uses_the_visible_page_after_connections_shrink`：第 11 页的 501 条连接缩为 51 条后，一次点击上一页即返回第 1 页。 |
| 8 | 已修复并验证 | 顶部选中组测速和测试状态使用 `controller_name()`。 | `selected_group_measurement_uses_its_controller_key_even_when_another_key_matches_its_label`：真实按钮点击只请求原选中组的节点，即使另一个组的 key 等于其显示名。 |
| 9 | 已修复并验证 | 主窗口用现有 Alert 展示托盘命令/刷新错误；系统代理的 RolledBack/ReconcileNeeded 失败原因转交相同错误路径，保留原来源校验。新命令获准执行时清除上次命令错误，避免排队重试继续显示旧失败。 | 扩展现有托盘来源回归：检查实际 `tray-feedback` 可见且内容正确；旧查询错误不能覆盖当前查询反馈。`admitting_a_queued_retry_removes_the_previous_tray_error` 验证排队重试清除旧提示。 |
| 10 | 已移除并验证 | 删除 `NodeSummary`、`GroupOrders.current`、关联更新调用及两个镜像测试，保留排序、搜索、失败记录和选择确认逻辑。上述缓存相关文件合计净减少 196 行。 | 全树引用核查无残留；保留的代理页面回归及完整 UI 套件通过。 |

### 本轮验证结果

使用本机 Rust 1.99，所有命令使用锁定依赖和离线模式。

| 验证 | 结果 |
| --- | --- |
| `cargo +stable test -p zenclash-ui --lib --bins --all-features --locked --offline` | UI 库 537 通过、1 忽略；启动入口 29 通过。 |
| `cargo +stable test -p zenclash-core --lib --all-features --locked --offline` | 最新执行被范围外改动阻断：`core_update/recovery.rs` 引用的测试模块文件缺失（E0583），尚未进入测试执行。此前本轮新增 DNS 持久化回归已通过。 |
| `cargo +stable clippy -p zenclash-ui -p zenclash-core --all-targets --all-features --locked --offline -- -D warnings` | 较早一轮通过；最新全目标执行被相同的 core 测试模块缺失阻断。 |
| 分别执行 UI `--all-targets` 与 core `--lib` 的 Clippy（同上依赖及警告选项） | 最新执行均通过。core 这一项不包含其测试目标。 |
| `cargo +stable fmt -p zenclash-ui -- --check`、本轮修改 core 文件的 `rustfmt --check`、限定本轮文件的 `git diff --check` | 通过。全仓格式检查另在本轮范围外的 core 改动上发现格式差异，未覆盖整理这些改动。 |

验证期间工作区另有同时进行的 core 修改，涉及更新恢复、进程资源、权限等文件。本轮保留这些修改，没有补写或删除其尚缺的测试模块。较早一次 core 全量执行为 1117 通过、5 忽略、1 个既有 macOS 授权脚本失败；此后相关权限实现已被范围外修改移除，因此这个历史结果不能代表当前工作区。最新全量 core 测试与全目标 Clippy 需要在该模块补齐后重新执行；本报告不把它们记为通过。

本轮未执行真实系统代理、TUN 或提权写入，也未运行 Windows/Linux 原生窗口和系统注销验收；这些仍属于上述未验证范围。
