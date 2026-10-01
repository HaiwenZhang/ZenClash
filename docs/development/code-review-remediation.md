# 22 项代码审查整改记录

2026-10-01，基准提交为 `bd53166baa630aceff5a0fcd02b130c3984e9cf1`。比较项目为 `debug/rovar` 与 `debug/scope`；当前项目的行为测试和实际依赖源码是验收依据。以下状态对应未提交的当前工作区，原生平台验收与完整应用性能结论分别列明。

## 范围与状态

用户已批准跨边界方案，以及模式冲突拒绝、历史最多 1,000,000 行并淘汰最旧样本的策略。本轮按功能分批实施、复现失败和交叉审查；测试仅证明所覆盖的行为。最终 workspace 为 686 项通过、0 失败、6 项默认忽略；另显式运行的 3 项真实 Mihomo 测试通过。重新构建的 release 完整应用探针通过；原生平台验收边界保留。

| 编号 | 审查问题 | 当前结果与验收边界 |
| --- | --- | --- |
| 1 | 备份失败后重复回滚可删除已恢复文件 | 回滚只移除成功完成的步骤，重试继续处理未恢复的部分；覆盖部分失败和重复回滚。 |
| 2 | 内核升级未统一经过 `CoreSession` | 升级安装、停止、重启和失败恢复经过会话串行边界；下载后退出会取消安装，退出期间的配置重启不得产生新进程。 |
| 3 | 退出失败路径与内核生命周期 | 当前 HEAD 已包含显式退出失败后保持运行的处理；真实 Mihomo 生命周期测试和完整应用正常退出探针通过，退出后无观测到的内核残留。不计为本批新修复，原生代理权限失败仍需平台实机验收。 |
| 4 | 原生代理事务回滚未完整恢复原状态 | 恢复 HTTP、HTTPS、PAC 全部字段并回读；检测外部修改，失败保留恢复上下文。事务与字段映射行为测试已覆盖，原生授权和写入仍须各平台实机验收。 |
| 5 | 同目录独立 `ProfileStore` 的锁不共享 | 独立实例复用协调范围和索引事务锁；丢失自动更新设置的原回归已修复。 |
| 6 | 备份恢复和其他数据写入没有共同排他范围 | 正常读取和写入与恢复独占范围协调；涵盖子目录、文件别名及恢复期间暂时悬空的符号链接。独占租约保留到磁盘与运行状态回滚结束；后台快照在准入后捕获。 |
| 7 | 覆写保存可重新激活提交时已过时的配置 | `ReapplyCurrent` 在会话串行边界内读取当时已提交的配置，保存回调不重新指定旧配置。 |
| 8 | 配置失败回滚使用旧源文件和新覆写重建 | 直接修改、订阅应用和备份恢复均使用实际缓存的原始运行 payload；不重新读取已变更或删除的源。没有实际缓存时明确报告运行状态未知。 |
| 9 | 保存完成覆盖保存期间新增的草稿 | 保存回执更新基线，保留更新后的编辑内容；UI 草稿行为测试通过。 |
| 10 | 切页后忽略保存完成的业务同步 | 业务缓存和事件同步不依赖页面 token；展示通知仍检查页面有效性；切页后实际表单刷新及业务事件测试通过。 |
| 11 | 模式按钮与 YAML 覆写冲突 | 拒绝与启用的 YAML 模式覆写冲突的操作，中英文提示引导编辑或停用覆写；页面与托盘显示失败。 |
| 12 | 网络探测取消与过时结果发布不完整 | 页面切换、隐藏、失焦取消探测；请求 revision 和内核 generation 双重校验；UI 取消及过时结果测试通过。 |
| 13 | 内核离线阻止本地设置 | 本地偏好保留可用；运行时控件和开机启动失败分别处理；UI 实际点击及偏好磁盘持久化测试通过。 |
| 14 | 日志按条数限量且 render 中等待锁、计算 | 增加序列化字节预算与 WebSocket 限制；快照、筛选和行数据在后台准备；日志 25 个、WebSocket 5 个测试及 UI 后台投影测试通过。release 短探针、180 秒隐藏场景及用户配合的 60 秒持续前台日志负载通过；首次失焦无效样本仍保留，更长时间运行尚未测量。 |
| 15 | 节点在多组内复制且所有组标题参与 render | 目录按 controller key 与 provider 保存唯一节点，组仅保存身份；跨组测速共享更新。组与节点分别按 8/24 分页，排序、筛选和测速标题状态使用缓存。同显示名控件的独立选择与测速、旧快照隔离行为测试通过，两轮审查闭环。 |
| 16 | 流量历史超过 SQLite 单批上限后丢失 | 每批最多 10,000 条，提交成功后推进游标，只重试未提交的后缀；UI 内真实临时 SQLite 大批、部分失败重试、保留期限和失败预算测试通过。 |
| 17 | 历史总量无预算且退出无最终刷新 | SQLite 写入事务按观测时间淘汰最旧行，总量最多 1,000,000；退出任务保留所有权并等待最终写入，取消等待不会丢弃任务，写入失败允许重试。 |
| 18 | App 反向依赖页面中的业务工作流 | 共享 `ProfileService` 位于 UI crate 的服务层，页面、托盘、自动更新和编辑器复用；业务恢复上下文独立于页面展示生命周期。 |
| 19 | 配置持久化重复逻辑与入口职责 | 四处私有 YAML/索引回滚合并；配置服务统一应用和重应用入口，保留类型化失败与恢复结果。 |
| 20 | UI 丢弃恢复信息和运行版本，旧结果覆盖新状态 | Core 发布成对的已提交路径与版本；回执保留实际运行尝试版本，服务拒绝迟到结果覆盖新恢复状态。UI 发布当前已提交路径、保留新草稿与模式 revision；手动重载及添加订阅后的恢复卡实际点击回归通过。 |
| 21 | 控件缺少可访问名称 | 补充侧栏、开关、删除按钮等名称；AX 名称和键盘行为测试通过，原生屏幕阅读器验收待实机。 |
| 22 | Windows 标题栏控件只有鼠标操作 | 使用 Kit Button 提供焦点和键盘操作；共享控件 Tab、Enter 和实际点击测试通过，Windows 原生验收待实机。 |

