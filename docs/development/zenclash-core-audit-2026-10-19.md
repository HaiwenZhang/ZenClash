# zenclash-core 审计报告

审计日期：2026-10-19。审计方法：`ponytail-audit`。

审计对象为 `crates/zenclash-core` 的当前工作区，包含审计时已有的未提交连接管理改动。以下编号保留原始审计发现；审计阶段未修改源码，后续修复状态见下表。原始行号对应审计时版本，修复后的代码与测试以处理状态中的链接为准。

## 修复进度（2026-10-19）

| 编号 | 状态 | 已落实的改动与回归 |
| --- | --- | --- |
| 1 | 已修复，宿主回归通过 | 控制器客户端禁用环境代理；[独立子进程回归](../../crates/zenclash-core/src/client/tests.rs)验证认证头只到达指定控制器。 |
| 2 | 已修复，宿主回归通过 | [持久化恢复日志](../../crates/zenclash-core/src/core_update/recovery.rs)接入启动选择；[强退测试](../../crates/zenclash-core/src/core_update/recovery/tests.rs)覆盖替换、回滚、提交与符号链接。提交记录失败、存活更新锁和损坏记录拒绝也有回归覆盖。 |
| 3 | 已修复，Unix 回归通过 | [数据库文件保护](../../crates/zenclash-core/src/traffic_history/storage/private_io.rs)在打开 SQLite 前执行；[权限回归](../../crates/zenclash-core/src/traffic_history/storage/permissions_tests.rs)覆盖新建、已有数据和存活的 WAL/SHM。Windows ACL 代码及测试已加入，尚未实机执行。 |
| 4 | 已修复，宿主回归通过 | [预检](../../crates/zenclash-core/src/core_validation.rs)只识别明确的致命级别标记；覆盖含 `fatal` 路径的成功输出及退出码 0 的真实致命日志。 |
| 5 | 已修复，宿主回归通过 | [Release 校验](../../crates/zenclash-core/src/app_update.rs)仅对 owner/repo 忽略大小写，保留主机、路由、查询参数等限制。 |
| 6 | 已修复，宿主回归通过 | [连接重建](../../crates/zenclash-core/src/proxy_operations/selection.rs)同时匹配被切换组和旧成员；回归保留其他组的共享节点连接。 |
| 7 | 已修复，宿主回归通过 | [HTTP JSON 读取](../../crates/zenclash-core/src/client/request.rs)统一限制为 4 MiB，使用已有有界读取器；覆盖精确上限、声明超限和持续分块超限，并保留结果不确定语义。 |
| 8 | 已修复，宿主回归通过 | [原始日志](../../crates/zenclash-core/src/managed_core_process/launch_logs.rs)限制单行 64 KiB、历史总量 2 MiB，保留原有 1000 行限制；覆盖无换行、超长行后继续读取及 UTF-8 边界。 |
| 9 | 已移除，工作区编译通过 | 删除 `TunPermissionManager`、Unix setuid 脚本及其余旧平台实现，保留权限状态/错误类型；仓库已无旧接口的 Rust 调用。 |

上述状态只描述本报告列出的修复，不代表补做了全部 90,578 行人工审计或完成三平台原生验收。最终验证结果见文末。

### Local 内核更新恢复文件及兼容性

新恢复记录只协调普通 Local 可执行文件替换，不协调受保护 Service 副本，不改变服务安装 journal、用户配置或备份格式。

- 同目录 `.目标文件名.zenclash-update.json` 记录格式版本、阶段及本次暂存/备份文件名；写入后同步记录文件，Unix 额外同步父目录。
- 同目录 `.目标文件名.zenclash-update.lock` 复用现有操作系统文件锁，更新期间持有；空锁文件保留复用，避免删除锁名后并发操作锁住不同文件。
- 未提交的 `Preserving`、`Installing`、`Restoring` 状态恢复旧内核；已提交的 `Committed` 状态保留新内核并清理备份。
- 候选内核停止失败时记录 `Manual`，后续启动保留现场并返回备份位置，不自动替换可能仍在运行的文件。
- 自定义、托管、工作区及 PATH 启动候选在检查可执行文件前处理恢复记录；服务健康探测仍保持只读。
- 校验记录大小、格式版本及受限同目录文件名；损坏、越界或缺少必需原件时拒绝继续，不猜测恢复来源。