本轮不新增依赖，不改变持久化格式。原生代理授权、Windows 标题栏与屏幕阅读器验收仍缺少对应实机证据；不能用跨平台映射、GPUI 测试或构建成功替代。

## 已批准的实现方案

以下方案保留现有 crate 和依赖，不改变持久化格式。按功能分批实施，每批先补失败复现，再检查、独立审查和验证；不把所有改动合成一个难以回滚的重构。

- **#5/#6**：相同数据根目录的实例使用同一协调锁；正常写入持有共享租约，备份恢复持有独占租约。索引读改写继续持有对应事务锁。恢复期间后台写入等待，GPUI 主线程不等待这些锁。验收同时打开独立 store、恢复与自动更新竞争，以及失败后释放租约。
- **#2/#7**：升级的下载和校验在后台执行，安装、停止、重启和重应用当前配置经过 `CoreSession` 的串行边界。覆写保存后重应用执行时的当前配置，避免重新激活已经离开的配置。验收升级和配置切换竞争、安装或重启失败、应用退出期间取消。
- **#4**：保留已有原生事务快照，分别写回 HTTP、HTTPS、PAC 的全部状态并回读校验，恢复失败保留可重试状态。正常释放代理的所有权规则保持原语义；此项修复事务失败回滚，不引入新的接管策略。
- **#18/#19/#20**：在现有 UI crate 内建立共享配置服务，供页面、托盘和自动更新调用；保留 `ProfileApplication` 的类型化恢复结果和运行版本，在业务状态发布时校验版本。页面只保留展示状态，业务提交和恢复不随切页丢失。覆盖旧 YAML 保存、旧目录查询与新配置竞争；已完成的页面模式 revision 保护保留行为验收，不能替代跨页运行版本。
- **#15**：Core 目录内每个节点保留一份数据，组保存节点标识，UI 使用同一目录快照；测速结果按节点身份更新，避免不同组显示不同副本。此项会改变 Core/UI 的内存 API，持久化格式不变。验收多组引用同一节点、provider 身份、延迟更新、排序和隐藏过滤。
- **#11**：拒绝与启用的 YAML 模式覆写冲突的模式操作，并提示编辑或停用覆写；这是已批准的冲突行为。
- **#17**：保留现有天数清理，额外限制总量为 1,000,000 行并按时间淘汰最旧记录；持有历史任务的退出流程取消采样后执行最终刷新。这是已批准的保留策略。

## 行为与资源预算

- 日志内存缓冲最多 500 条且最多 2 MiB 序列化条目字节；单条最多 64 KiB。落盘队列最多 512 条且最多 4 MiB 序列化条目字节，预留覆盖排队和写入中条目。超限条目产生受限长度的错误记录，重复错误合并，不更新最近成功接收时间。
- `/logs` WebSocket 单帧和完整消息均最多 128 KiB。流量 WebSocket 沿用原配置。
- 上述字节预算不包含 Arc、字符串容量、队列元数据、展示副本、图形资源等，不能宣称整个 Rust 堆或进程 RSS 有相同上限。
- 日志查询变更延迟 80 ms 后开始后台投影；渲染只使用缓存的展示数据和 100 条分页结果。仅失焦取消展示任务，保留结果；离开日志页或隐藏窗口释放可丢弃结果。复制、导出读取源日志并在后台准备内容。
- 流量分批写入不改变原有保留天数策略或失败时 5,000 条待写队列上限。最终批次和空批次执行清理，已提交条目不再重试。
- 代理组每页最多 8 组，节点每页最多 24 个；分页控件保持在滚动内容外，最后一页可通过键盘和鼠标到达。模式、隐藏设置及目录替换更新索引，缩小目录校正页码，suspend 释放索引。组标题从缓存读取测速状态，不在 render 中扫描全部在途测速。
- 每个目录节点只保留一份 `Arc<ProxyNode>`；组与反向索引共享成员身份。旧目录快照存在时，首次修改需要浅复制节点映射和组元数据，成本与唯一节点数及组数有关；不能宣称严格 O(1)。本地测速更新后，该节点历史保留最近 20 条；未更新节点的控制器历史没有额外截断。目录没有全局节点数或堆字节上限。
- 代理页每次模式输入都推进 `mode_revision`，包括相同值；目录请求没有后续输入时采纳 Controller 实际模式，有后续输入时保留当前模式并重新生成分组。Core 运行版本另用于跨页已提交路径与恢复回执的顺序保护；已提交修改继续同步业务状态。

## 首批验证与证据（历史 checkpoint）

本轮使用本机已安装的 Rust `1.95.0`，未改变清单、依赖或持久化数据结构。`--offline` 依赖本机缓存；本地回环 mock 服务测试需要允许测试进程绑定端口。真实内核测试仍须显式配置 `ZENCLASH_MIHOMO_BINARY`，不能由 mock 替代。

已运行：

```sh
cargo +1.95.0 fmt --all -- --check
cargo +1.95.0 check --workspace --all-targets --all-features --locked --offline
cargo +1.95.0 test -p zenclash-core --lib --all-features --locked --offline
cargo +1.95.0 test -p zenclash-ui --all-features --locked --offline
cargo +1.95.0 test -p zenclash-i18n --all-features --locked --offline
cargo +1.95.0 test --workspace --all-features --locked --offline --no-fail-fast
cargo +1.95.0 clippy --workspace --all-targets --all-features --locked --offline -- -D warnings
```

格式、workspace check 和最终 Clippy 通过。含代理组分页、测速状态缓存及模式 revision 的完整 workspace 测试为 **598 通过、1 失败、6 默认忽略**；`--no-fail-fast` 使其他套件在已知失败后继续执行，不跳过失败测试。其中 Core 库为 381 通过、1 失败、2 默认忽略；UI 为 209 库测试及 5 程序测试通过；i18n 为 3 个测试通过；另 4 个真实内核集成测试默认忽略。唯一失败是 #5 独立实例的数据覆盖回归，自动更新设置实际从 `(true, 15)` 被覆盖为 `(false, 1440)`。不能报告 workspace 全套测试通过；不会通过删除断言或忽略 #5 将其伪装为通过。首批修改由其他 Owner 的 Agent 独立审查，未发现本批新增功能性 bug 或重大漏洞；新增代理组分页首轮审查、测速状态缓存最后一次独立审查均通过。

#20 局部模式修复是独立的新批次，仅修改页面现有状态与回执处理。4 个回归用本机真实延迟 `/configs` 与 `/proxies` HTTP 响应、实际 GPUI 分组呈现验收：旧实现正常模式发现通过，其余新模式、同值重申与 ABA 三个场景失败；最小修复后代理页 34 个测试均通过。workspace check 后的独立审查未发现该批新增功能性 bug 或重大漏洞；最终完整 workspace 测试及 Clippy 已包含这次修复。不能据此证明跨页/Core 统一运行版本已实现。[RED](/private/tmp/zenclash-proxy-mode-red.log)、[GREEN](/private/tmp/zenclash-proxy-mode-green.log)、[最终 workspace 测试](/private/tmp/zenclash-22-workspace-with-mode-revision.log)、[最终 Clippy](/private/tmp/zenclash-22-clippy-with-mode-revision.log)。