历史数据库现在只接受普通文件，拒绝最终路径为符号链接或重解析点的文件；使用该类路径的调用者需改用真实数据库路径。Windows 权限修复只处理当前用户所有的文件，不自动接管其他账户的文件。

没有更新记录的旧安装无需数据迁移。新版本之前已经遗留的孤立备份不会被自动采用；旧版本也不识别新恢复记录，降级前应先由新版本完成恢复或正常结束更新。移除 `TunPermissionManager` 是公开 Rust API 的删除，仓库内调用者已核对，外部调用者需迁移至现有服务授权和 `CoreSession` 权限观察入口。

## 范围与结论

`zenclash-core` 负责 Mihomo 生命周期、订阅与配置事务、后台服务、系统代理、更新、备份及流量历史。此次按单个桌面实例、数百至数千连接、后台任务与用户操作并发的负载进行审计。

目录共有 **292 个 Rust 文件、90,578 行**。本次完成了全目录编译与静态检查，以及关键调用链的人工逐行深审；**尚未完成全部文件的人工逐行复核**。上述文件数和行数是目录规模，不是人工逐行审阅完成量。

深审主要覆盖订阅下载与仓库写入、配置合成与应用事务、备份导入导出及恢复日志、内核更新、控制器 HTTP 与 IPC 请求、代理选择、流量历史、日志收集，以及部分服务交接和系统代理事务。沿调用关系核对了相关 UI 调用点、测试和开发文档。

确认 **8 个具体问题、1 项精简建议**。优先处理控制器令牌转发和内核更新的崩溃恢复。

- **P1：** 高优先级，涉及认证信息意外转发或异常退出后无法恢复内核。
- **P2：** 需要修复的功能、隐私或资源限制问题。
- **P3：** 较低优先级的维护与代码精简建议。

## Must fix

### 1. [P1] HTTP 控制请求会把认证令牌发给环境代理