首轮 UI 测试的三个断言问题已修正：配置表单快照不含 `mode`，改验证真实修改后的 `mixed-port` 及输入值；侧栏 Enter 测试等待延迟 action 完成；Kit Switch 当前不导出 AX disabled 属性，改以真实点击不产生 mutation 和 Tab 无法聚焦来验收禁用行为。另两项本地监听权限失败经授权重跑后通过。

真实 Mihomo 综合测试显式运行通过，1 个测试、0 失败，运行 11.61 秒。测试使用默认配置的临时副本、独立 home 与回环端口、Mihomo 1.19.30；TUN 关闭，不接管主机系统代理，不读取私人配置。验证真实进程启动、托管重启、停止，`ProfileApplication` 本地和远程配置应用，日志 WebSocket 和持久化。观测到本次 4 个内核 PID，退出后残留为 0。既有完整夹具访问公开 gstatic 测速地址，因此不能称为全离线集成测试。

```sh
ZENCLASH_MIHOMO_BINARY=/absolute/path/to/mihomo \
ZENCLASH_CONFIG=/absolute/path/to/temporary/profile.yaml \
ZENCLASH_INTEGRATION_HOME=/absolute/path/to/temporary/home \
ZENCLASH_INTEGRATION_CONTROLLER=127.0.0.1:available_port \
ZENCLASH_INTEGRATION_GEODATA_DIR=/absolute/path/to/geodata \
  cargo +1.95.0 test -p zenclash-core --test real_mihomo \
  --all-features --locked --offline -- --ignored --test-threads=1
```

`available_port` 需替换为未占用端口；夹具还使用固定回环端口 17890、17891，执行前检查 TCP/UDP 可绑定。geodata 至少提供本地 `geoip.metadb`。

另显式运行 `real_automatic` 的两个真实 Mihomo 自动生命周期测试，**2 通过、0 失败、0 忽略**，耗时 2.06 秒。覆盖源配置改变只重启一次、拒绝的源配置保留当前进程、网络暂停不触发崩溃恢复、恢复失败保留停止状态、手动停止和退出阻止自动恢复。原测试使用独立临时目录、动态回环控制器、`mixed-port: 0` 与关闭的 TUN，原生 capture owner 为 `None`；不改变系统代理。观测到本次 5 个 Mihomo PID，退出后匹配本次隔离目录的残留为 0；未预置 GeoData。源码无显式外网请求，未进行网络抓包，不能据此宣称所有进程均没有网络访问。证据：[测试日志](/private/tmp/zenclash-22-real-automatic.log)、[进程记录](/private/tmp/zenclash-22-auto-e1tl44gb/process-evidence.json)。

```sh
ZENCLASH_MIHOMO_BINARY=/absolute/path/to/mihomo \
  cargo +1.95.0 test -p zenclash-core --test real_automatic \
  --all-features --locked --offline -- --ignored --test-threads=1
```

完整应用性能使用 [隔离 release 探针](full-app-responsiveness-probe.md)。基准为 macOS 26.6.2 / Apple Silicon，Mihomo 1.19.30，500 节点、20 组、10,001 条规则，1280 × 820 窗口，隐藏观察 3 秒。短场景不证明三分钟闲置、持续高频日志或 Windows 可用性。

```sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/diagnostics/full_app_probe.py \
  --run --baseline --idle-seconds 3 --cargo-toolchain 1.95.0
PYTHONDONTWRITEBYTECODE=1 python3 scripts/diagnostics/full_app_probe.py \
  --run --idle-seconds 3 --cargo-toolchain 1.95.0
```

初次工作区样本的导航耗时和 UI RSS 高于初次 HEAD 样本，因此追加配对复测。复测复用两个已保存的独立 release 应用包，按 HEAD、工作区、工作区、HEAD 顺序运行；不重建或替换二进制，沿用各自的隔离数据根。临时 harness 复用生产探针的运行、采样和退出检查逻辑，只跳过构建与二进制复制，并允许应用包目录已经存在。重用数据根属于热夹具，不能把复测与初次样本当成完全相同的冷启动条件。

每次观察约 30 秒，导航 20 次、定时回调间隔 400 次；百分位取排序后向上取整的秩，20 个导航样本的 p95 是第二大的样本。导航仅计同步调用，包含 GPUI 更新、通知处理及旧页展示数据清理，不包含新页面绘制或后台加载完成；十种页面各两次，不能据整体 p95 归因某个页面。回调间隔包含正常等待的 50 ms，不是帧耗时或输入呈现延迟。RSS 为 macOS 进程统计，UI 和内核分别报告，不是 Rust 堆或图形资源分项；内核无法唯一识别的样本不按零统计。下表 RSS 为最大值，括号内为有效样本数。

| 样本 | 导航 p50 / p95 / max（µs） | 回调间隔 p50 / p95 / max（µs） | UI RSS max（KiB） | 内核 RSS max（KiB） |
| --- | --- | --- | --- | --- |
| 初次 HEAD | 40 / 644 / 783 | 52213 / 52637 / 84582 | 170192（51） | 69168（48） |
| 初次工作区 | 80 / 2862 / 3146 | 52578 / 52650 / 109636 | 178848（52） | 69536（48） |
| 配对 1：HEAD | 95 / 1383 / 1531 | 52479 / 52653 / 110932 | 184464（50） | 72096（49） |
| 配对 2：工作区 | 100 / 1372 / 1623 | 52553 / 52648 / 88370 | 176432（50） | 74128（49） |
| 配对 3：工作区 | 84 / 1623 / 2329 | 52546 / 52649 / 118950 | 184160（50） | 74448（49） |
| 配对 4：HEAD | 95 / 1489 / 1521 | 52561 / 52651 / 115660 | 182176（50） | 67856（49） |

六次均完成真实页面导航、隐藏观察和正常退出，退出码 0，观测到的子进程残留为 0。隐藏的 3 秒内，初次工作区渲染 2 次，其他样本均为 1 次。HEAD 自身也出现明显波动，配对样本没有给出稳定的整体内存或时延改善证据；同样不能据初次样本宣称确定的性能回退。可报告的是任务、字节预算与可见展示范围的行为已验证，当前短场景仍可完成正常工作流。

本机原始结果保存在临时目录，后续清理可能删除：

- HEAD：[初次结果](/private/var/folders/52/lbpjdmsn7cz33yvvn3mv33bw0000gn/T/zenclash-full-app-4tsyw34h/result.initial.json)、[配对 1](/private/var/folders/52/lbpjdmsn7cz33yvvn3mv33bw0000gn/T/zenclash-full-app-4tsyw34h/result.repeat-1.json)、[配对 4](/private/var/folders/52/lbpjdmsn7cz33yvvn3mv33bw0000gn/T/zenclash-full-app-4tsyw34h/result.repeat-4.json)。
- 工作区：[初次结果](/private/var/folders/52/lbpjdmsn7cz33yvvn3mv33bw0000gn/T/zenclash-full-app-mjuwkke5/result.initial.json)、[配对 2](/private/var/folders/52/lbpjdmsn7cz33yvvn3mv33bw0000gn/T/zenclash-full-app-mjuwkke5/result.repeat-2.json)、[配对 3](/private/var/folders/52/lbpjdmsn7cz33yvvn3mv33bw0000gn/T/zenclash-full-app-mjuwkke5/result.repeat-3.json)。
- [临时复测 harness](/private/tmp/zenclash-22-repeat-probes.py) 与 [复测汇总日志](/private/tmp/zenclash-22-repeat-probes.log)。项目探针源码未修改。

工作区另补充 **180 秒隐藏窗口**的 release 场景，数据规模、平台、内核、窗口尺寸与短场景一致。应用共观察 206.957 秒，完成 20 次真实导航和正常退出；隐藏 180 秒内 RuntimePage 渲染 1 次，退出码 0，观测到的子进程残留为 0。整体导航 p50/p95/max 为 73/1730/2742 µs（20 次）；定时回调间隔为 52038/52663/108456 µs（400 次）。UI RSS p50/p95/max 为 180480/182560/183760 KiB（363 个有效样本），内核为 72160/72160/72176 KiB（359 个）。这些是全场景样本，不是隐藏段单独统计；没有相同长度的 HEAD 对照，不作为性能提升证据，也不代表持续高频日志或长期使用验证。[原始结果](/private/var/folders/52/lbpjdmsn7cz33yvvn3mv33bw0000gn/T/zenclash-full-app-xada_7ns/result.json)。

```sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/diagnostics/full_app_probe.py \
  --run --idle-seconds 180 --cargo-toolchain 1.95.0
```