定位：[client.rs:227](../../crates/zenclash-core/src/client.rs#L227)。

- **作用：** 创建访问 Mihomo 控制器的 HTTP 客户端。
- **问题：** 没有禁用环境代理。发往本机回环地址的控制请求也可能被转发到代理服务器。
- **最小修复：** 为控制器客户端添加 `.no_proxy()`，补充带 `HTTP_PROXY` 的子进程回归测试。
- **不修后果：** 本地控制失败，控制器认证信息可能被意外转发给外部代理。此问题影响 HTTP 控制路径。

**触发与证据：** 在隔离子进程中设置 `HTTP_PROXY` 并清空 `NO_PROXY`，使用占位令牌请求 `http://127.0.0.1:9/version`。本机测试代理收到了绝对形式的控制请求和 `Authorization: Bearer audit-placeholder`，客户端还接受了测试代理返回的版本信息。

**回归验证：** 配置可观察的环境代理，访问本机测试控制器；控制请求和认证头必须直达指定控制器，环境代理不得收到请求。

### 2. [P1] 内核更新存在目标可执行文件消失的崩溃窗口

定位：[core_update/transaction.rs:48](../../crates/zenclash-core/src/core_update/transaction.rs#L48)、[启动路径解析](../../crates/zenclash-core/src/managed_core_process/launch_config.rs#L230)。

- **作用：** 把旧内核移到备份，再把候选内核移入正式路径。
- **问题：** 两次重命名之间异常退出时，正式路径不存在，依赖 `Drop` 的回滚不会执行；启动路径解析没有找回该更新备份。
- **最小修复：** 持久化更新恢复信息，并在启动解析内核路径前恢复未完成更新；增加两次重命名之间的强退测试。
- **不修后果：** 更新期间一次异常退出即可使自定义内核无法再次启动，需要手动寻找备份。

**触发与证据：** 在临时源码副本中，仅于旧内核移到备份后、候选内核移入前加入进程退出故障点。子进程以退出码 86 结束，正式目标缺失，旧备份仍保留；调用现有自定义内核启动路径解析后返回错误。

**回归验证：** 在更新的各个文件替换阶段强制结束子进程，重新启动后应恢复可执行的已确认版本。测试需要覆盖自定义内核路径。

### 3. [P2] 流量历史数据库没有私有文件权限保护

定位：[traffic_history/storage.rs:200](../../crates/zenclash-core/src/traffic_history/storage.rs#L200)。

- **作用：** 保存访问主机、进程、设备地址和流量数据。
- **问题：** 直接通过 SQLite 创建文件，没有显式限制其他账户读取。
- **最小修复：** 在 SQLite 打开前以私有权限创建数据库，并处理已有数据库及 WAL、SHM 文件的权限。
- **不修后果：** 父目录可遍历时，浏览行为和应用使用记录可能暴露给其他本机用户。

**触发与证据：** 使用独立临时目录和 `umask 022`，通过现有 `TrafficHistoryStore::insert_and_cleanup` 写入记录，数据库权限实测为 `0644`。

**回归验证：** 在独立子进程中设置宽松 umask，分别验证新建与已有数据库，以及相关辅助文件的权限。不能依赖用户主目录恰好禁止其他账户遍历。

### 4. [P2] 合法路径包含 fatal 就会被判为配置预检失败

定位：[core_validation.rs:363](../../crates/zenclash-core/src/core_validation.rs#L363)。

- **作用：** 检查内核预检输出，识别失败。
- **问题：** 对整个输出执行 `contains("fatal")`，没有区分致命日志级别与普通路径内容。
- **最小修复：** 匹配明确的致命日志级别标记，避免把路径或普通消息内容当作级别。
- **不修后果：** 某些合法安装目录或配置名称会导致启动、应用配置及更新预检失败。

**触发与证据：** 将验证目录命名为包含 `fatal` 的路径，测试内核按 Mihomo 格式输出 `configuration file <path> test is successful` 并以 0 退出。现有 `CoreConfigValidator` 仍返回 `Rejected`，错误中同时保留了成功信息和退出码 0。[Mihomo 成功输出包含配置路径](https://github.com/MetaCubeX/mihomo/blob/Meta/main.go)。

**回归验证：** 包含 `fatal` 的合法目录及普通消息不得被误判；真正的致命日志仍须被识别。

### 5. [P2] 应用更新检查会拒绝官方 Release 地址

定位：[app_update.rs:225](../../crates/zenclash-core/src/app_update.rs#L225)。

- **作用：** 验证更新链接属于官方仓库。
- **问题：** 路径要求小写 `zenclash`，而实际官方 Release 链接使用 `ZenClash`；路径比较区分大小写。
- **最小修复：** 对 owner、repo 两段忽略大小写，保留其余地址检查；测试使用实际官方地址。
- **不修后果：** 正常更新响应会被显示为不可信或无效，甚至无法报告“已是最新版本”。

**触发与证据：** 将[官方 v0.1.2 Release 地址](https://github.com/HaiwenZhang/ZenClash/releases/tag/v0.1.2)交给现有验证函数，结果为拒绝。该验证发生在新旧版本比较结果返回之前。隔离复现使用原验证模块的临时副本，仅附加调用入口，未改变验证逻辑。

**验证边界：** 已核对公开 Release 链接并复现地址验证错误；本机直接请求 GitHub API 的端到端检查被 DNS 解析失败阻断。

**回归验证：** 官方仓库路径的大小写变体应通过；其他 owner、repo、主机或不符合要求的路径仍应拒绝。

### 6. [P2] 重建受影响连接会误断其他代理组的连接

定位：[proxy_operations/selection.rs:314](../../crates/zenclash-core/src/proxy_operations/selection.rs#L314)。

- **作用：** 切换节点后关闭需要重建的连接。
- **问题：** 只检查连接链是否包含旧节点，没有检查所属代理组。
- **最小修复：** 同时核对被切换组和旧成员，增加两个组共用节点的回归测试。
- **不修后果：** 调用 `RebuildAffected` 时，其他组中的下载、会话也会被中断。

**触发与证据：** A、B 共用 `Shared` 节点。调用现有 `ProxyOperations::apply_selection`，把 A 切换到 `New` 并选择 `RebuildAffected`；测试控制器提供一条链为 `["Shared", "B"]` 的连接，随后收到了删除该连接的请求。

**影响范围：** 当前 UI 常用选择路径使用 `KeepExisting`；上述问题存在于可调用的 `RebuildAffected` 行为中。

**回归验证：** 切换 A 时，仅属于 B 的共享节点连接必须保留；确实通过 A 旧路径建立的连接应按策略处理。

### 7. [P2] HTTP 控制器成功响应没有字节上限

定位：[client/request.rs:196](../../crates/zenclash-core/src/client/request.rs#L196)。

- **作用：** 读取并解析控制器返回的 JSON。
- **问题：** `response.bytes().await` 完整读取响应，没有限制实际字节数。请求超时不能限制累计内存。
- **最小修复：** 复用已有有界响应读取器，在读取过程中限制实际字节数，并覆盖分块响应。
- **不修后果：** 异常控制器或大规模响应可造成明显内存增长，严重时耗尽内存。

**触发与证据：** 本机隔离控制器在 `/version` 返回一个 8 MiB 的版本字符串，现有客户端完整读取并解析成功，得到长度 `8388608`。

**回归验证：** 普通长度响应应通过；声明超大长度及未声明长度的持续分块响应都应在预算耗尽时立即停止读取。限制应与各控制器接口的合法数据规模匹配。

### 8. [P2] 原始子进程日志只有行数限制，没有字节限制

定位：[managed_core_process.rs:844](../../crates/zenclash-core/src/managed_core_process.rs#L844)、[launch_logs.rs:31](../../crates/zenclash-core/src/managed_core_process/launch_logs.rs#L31)。

- **作用：** 收集内核 stdout、stderr，保留启动诊断。
- **问题：** `BufReader::lines()` 持续累积到换行，再完整存入历史。历史只限制行数，没有单行或总字节上限。
- **最小修复：** 增加单行和历史总字节上限，超长行持续读取并丢弃多余内容。
- **不修后果：** 内核日志异常时，行数上限仍无法防止 GUI 进程内存耗尽；无换行输出也会持续增长。

**触发与证据：** 使用 `test-support` 的隔离子进程入口启动测试脚本，输出一条 2 MiB 的长行。现有 `recent_logs` 保留的该行长度为 `2097159`，包含附加日志前缀。测试未使用真实内核执行槽位。

**回归验证：** 覆盖超长单行、持续无换行输出及大量普通日志；内存必须受单行和总预算限制，后续合法日志仍可被收集。

## Should fix

### 9. [P3] 删除已脱离生产调用链的旧 setuid 授权实现

原定位：`tun_permissions.rs:119`、`tun_permissions/linux.rs:28`、`tun_permissions/macos.rs:9`。旧入口与平台脚本现已删除；保留的[权限状态与错误类型](../../crates/zenclash-core/src/tun_permissions.rs)仍供当前运行时观察使用。

- **作用：** 通过管理员脚本修改内核所有者并设置 `4755` 权限。
- **问题：** 全仓库检索未发现 `TunPermissionManager` 的生产调用，但公开 API 和平台脚本仍保留；当前 TUN 流程已经走服务授权。
- **最小修复：** 移除旧 manager、授权脚本及专属测试，保留当前仍被使用的权限状态和错误类型。移除公开接口时明确记录兼容性影响。
- **不修后果：** 继续维护一套与当前服务流程分离的高权限代码，公开 API 也仍能进入旧授权方式。

**触发与证据：** 已检索源码、测试、脚本及文档中的 `TunPermissionManager`、`request_grant` 引用。源码中的引用只剩定义、导出及旧实现内部调用；当前 `CoreSession::ensure_tun_permission` 使用服务或现有进程权限检查。旧公开方法仍会进入 `chown`、`chmod 4755` 脚本。

**验证边界：** 此项是代码精简建议，影响限于遗留公开 API。审计未执行管理员授权或修改任何真实内核权限。

## 审计时验证结果

仓库固定工具链为 Rust 1.98.1，本机该工具链不可用，下载被网络解析失败阻断。验证改用本机已有的 **Rust 1.99.0**，依赖使用 `--locked --offline`。

| 检查 | 结果 |
| --- | --- |
| 严格 Clippy，全部目标与全部特性 | 通过 |
| 默认库测试 | 1114 通过、1 失败、5 忽略 |
| 全特性库测试，跳过已确认受 AppleScript 环境影响的一项 | 1114 通过、0 失败、5 忽略、1 跳过 |
| 原生 IPC 集成测试 | `native_service_client` 被内核占用检测阻断，后续集成测试未继续完成 |
| 隔离复现 | 第 1、3、4、6、7、8 项通过运行现有公开接口验证；第 2 项通过源码副本故障注入验证；第 5 项通过原验证逻辑副本验证 |

运行的主要命令：

```sh
cargo +stable clippy -p zenclash-core --all-targets --all-features --locked --offline -- -D warnings
cargo +stable test -p zenclash-core --locked --offline
cargo +stable test -p zenclash-core --all-features --locked --offline -- \
  --skip tun_permissions::platform::tests::authorization_applescript_compiles_before_any_privilege_prompt
```

默认测试的唯一剩余失败为 `tun_permissions::platform::tests::authorization_applescript_compiles_before_any_privilege_prompt`。最小独立 `do shell script "true"` 编译也失败，因此暂不能把该测试失败归因于项目脚本。

全特性测试中的 `native_service_client` 返回：

```text
Error: process mihomo remains after IPC failure; refusing a second core
```

审计没有强行停止现有内核，也没有通过修改真实系统服务或 TUN 配置来绕过该检测。

隔离复现的关键输出：

```text
history_mode=644
loopback_request_reached_proxy=true,bearer_reached_proxy=true
controller_version_via_proxy=audit-proxy
accepted_version_bytes=8388608
selection_A_closed_B=true
successful_validation_rejected=true
retained_stdout_line_bytes=2097159
official_release_url_rejected=true
crash_exit=Some(86),target_missing=true,old_backup_preserved=true
startup_rejects_missing_preferred_binary=true
```

审计时的本机临时证据路径如下；这些文件不属于仓库，后续可能被系统清理：

```text
/tmp/zenclash-core-audit-probes/src/main.rs
/tmp/zenclash-core-audit-probes/src/app_update_probe.rs
/tmp/zenclash-core-audit-probes/src/update_transaction_probe.rs
/tmp/zenclash-core-audit-tests.log
/tmp/zenclash-core-audit-all-features-tests.log
/tmp/zenclash-core-audit-clippy.log
```

## Verdict

已按原优先级落实第 1–9 项。新增独立回归覆盖原缺陷边界，最终结果见修复后验证记录；真实服务与跨平台验收仍按检查范围单独记录。

## Lean

第 9 项已净删除约 **390 行**旧授权实现与测试，减少 **0 项依赖**，保留当前仍被使用的权限状态和错误类型。新增更新恢复与回归代码另计。

## 修复后验证结果

验证工具链：本机 Rust 1.99.0。未修改依赖版本或添加第三方依赖。

| 检查 | 最终结果 |
| --- | --- |
| 默认特性核心库测试 | **1134 通过、0 失败、5 忽略** |
| 全特性核心库测试 | **1134 通过、0 失败、5 忽略**，包含新增提交记录失败回归与 native pending-session 回归 |
| 严格核心 Clippy，全部目标与全部特性 | **通过** |
| 全工作区格式检查 | **通过** |
| 全工作区编译检查，全部目标与全部特性 | **通过**，包括 UI 对公开 API 的编译检查 |
| 修改差异与本文本地链接检查 | **通过** |
| Windows 交叉编译检查 | **未完成**：缺少锁定依赖缓存，联网补齐时 `rsproxy.cn` DNS 解析失败 |
| Windows/Linux 原生权限、真实服务与 TUN 验收 | **未执行** |

最终运行命令：

```sh
cargo +stable test -p zenclash-core --lib --locked --offline
cargo +stable test -p zenclash-core --all-features --lib --locked --offline
cargo +stable clippy -p zenclash-core --all-targets --all-features --locked --offline -- -D warnings
cargo +stable fmt --all -- --check
cargo +stable check --workspace --all-targets --all-features --locked --offline
```

删除旧授权实现后，其 AppleScript 编译测试随之移除，本轮库测试没有跳过该测试来取得通过结果。原生服务进程集成测试没有在本轮重新执行；不能用全特性库测试代替其验收。

没有执行真实服务安装、管理员授权、TUN 变更或停止现有内核。保留工作区已有的其他修改，没有提交或推送代码。

最终本机日志：

```text
/tmp/zenclash-core-fixes-final-default.log
/tmp/zenclash-core-fixes-all-features-library.log
/tmp/zenclash-core-fixes-final-clippy.log
/tmp/zenclash-core-fixes-format.log
/tmp/zenclash-core-fixes-final-workspace-check.log
/tmp/zenclash-core-fixes-windows-check.log
```

## Not checked

- 尚未完成全部 292 个 Rust 文件、90,578 行的人工逐行复核。
- 服务状态机、各平台系统代理适配器及全部测试源码仍有未逐行深审部分。
- Windows、Linux 原生写入与真实提权、TUN 场景未执行。
- 需要真实 Mihomo 或系统权限的忽略测试未执行；原生 IPC 集成测试未全部完成。
- 未使用仓库固定的 Rust 1.98.1 完成验证；本机验证版本为 Rust 1.99.0。