日志页另运行一次临时 **60 秒、目标 200 请求/秒、16 个有界 worker、10 秒冷却**的 release 夹具，使用同一规模合成目录。真实 Mihomo 将本机请求转发到回环 HTTP origin，关闭 TUN、DNS 和 GeoData 自动更新；临时源码副本关闭应用更新检查，清理代理环境变量，不接管主机代理。诊断只在 UI 读取准备好的缓存长度、容量和 revision，Core 字节计数在 Tokio 后台读取；仓库生产源码未因该夹具改变。

实际完成 11,939 次 HTTP 请求，0 请求错误，约 198.98 次/秒；这些是 HTTP 请求数，不能直接当作已接收日志条数。Core monitor revision 增加 11,939，但 revision 也包含连接状态变化。观测最大 Core 缓冲为 500 条、102,500 序列化字节，UI 缓存为 500 条/行、500 匹配项；Vec 容量为 500/500/512。落盘启用，按每秒采样的队列预留字节为 0，仅说明采样时已排空，不能证明未发生瞬时排队或通过磁盘拥塞压力验收。

**首次持续前台测量无效，不能报告该样本通过。** 66 个回调批次中只有 15 个全程 active/visible/Logs；窗口多次失焦后 `live_updates` 停止，缓存 revision 随前台恢复而继续更新。夹具保留 `valid_stress=false` 和失败退出状态，原应用正常退出为 0，观测到的子进程残留为 0。60 秒阶段记录 1,140 个回调间隔，p50/p95/max 为 51787/52718/93122 µs，但这些混合前台与失焦阶段，不支持持续前台性能结论。期间 UI RSS 从 169856 到 142368 KiB，最大 172096 KiB；内核从 68624 到 49120 KiB，最大 69424 KiB。它们是进程统计，不能当作堆释放或图形资源释放的证据。

用户明确回复“可以，保持前台 60 秒”后，复用同一独立 release 应用包补测；未重建或替换二进制，保留上面的失焦结果。原样有效性断言通过：**64/64 回调批次均为 active/visible/Logs，无失焦回调，55 个不同的压力阶段缓存 revision，观测容量和正常退出检查通过**。实际 59.989 秒提交并完成 11,976 请求，0 请求错误，约 199.64 次/秒；Core revision 增加 11,976。Core 缓冲最大仍为 500 条、102,500 序列化字节；UI 条目/行/匹配最大为 500/500/500，Vec 容量为 500/500/512；启用的落盘队列采样预留为 0。

有效样本的压力阶段定时回调间隔 p50/p95/max 为 **51922/82978/98667 µs（1,100 次）**，冷却阶段为 52557/52674/94324 µs（180 次）。UI 压力段 RSS 起始/结束/最大为 180368/178080/182560 KiB，内核为 73616/74528/74528 KiB；冷却结束为 UI 178032 KiB、内核 74544 KiB。回调仍包含 50 ms 的正常等待，且涉及真实 GPUI 状态读取；p95 82.978 ms 不能当作帧耗时，也不能证明原生输入呈现延迟。该样本证明本次负载下页面仍持续提交展示、缓存保持预算并正常退出，**没有相同负载的 HEAD 对照，不报告性能提升**。两个压力样本均采用 #20 局部模式修复前复制的源码，日志生产代码与本轮最终日志实现相同。

本机证据：[临时日志压力 runner](/private/tmp/zenclash-log-stress-prep/log_stress_probe.py)、[源注入片段](/private/tmp/zenclash-log-stress-prep/stress_stage.rs)、[首次失焦结果](/private/tmp/zenclash-log-stress-5m5a947_/result.focus-interrupted.json)、[原 runner 日志](/private/tmp/zenclash-22-log-stress-runner.log)、[复用二进制 runner](/private/tmp/zenclash-22-focused-log-stress.py)、[有效前台结果](/private/tmp/zenclash-log-stress-5m5a947_/result.focused.json)。该夹具只覆盖普通 TCP 日志，不能替代超大条目、磁盘拥塞、Rust 堆或图形资源归因、原生输入呈现延迟的验证。

后续实机验收：Windows 标题栏用 Tab/Shift-Tab 移动焦点，Enter/Space 操作最小化、最大化/还原和关闭；明暗主题检查焦点、图标和点击目标。屏幕阅读器确认侧栏、开关和删除按钮均读出正确名称。没有对应实机证据时保留未验证状态。


## 批准后继续实施的验证记录

以下记录对应后续工作区，不能把此前 release 探针当作这些新改动的性能证明。最终工作区验收单独记录；原生平台实机验收仍保留边界。

- #5 独立 store 丢更新回归已转为 GREEN；#6 的恢复内部授权组合随后复现 5 秒自锁。两轮审查还确认磁盘回滚后过早释放独占范围、导出漏锁、最终文件别名漏锁；修复均保留行为回归。最终全套测试另复现恢复期间目录别名暂时悬空导致提前放行的实际竞态，归一化现在仍解析符号链接目标并保留缺失后缀；最多跟随 40 次防止循环。重复并发验证进一步复现两次 canonicalize 之间目标被移除的窗口，修复复用第一次解析结果，持续目录替换夹具检查独占期间不得放行。
- #4 第 1 轮独立审查额外复现首次回滚覆盖外部活动协议，以及重试覆盖外部修改的禁用协议缓存。修复后第二轮未发现新增功能性问题，最新 Core 全套已包含相关回归；纯 CF 映射或 planner 测试不能代替 macOS 授权或 Windows/Linux 原生写入验收。
- #2/#7/#8/#11/#20 会话行为覆盖下载期间退出、串行重应用当前订阅、直接修改后的未知运行状态版本推进、精确缓存回滚及退出期间阻止接受和恢复重启。第二轮独立审查未发现新增功能性问题。升级测试使用本地 HTTP 和自有进程协议夹具，不是官方 Mihomo 下载升级验收。
- #17 真实 1,000,000 行 SQLite 夹具复现总量超限；取消退出等待后，真实数据库仍为零条而预期应保存一条 7 bytes 样本。修复引入明确的采样/最终写入任务所有者，最终写入等待取消时仍保留结果；正常退出失败允许重试，原生事件循环返回后即使落盘失败也等待内核停止，再阻止重启。
- #18/#19 共享订阅服务已接入页面、托盘、自动更新和 YAML 编辑器，保留完整失败及未确认运行状态的恢复上下文。恢复入口明确使用订阅当前保存内容，不声称回到精确历史源版本。第二轮审查发现 UI 缓存未跟随添加订阅失败和手动重载成功更新；两项实际点击回归在旧路径失败，共同完成入口同步恢复上下文后通过。
- #20 真实 HTTP 已接受 A 保存后再接受 B，迟到 A 回执仍把 GPUI 路径改回 A 的失败已复现并修复。业务回执保留 generation，查询和发布校验版本；另补成对的已提交路径快照，防止“B 后又切换模式”造成唯一 B 回调被拒绝后 UI 路径仍停留 A。服务按实际运行回执版本拒绝迟到成功清除新未知状态，也拒绝迟到未知覆盖新成功状态。
- #15 两个跨组节点副本回归在旧目录失败，唯一节点模型后通过。第一轮独立审查发现同显示名控件标识重复；实际 Tab/Enter 选择与点击测速回归复现未启动独立测量。标识改为动作、组、controller key、provider 后，本机 HTTP 协议夹具中两个 provider 的独立请求和 42/77 延迟断言通过。第二轮独立审查未发现新增功能性问题；该测试不替代真实 Mihomo provider 接口验收。

本机 RED 证据：[全局行数预算](/private/tmp/zenclash-22-history-budget-red.log)、[恢复授权组合](/private/tmp/zenclash-22-restore-authorized-red.log)、[首次代理回滚](/private/tmp/zenclash-22-native-initial-red.log)、[代理重试缓存](/private/tmp/zenclash-22-native-inactive-red.log)、[旧 YAML 回执](/private/tmp/zenclash-22-editor-version-red.log)、[退出等待取消](/private/tmp/zenclash-22-history-cancel-red.log)、[服务发布顺序](/private/tmp/zenclash-22-service-order-red.log)、[原始缓存与退出取消](/private/tmp/zenclash-22-direct-exact-cancel-red.log)、[备份响应丢失](/private/tmp/zenclash-22-backup-lost-response-red.log)、[悬空目录别名](/private/tmp/zenclash-22-dangling-alias-red.log)、[重复路径解析竞态](/private/tmp/zenclash-22-alias-canonical-race-red.log)、[恢复卡](/private/tmp/zenclash-22-recovery-card-red.log)、[同显示名测速](/private/tmp/zenclash-22-identical-labels-red.log)。编译失败、测试夹具缺失协议响应及沙箱监听限制不计为功能复现。

## 最终工作区验证（2026-10-01）

冻结源码使用 Rust 1.95.0、当前锁定依赖和所有 feature；未新增依赖或改变持久化格式。格式、全目标 workspace check、完整 workspace 测试、严格 Clippy 和 diff whitespace 检查通过。最终全套为 **686 通过、0 失败、6 默认忽略**：Core 450、UI 库 228、UI 程序 5、i18n 3。忽略项为两个官方升级下载测试、两个真实自动生命周期测试、一个真实 Mihomo 综合测试和一个实验 meow-rs 综合测试。

已运行 cargo +1.95.0 的 fmt --all -- --check、check --workspace --all-targets --all-features --locked --offline、test --workspace --all-features --locked --offline --no-fail-fast，以及 clippy --workspace --all-targets --all-features --locked --offline -- -D warnings。

独立 Agent 按功能交叉审查；目录恢复、原生代理快照、共享配置服务、会话状态、历史退出任务和唯一节点模型均完成各批最多两轮的审查与修复。测试中后续发现的目录解析竞态保留独立 RED 和持续替换回归；修后原备份四组合连续 12 次通过，生产协调器独立夹具三次各 10,000 次目录替换均未提前准入。UI 测试夹具补齐模式 PATCH 后的 GET 回读、配置请求的 force=true 和关闭窗口后的任务清理；业务及取消断言保持。

真实 Mihomo 1.19.30 综合测试 **1 通过、0 失败，11.61 秒**；两个自动生命周期测试 **2 通过、0 失败，2.04 秒**。使用独立临时 home、回环端口和关闭的 TUN，不接管主机代理。分别观测到 3 和 5 个自有内核 PID，执行后本次隔离目录匹配的残留均为 0；采样不保证记录每个短寿命进程。综合测试访问公开 gstatic 测速地址，不能称为全离线；自动测试未抓包，不能宣称所有进程完全没有外网访问。两个官方升级下载测试及真实 meow-rs 未执行。

证据：[最终 workspace](/private/tmp/zenclash-22-approved-workspace-final.log)、[最终 check](/private/tmp/zenclash-22-final-check.log)、[最终 Clippy](/private/tmp/zenclash-22-approved-clippy-final.log)、[目录恢复全套](/private/tmp/zenclash-22-core-alias-final-green.log)、[重复备份验收](/private/tmp/zenclash-22-alias-backup-repeat-green.log)、[真实 Mihomo](/private/tmp/zenclash-22-approved-real-mihomo.log)、[真实自动生命周期](/private/tmp/zenclash-22-approved-real-automatic.log)、[综合进程记录](/private/tmp/zenclash-22-real-9ay3skol/process-evidence.json)、[自动进程记录](/private/tmp/zenclash-22-auto-88qgc9g2/process-evidence.json)。

当前冻结源码另重新构建并运行 release 完整应用探针，**通过**。平台为 macOS 26.6.2 / arm64，Mihomo 1.19.30，500 节点、20 组、10,001 条规则，1280 × 820 窗口；构建 3 分 11 秒，应用观察 29.372 秒。实际完成 20 次导航，隐藏 3 秒内 RuntimePage 渲染 2 次，正常退出码 0，本次观测到的自有子进程残留为 0。

| 指标 | p50 | p95 | max | 有效样本 |
| --- | --- | --- | --- | --- |
| 导航调用（µs） | 76 | 1513 | 2637 | 20 |
| 50 ms 定时回调间隔（µs） | 52010 | 52646 | 89225 | 400 |
| UI RSS（KiB） | 169504 | 173872 | 173984 | 53 |
| 内核 RSS（KiB） | 66608 | 71152 | 71152 | 49 |

百分位仍取排序后向上取整的秩。导航只计同步调用，不含页面绘制或后台加载完成；回调包含正常 50 ms 等待，不能当作帧耗时或输入呈现延迟。RSS 为全场景进程统计，包括启动和隐藏，不能当作 Rust 堆或图形资源归因；不能唯一识别内核时不按零统计。本批只有一个新 release 样本，没有当前同条件 HEAD 配对对照，**不报告整体性能提升**。

证据：[新的完整应用结果](/private/var/folders/52/lbpjdmsn7cz33yvvn3mv33bw0000gn/T/zenclash-full-app-ivi7nwl7/result.json)、[探针日志](/private/tmp/zenclash-22-approved-release-probe.log)。前面的 ABBA、180 秒隐藏和 60 秒持续前台日志样本属于历史 checkpoint；日志生产实现保留原验收，但这些旧样本不代表本批所有新代码的长期运行表现。Windows 原生标题栏、屏幕阅读器及各平台原生代理授权/写入仍未进行实机验收。
