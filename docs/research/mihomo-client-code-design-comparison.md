# 四款 Mihomo 客户端代码与设计比较：面向 ZenClash 的取舍建议

> 调研日期：2026-08-27（Asia/Shanghai）
> Mihomo 基线：`v1.19.30`（ZenClash 当前固定的正式内核版本）
> ZenClash 基线：[`7415b25f4a3e70af9a50f690dd56bfc403cd808e`](https://github.com/HaiwenZhang/zenclash/commit/7415b25f4a3e70af9a50f690dd56bfc403cd808e)
> 对象：Clash Verge Rev、Clash Party、ClashMi、FlClash，以及 ZenClash 当前实现

## 0. 结论先行

这次比较没有“功能最多者胜出”。一个 Mihomo 客户端是否合格，先看五个门槛：

1. UI 是否区分进程、Controller、流量接管和端到端路径，而不是用一个绿点概括全部状态；
2. 配置或订阅失败时，最后可用配置、运行内核和系统接管能否保持可用；
3. 系统代理、TUN、代理组和连接操作的副作用是否真实、可解释、可恢复；
4. 更新、Controller、脚本覆写和高权限平台操作是否有可验证的安全边界；
5. 关键失败路径是否能通过模块 interface 做行为测试，而不是依赖页面联调或人工回归。

按这个口径，四个客户端分别提供不同参考：

- **Clash Verge Rev** 最适合研究“有状态、有代际、有回滚”的成熟控制系统；它不是简洁表层的模板。
- **Clash Party** 最适合研究候选配置验证、内核操作串行化、WebSocket generation 和完整代理组语义；它也展示了功能面过宽如何侵蚀状态真实性。
- **ClashMi** 最适合研究移动优先的短主路径和可复制网络诊断；其公开核心不完整、全局静态状态和单一 `connected` 不适合作为 ZenClash 架构基础。
- **FlClash** 最适合研究 latest-intent 生命周期、进程 lease、有界 IPC 和键盘行为测试；它同时暴露了宽接口、系统状态失真与高权限边界过大的风险。

ZenClash 不需要追平四者的功能总和。它在 `CoreSession`、`SystemProxySession`、`ControlledConfigStore`、随机 Controller secret 和真实 Mihomo E2E 上已经形成有价值的差异化。下一阶段的正确方向是把这些后端真相准确送到 UI，并补齐 Mihomo 代理组语义和诊断闭环。

建议的明确取舍是：

- **保留**现有 Rust/GPUI 与深模块方向，不改成全局 manager、跨进程大白名单或第二套 Web dashboard。
- **P0 先做**四层运行状态、首页分片降级、状态驱动首次使用、`hidden/fixed/restore-auto`、节点切换不默认断全部连接、主路径键盘可操作。
- **P1 再做**DNS query/flush、structured logs、结构化网络诊断、脱敏支持包、Provider 运维。
- **P2 收束**首页与信息架构、按操作域拆 UI 状态、应用更新提示和高级能力入口。
- **明确不做**为功能对齐而引入 JS 覆写、Sub-Store、插件市场、多内核、全量 YAML 表单或默认静默安装。

## 1. 研究基线、范围与限制

### 1.1 锁定版本

| 对象 | 锁定版本/提交 | 审查口径 |
|---|---|---|
| Mihomo | `v1.19.30` | 官方文档、官方仓库 tag 与 RESTful API；动态文档以 2026-08-27 页面为准 |
| ZenClash | [`7415b25f`](https://github.com/HaiwenZhang/zenclash/commit/7415b25f4a3e70af9a50f690dd56bfc403cd808e) | 当前仓库静态源码、README、测试与 ADR |
| Clash Verge Rev | [`3503a2da`](https://github.com/clash-verge-rev/clash-verge-rev/commit/3503a2da29d68a4398c0b8e9234cffb711e65783) | `2.5.4` 开发源码；不是已发布 `v2.5.4` |
| Clash Party | [`061faeef`](https://github.com/mihomo-party-org/clash-party/commit/061faeefd15849d31062e78cbd4084bad7f0f497) | `smart_core` 默认分支，包版本 `2.0.2`，比 `v2.0.2` 多 21 commits |
| ClashMi | [`917fd460`](https://github.com/KaringX/clashmi/commit/917fd46085d71e8a1caa91f018681337283d5162) | `v1.0.29.1501` / App `1.0.29+1501` |
| FlClash | [`62addf73`](https://github.com/chen08209/FlClash/commit/62addf738a76b1a492e19af2dbabdb6d572b9e72) | Flutter/Go/Rust/Android 平台源码与仓库内测试 |

### 1.2 证据标签

本文用三种标签约束结论：

- **源码事实**：可以从锁定 commit 的精确 permalink 或 Mihomo 官方资料直接复核。
- **产品判断**：基于源码事实，对可靠性、简洁性、可访问性或产品取舍作出的评价。
- **推断**：源码没有直接声明、或公开仓库存在缺口，需要真实 OS/Mihomo 行为测试确认。

竞品仓库只能证明该项目在锁定提交公开了什么，不能证明 Mihomo 要求所有客户端照做，也不能证明没有出现在公开源码中的平台行为一定不存在。

### 1.3 研究限制

- 本次以静态源码审查为主，没有在所有支持平台运行四款 GUI、安装系统服务、开启 TUN 或执行更新。
- ClashMi 的 `libclash_vpn_service`、`board_service`、私有 Dart 目录与 Android AAR 不在公开仓库中，关键平台实现无法完整审查。
- 运行权限、路由、崩溃后的 OS 恢复和安装包签名等结论，只有源码不足时才标为推断或风险，不冒充已复现缺陷。
- UI/UX 建议使用 `ui-ux-pro-max` 本地规则库中实际检索到的四类原则：可操作空状态、带恢复路径的局部错误、完整键盘路径与可见焦点、状态/图表不只依赖颜色。没有把未匹配的“渐进披露”搜索结果写成证据。
- 本文继承并压缩已有 [`mihomo-client-design-research.md`](./mihomo-client-design-research.md) 的 Mihomo 原理，不重复把所有配置字段展开。

## 2. Mihomo 给客户端划定的真实职责

### 2.1 数据面、控制面与接管层不是一回事

**来源事实。** Mihomo 提供入站、TUN、DNS、嗅探、规则路由、代理组、Provider、连接统计与 RESTful 控制面；桌面托盘、订阅存储、系统代理写入、权限引导和应用更新不是内核替客户端完成的职责。[Mihomo README](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/README.md) · [核心数据路径](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/tunnel/tunnel.go)

```text
应用流量 ─┬─ System Proxy ─> HTTP/mixed/SOCKS inbound ─┐
          ├─ TUN/route ───> TUN inbound ──────────────┼─> metadata/fake-IP/sniff
          └─ 显式/透明代理 ────────────────────────────┘
                         -> Direct/Global/Rule
                         -> Selector/URLTest/Fallback/LoadBalance
                         -> outbound + trackers
客户端 -> REST/WebSocket -> Controller（控制与观测）
客户端 -> OS integration -> System Proxy/TUN（接管）
```

**产品判断。** `/version`、`/configs` 或 `/traffic` 成功，只证明 Controller 可达或统计流可读。它不能证明系统代理仍指向本应用、TUN 设备/路由已经生效，更不能证明目标路径可用。

因此首页至少应区分：

| 层 | 问题 | 可观察证据 |
|---|---|---|
| L1 Process | 内核是否仍在运行 | 托管 child、退出码；外部内核仅标记为外部可达 |
| L2 Controller | 控制协议是否兼容且可达 | `/version`、认证与连接错误分类 |
| L3 Capture | 流量是否有进入 Mihomo 的有效入口 | System Proxy actual/ownership；TUN 权限、设备、路由与错误 |
| L4 Path | 指定路径是否真实连通 | 明确标注走 Mihomo 或 DIRECT 的端到端探测 |

### 2.2 客户端必须正确消费的 Mihomo 语义

| 官方能力 | 官方行为 | 客户端设计含义 |
|---|---|---|
| `mihomo -t` | 验证配置后退出 | 生成最终候选配置后验证，不只做 YAML 语法检查 |
| `GET /proxies` | 返回组、`now/all/hidden/fixed` 等 | 组类型、隐藏与固定状态必须进入模型 |
| `PUT /proxies/:name` | 选择成员；自动组可能变为 fixed | 点击自动组节点必须说明“固定”副作用 |
| `DELETE /proxies/:name` | 非 Selector 自动组恢复自动 | URLTest/Fallback 提供明确“恢复自动” |
| `GET /group/:name/delay` | 组测速且会清自动组 fixed | 不能把它冒充无副作用的批量测速 |
| `GET/WS /connections` | 返回 metadata、chains、rule 等 | 连接详情可解释“为什么走这里” |
| `GET/WS /traffic` | 返回统计流 | 不是接管健康探针 |
| `GET /logs?format=structured` | 返回 core 时间、level、message、fields | structured-first，普通文本只是兼容回退 |
| `GET /dns/query` | 查询 DNS message | DNS 页应先服务诊断，不只编辑配置 |
| `POST /cache/dns/flush` | 清 DNS cache | 属于恢复动作，需要反馈与边界说明 |
| `POST /cache/fakeip/flush` | 清 fake-IP cache | 不是普通刷新，不承诺既有连接同步改变 |

官方接口与副作用以 [Mihomo RESTful API](https://wiki.metacubex.one/en/api/) 为准；URLTest 的 fixed 行为还能在 [`urltest.go`](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/adapter/outboundgroup/urltest.go) 中复核。

### 2.3 “简洁”的定义

简洁不是删掉可靠性，也不是把五个状态压成一个开关。这里把简洁定义为：普通用户只学习少量任务 interface，复杂排序、验证、回滚、所有权和平台差异隐藏在深模块 implementation 内。

最低可用闭环应只有：

1. 导入或选择 Profile；
2. 验证并应用，不破坏 last-known-good；
3. 选择接管方式并验证实际状态；
4. 选择模式和关键代理组；
5. 看见当前是否可用、失败在哪一层、下一步能做什么。

DNS、规则、Provider、连接、日志和网络探测是必要诊断能力，但不等于都要成为首页或一级导航。脚本市场、Sub-Store、多内核、全量字段表单则不是合格客户端的门槛。

## 3. 横向比较方法

### 3.1 不做功能数量评分

本报告不对“有 WebDAV、主题、插件、规则编辑器、多少个页面”简单计数。功能更宽可能提高专家效率，也可能扩大权限、状态同步、失败恢复和学习成本。

比较优先级是：

1. **真实性**：显示的是 intent、readback 还是推测；未知是否被诚实表达。
2. **可恢复性**：失败是否保留 last-known-good，是否区分明确拒绝与结果不确定。
3. **副作用控制**：是否默认断连接、改 DNS、提权、静默更新或覆盖源文件。
4. **模块深度**：调用者需要学习多少排序与错误规则；interface 是否是自然测试面。
5. **可支持性**：错误是否局部、可行动、可复制、可脱敏；是否有诊断而不只是配置。
6. **交互成本**：首次成功路径、信息架构、键盘与可访问性。

### 3.2 合格门槛与加分项

| 类型 | 能力 |
|---|---|
| 门槛 | 配置验证与回滚、托管生命周期、Controller 安全、接管真实状态、代理组语义、明确错误与退出清理 |
| 高价值加分 | DNS query/flush、structured logs、Provider 运维、端到端诊断、脱敏支持包、精确连接清理 |
| 专家可选 | YAML/Merge 覆写、完整规则编辑、LAN controller、手动内核维护 |
| 不应默认 | JS 执行、远程 Controller、静默安装、切换即 close-all、全量功能导航 |

## 4. 四客户端横向矩阵

> 表中“强/弱”是本报告的产品判断；具体事实以后续独立章节和 permalink 为准。

| 维度 | Clash Verge Rev | Clash Party | ClashMi | FlClash |
|---|---|---|---|---|
| 外壳/主要栈 | Tauri 2 + Rust + React/MUI | Electron + TS + React/HeroUI | Flutter/Dart + 缺失的原生插件 | Flutter/Riverpod + Go core + Rust IPC |
| 模块边界 | 后端 runstate/manager/proxy control 较深 | main/preload/renderer 清楚，但主 manager 和 IPC 面偏宽 | 静态 Manager/回调多，关键插件不可审查 | lifecycle 深；`CoreInterface` 约 30 方法且转发多 |
| 生命周期 | 锁、代际、readiness、回滚、service/sidecar 恢复强 | 串行队列、阶段、ready、有限自恢复、有界退出强 | Flutter 层失败即 stop；平台核心不完整 | latest intent + revision + lease/generation/unconfirmed exit 强 |
| 配置事务 | Draft generate/validate/apply/commit，结果分型清楚 | 远程更新最终候选 `-t` 后提交；本地编辑语义不一致 | staging 思路存在，但弱验证和异常分支可破坏 live | 订阅先验证；generated config 非原子且失败会切默认 runtime |
| System Proxy 真相 | 读取并比对 OS actual，较强 | `Status` 实际只读 config intent | 有 self-address 比对，但无完整 intent/actual/ownership | intent 派生；写失败只记 warning，无 readback/ownership |
| TUN 真相 | capability 较强，actual capture 仍不足 | 配置 intent 冒充 status，权限失败可静默关闭 | 首页只看 service connected；插件细节不可审查 | 桌面 setuid core；Android 状态收敛好，actual path 仍未聚合 |
| `hidden/fixed` | hidden、fixed、unfix 完整 | hidden、fixed、restore-auto 完整 | hidden 完整，fixed/unfix 缺失 | Rule 下 hidden；fixed/unfix 缺失 |
| 测速副作用 | 单节点/provider-aware，避免 group delay | 逐节点/provider healthcheck，避免 group delay | 逐节点并发限制，避免 group delay | 嵌套实际节点 + 有界批量，结果显式 |
| 切换连接 | 可按旧 chain 精确清理 | 默认 close-all | 默认不清连接 | 乐观持久化且默认 close-all |
| DNS/日志 | 无 DNS query/flush；日志用本地时间 | 配置宽但无 query/flush；日志用本地时间 | DNS query 进入 Network Check；文本日志 | 无 query/flush；日志用本地时间，连接 stale 不可见 |
| 网络诊断 | 能力散落，未形成四层摘要 | 有网络页但状态不聚合 | DNS/TUN/显式代理/route 可复制检查突出 | 设置能力宽，操作型诊断不足 |
| 更新安全 | App updater 签名；core 只验自报版本 | App SHA-256 + macOS 签名；特定 core 无 digest | 同源 hash 可为空，且移除 macOS quarantine | App 只打开 release 较克制；全局 TLS 绕过与无认证 Helper 严重 |
| 测试可信度 | 状态机测试较强，真实 OS 仍有缺口 | 插件测试多，核心高风险链路覆盖弱 | 无公开 test/CI，且依赖缺失 | lifecycle/键盘行为测试强；系统代理跨层契约不足 |
| 首页/IA | 八主导航、首页卡片偏多 | 十五一级入口，没有可信总览 | 主路径短，移动布局直接放大到桌面 | 自适应导航好；自由 Dashboard 成本高、空状态动作分离 |
| 键盘/A11y | MUI 基础好，仍有 click-only/右键唯一入口 | 连接行、侧栏 DnD、Escape 等问题明显 | Material 基础，桌面键盘弱、可禁字体缩放 | focus preservation/测试好；24px 控件与 1.4 字号上限有问题 |

**矩阵结论。** Clash Verge Rev 和 Clash Party 的功能宽度不是追赶清单，ClashMi 的短首页也不能补偿单一 `connected`。可组合的参考是：Verge 的 runstate/精确清连接、Party 的候选验证/串行生命周期/WS generation/fixed 交互、ClashMi 的短主路径/可复制诊断；最终用 ZenClash 的 ownership、受控配置和真实 E2E 收进更小 interface，而不是拼接四套页面。

## 5. Clash Verge Rev：成熟控制系统，不是表层模板

### 5.1 代码架构

**源码事实。** 项目是 React/TypeScript + Rust/Tauri 2；Rust workspace 拆出多个 crate，高权限行为位于后端。内核通过 Unix socket/Windows named pipe 控制，Unix 使用 `umask 077`，Windows 使用用户限定 SDDL，并等待 `/version` ready 后发布状态。[workspace](https://github.com/clash-verge-rev/clash-verge-rev/blob/3503a2da29d68a4398c0b8e9234cffb711e65783/Cargo.toml#L1-L11) · [sidecar state](https://github.com/clash-verge-rev/clash-verge-rev/blob/3503a2da29d68a4398c0b8e9234cffb711e65783/src-tauri/src/core/manager/state.rs#L91-L188)

**产品判断。** `manager/runstate/proxy_control/enhance` 的 seam 清楚；但生命周期、Profile 和 proxy control 文件规模很大。ZenClash 应借其状态模型，不复制其实现规模或 service/sidecar 模式数量。

### 5.2 可靠性与状态

**源码事实。** 启动/停止有操作锁、代际与幂等检查；sidecar 有有界探活，不 ready 会杀进程。Windows sidecar → service 失败时尝试恢复 sidecar。[lifecycle](https://github.com/clash-verge-rev/clash-verge-rev/blob/3503a2da29d68a4398c0b8e9234cffb711e65783/src-tauri/src/core/manager/lifecycle.rs#L465-L529) · [恢复](https://github.com/clash-verge-rev/clash-verge-rev/blob/3503a2da29d68a4398c0b8e9234cffb711e65783/src-tauri/src/core/manager/lifecycle.rs#L702-L788)

**源码事实。** `RunState` 持有 service health、pending action 与操作锁，派生 capability；传输错误保留上次确认状态，而不是把“读不到”写成“未安装”。[RunState](https://github.com/clash-verge-rev/clash-verge-rev/blob/3503a2da29d68a4398c0b8e9234cffb711e65783/src-tauri/src/core/runstate/mod.rs#L1-L38) · [unknown 处理](https://github.com/clash-verge-rev/clash-verge-rev/blob/3503a2da29d68a4398c0b8e9234cffb711e65783/src-tauri/src/core/runstate/mod.rs#L98-L159)

**产品判断。** “Unknown 是一等状态”和后端派生 capability 是最值得借鉴的部分。待真实行为测试确认的风险是 sidecar 意外终止路径没有明确展示 OS 系统代理清理。

### 5.3 配置、代理与安全

**源码事实。** `DraftTransaction` 按 generate → validate → apply → commit 执行，明确区分拒绝、回答不确定等结果；文件持久化使用同目录临时文件、flush 与原子 rename。[配置事务](https://github.com/clash-verge-rev/clash-verge-rev/blob/3503a2da29d68a4398c0b8e9234cffb711e65783/src-tauri/src/core/manager/config.rs#L25-L126) · [原子写入](https://github.com/clash-verge-rev/clash-verge-rev/blob/3503a2da29d68a4398c0b8e9234cffb711e65783/src-tauri/src/utils/help.rs#L54-L111)

**源码事实。** 代理列表尊重 `hidden`/`fixed`，再次选择固定节点可 unfix；如启用关闭连接，只删除 chains 包含旧代理的连接。[组渲染](https://github.com/clash-verge-rev/clash-verge-rev/blob/3503a2da29d68a4398c0b8e9234cffb711e65783/src/components/proxy/use-render-list.ts#L46-L59) · [选择与 unfix](https://github.com/clash-verge-rev/clash-verge-rev/blob/3503a2da29d68a4398c0b8e9234cffb711e65783/src/hooks/use-proxy-selection.ts#L66-L103) · [精确清连接](https://github.com/clash-verge-rev/clash-verge-rev/blob/3503a2da29d68a4398c0b8e9234cffb711e65783/src/hooks/use-proxy-selection.ts#L1-L32)

**源码事实。** Tauri App updater 有公钥；core 更新则在执行 staged binary 后依赖自报版本，没有看到独立发布签名或固定 SHA-256。[App updater](https://github.com/clash-verge-rev/clash-verge-rev/blob/3503a2da29d68a4398c0b8e9234cffb711e65783/src-tauri/tauri.conf.json#L32-L43) · [core upgrade](https://github.com/clash-verge-rev/clash-verge-rev/blob/3503a2da29d68a4398c0b8e9234cffb711e65783/src-tauri/src/feat/core_upgrade.rs#L45-L120)

### 5.4 产品设计取舍

**源码事实。** 主导航有八项，首页九类核心卡片默认几乎全开；空 Profile 卡能引导导入，TUN service 在用户触发后再引导安装。[导航](https://github.com/clash-verge-rev/clash-verge-rev/blob/3503a2da29d68a4398c0b8e9234cffb711e65783/src/pages/_navigation-meta.ts#L1-L25) · [首页卡片](https://github.com/clash-verge-rev/clash-verge-rev/blob/3503a2da29d68a4398c0b8e9234cffb711e65783/src/pages/home.tsx#L73-L99) · [空状态](https://github.com/clash-verge-rev/clash-verge-rev/blob/3503a2da29d68a4398c0b8e9234cffb711e65783/src/components/home/home-profile-card.tsx#L252-L280)

**产品判断。** “用到 TUN 再提权”正确；主导航和默认首页更像专家总台。空状态动作却由不可聚焦 `Box onClick` 承担，说明“有 CTA”仍不足以满足键盘主路径。

### 5.5 ZenClash 借鉴与避免

**借鉴：** 后端单一运行事实、Unknown/pending action/derived capability；生命周期锁、代际、ready、有界恢复；配置结果分型；`hidden/fixed/unfix` 与按旧 chain 精确清连接；错误脱敏和按需提权。

**避免：** 八个平级导航与默认全开卡片；TUN 配置冒充 actual capture；无独立摘要的 core 更新；空 CSP、宽 scope 与远程 raw HTML 的组合；click-only、右键唯一入口和标签不足的 Switch。

## 6. Clash Party：候选配置与并发可靠性强，状态表层偏弱

### 6.1 代码架构

**源码事实。** 项目采用 Electron + React 19 + TypeScript；主进程负责文件、网络、子进程和 OS 行为，preload 维护 IPC 白名单，renderer 负责界面。[技术栈](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/package.json#L1-L38) · [preload 白名单](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/preload/index.ts#L193-L239)

**产品判断。** main/preload/renderer 是清楚的平台层次；约 160 个 invoke channel 和几个大型总管文件使 renderer interface 偏宽。ZenClash 不应按底层动作一对一扩展页面调用面。

### 6.2 生命周期与配置事务

**源码事实。** 内核操作通过 promise tail 串行并维护 lifecycle phase；启动前生成最终配置、可执行 `mihomo -t`，等待日志或 `post-up` ready，启动后再连接四条流。异常退出有限重启，退出使用同一清理 promise 和整体超时。[串行与阶段](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/main/core/manager.ts#L88-L163) · [启动准备](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/main/core/manager.ts#L440-L510) · [退出](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/main/core/manager.ts#L796-L855)

**源码事实。** 远程订阅先在临时目录生成包含覆写和受控设置的最终候选，调用 Mihomo `-t`，成功后才原子写源订阅；仓库有 last-known-good 回归测试。[候选验证](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/main/config/profile.ts#L470-L568) · [回归测试](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/main/config/profile.test.ts#L101-L128)

**产品判断。** 这是四款中最直接可复用的远程订阅事务参考。但本地编辑先改源再 reload，未共享相同事务，说明正确行为必须由一个深 module interface 强制，而不能靠每个入口自觉遵守。

### 6.3 状态、代理语义与诊断

**源码事实。** `SysProxyStatus` 与 `TunStatus` 实际只读取 app/controlled config 中的布尔 intent；TUN 权限不足时启动流程还能静默把配置改为 false。[System Proxy status](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/main/core/mihomoApi.ts#L649-L654) · [TUN status](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/main/core/mihomoApi.ts#L656-L678) · [权限处理](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/main/core/permissions.ts#L348-L407)

**源码事实。** 四条 WebSocket 流各自维护 active/retry/generation，generation 阻止旧连接回调污染新会话；重连耗尽没有通知 UI。[流状态](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/main/core/mihomoApi.ts#L20-L145)

**源码事实。** 代理页默认过滤 hidden、显示 fixed、支持 DELETE 恢复自动；组测速使用逐节点/provider healthcheck。切换代理或模式默认关闭全部连接。[代理组](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/main/core/mihomoApi.ts#L297-L382) · [组测速](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/renderer/src/pages/proxies.tsx#L264-L412) · [close-all](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/renderer/src/pages/proxies.tsx#L145-L158)

**产品判断。** 后端流生命周期强，前端新鲜度和失败状态弱；代理组语义正确，默认连接副作用不符合最小惊讶原则。

### 6.4 错误、安全与产品设计

**源码事实。** profile/config hook 捕获错误后不再抛出，上层导入流程在 await 后无条件清空输入。[吞错 hook](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/renderer/src/hooks/use-profile-config.tsx#L46-L83) · [清空输入](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/renderer/src/pages/profiles.tsx#L157-L173)

**源码事实。** App 更新校验 SHA-256，macOS 发布签名/公证；运行时特定 core 下载只验证 HTTP 与可解压性。JS 覆写在主进程 Node `vm` 同步执行且无 timeout；Node 官方明确 `vm` 不是安全机制。[App digest](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/main/resolve/autoUpdater.ts#L179-L220) · [core 下载](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/main/utils/github.ts#L133-L166) · [JS 执行](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/main/core/factory.ts#L358-L399) · [Node VM 文档](https://nodejs.org/api/vm.html)

**源码事实。** 默认侧栏有 15 个一级入口，首次导览在开始前就写入 `tourShown=true`；连接表格行使用 `<tr onClick>` 而无键盘语义。[一级入口](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/shared/appConfig.ts#L13-L29) · [导览](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/renderer/src/utils/tour.ts#L11-L194) · [连接行](https://github.com/mihomo-party-org/clash-party/blob/061faeefd15849d31062e78cbd4084bad7f0f497/src/renderer/src/components/connections/connection-table.tsx#L321-L401)

### 6.5 ZenClash 借鉴与避免

**借鉴：** promise tail 串行、阶段状态、ready 与有界退出；最终候选 `-t` 后提交；WS generation/retry、helper 自愈；`hidden/fixed/restore-auto`、逐节点测速、有界日志与虚拟化。

**避免：** intent 冒充 Status；mutation hook 吞错；切换默认 close-all；主进程执行无资源限制 JS；无摘要的可执行下载、默认静默更新与十五入口 IA。

## 7. ClashMi：产品路径短，但公开核心边界不可验证

### 7.1 代码架构与审查边界

**源码事实。** ClashMi 使用 Flutter/Dart，覆盖移动与桌面；业务状态主要由静态 Manager 和回调列表持有。公开依赖指向仓库外 `libclash_vpn_service` 与 `board_service`，Android 还引用未提供 AAR。[全局初始化](https://github.com/KaringX/clashmi/blob/917fd46085d71e8a1caa91f018681337283d5162/lib/app/modules/biz.dart#L9-L66) · [缺失依赖](https://github.com/KaringX/clashmi/blob/917fd46085d71e8a1caa91f018681337283d5162/pubspec.yaml#L98-L108) · [Android AAR](https://github.com/KaringX/clashmi/blob/917fd46085d71e8a1caa91f018681337283d5162/android/libclash/build.gradle#L1-L2)

**产品判断。** 跨平台交付范围很强，但公开仓库不能作为生命周期、权限和系统代理可靠性的完整架构证据。静态 Manager 的前置条件和副作用散落，弱于 ZenClash 现有 session interface。

### 7.2 生命周期、配置与状态

**源码事实。** Flutter 层 start/restart 有 60 秒上限，插件或 stderr 错误会 stop；显式退出反向释放并停止桌面服务。[VPN lifecycle](https://github.com/KaringX/clashmi/blob/917fd46085d71e8a1caa91f018681337283d5162/lib/app/local_services/vpn_service.dart#L305-L434)

**源码事实。** 订阅正常路径下载到临时文件、解密、弱格式检查后 rename；但解密失败分支可删除 live 文件，备用通道还存在未走同一验证/提交路径的不一致。[订阅事务](https://github.com/KaringX/clashmi/blob/917fd46085d71e8a1caa91f018681337283d5162/lib/app/modules/profile_manager.dart#L767-L928)

**源码事实。** 首页的绿点、文案和总开关全部由 `FlutterVpnServiceState.connected` 派生；Controller/traffic 失败可能只让数值变空，不改变“已连接”。[单一状态](https://github.com/KaringX/clashmi/blob/917fd46085d71e8a1caa91f018681337283d5162/lib/screens/home_screen_widgets.dart#L179-L257) · [错误吞掉](https://github.com/KaringX/clashmi/blob/917fd46085d71e8a1caa91f018681337283d5162/lib/screens/home_screen_widgets.dart#L789-L989)

**产品判断。** “失败就 stop”是正确收敛；单一 `connected` 过度承诺。ZenClash 应吸收短路径，不吸收状态压缩。

### 7.3 代理、诊断与安全

**源码事实。** 代理 DTO 解析并隐藏 `hidden`，测速桌面并发 10、移动并发 5，节点切换只 PUT 而不关闭连接；没有建模 `fixed`/unfix。[代理与 hidden](https://github.com/KaringX/clashmi/blob/917fd46085d71e8a1caa91f018681337283d5162/lib/app/clash/clash_http_api.dart#L210-L249) · [测速](https://github.com/KaringX/clashmi/blob/917fd46085d71e8a1caa91f018681337283d5162/lib/screens/proxy_board_screen_widgets.dart#L365-L427) · [选择](https://github.com/KaringX/clashmi/blob/917fd46085d71e8a1caa91f018681337283d5162/lib/app/clash/clash_http_api.dart#L522-L543)

**源码事实。** Network Check 顺序执行 A/AAAA DNS、TUN 下 HTTPS、经 mixed port HTTPS 和 route 收集，并允许复制结果。[Network Check](https://github.com/KaringX/clashmi/blob/917fd46085d71e8a1caa91f018681337283d5162/lib/screens/net_check_screen.dart#L42-L210)

**源码事实。** App 更新按元数据 SHA-256 校验，但 hash 缺失或计算失败时仍可能保留文件；macOS 主动清理 quarantine/provenance。[hash 处理](https://github.com/KaringX/clashmi/blob/917fd46085d71e8a1caa91f018681337283d5162/lib/app/modules/auto_update_manager.dart#L222-L299) · [xattr](https://github.com/KaringX/clashmi/blob/917fd46085d71e8a1caa91f018681337283d5162/lib/app/modules/auto_update_manager.dart#L308-L345)

### 7.4 产品设计与可测试性

**源码事实。** 首页优先展示连接、模式、Profile，连接后才出现代理、运行配置和网络检测；原生页没有完整连接/Provider/structured logs，部分能力交给 Zashboard。[首页](https://github.com/KaringX/clashmi/blob/917fd46085d71e8a1caa91f018681337283d5162/lib/screens/home_screen_widgets.dart#L179-L340)

**源码事实。** 锁定提交没有 `test/` 和 CI workflow，公开依赖又不完整。[仓库树](https://github.com/KaringX/clashmi/tree/917fd46085d71e8a1caa91f018681337283d5162)

**产品判断。** 移动端任务优先级清楚，桌面却像放大的手机长页；桌面键盘、并列信息和可复现测试不是一等能力。ZenClash 可以采用其任务顺序与诊断内容，不应引入第二套 dashboard 或移动布局直搬。

### 7.5 ZenClash 借鉴与避免

**借鉴：** 首页只突出捕获、模式、Profile 与关键代理；DNS/TUN/显式代理/route 的可复制诊断；hidden、有限并发测速；切换默认不破坏连接；启动失败主动 stop。

**避免：** 静态 Manager/散落回调；service connected 冒充真实路径；staging 失败操作 live 或备用通道绕过验证；第二套 Web UI；hash 不确定仍保留更新包、移除 provenance/quarantine；不可复现构建与缺失行为测试。

## 8. FlClash：生命周期 module 很深，系统与安全真相偏弱

### 8.1 架构与 module 深度

**源码事实。** 主应用使用 Flutter/Riverpod/Drift，Go core 以 Mihomo fork 为替换依赖，Rust 负责桌面 socket/pipe，Android 另有 Service/VPN 层。[Flutter 依赖](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/pubspec.yaml#L8-L85) · [Go core](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/core/go.mod#L1-L12) · [Rust IPC](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/plugins/rust_api/rust/Cargo.toml#L1-L21)

**源码事实。** `CoreInterface` 同时暴露 lifecycle、配置、代理、Provider、Geo、流量、日志、连接与清理约 30 个方法；`CoreController` 多数只是平台转发。[宽 interface](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/core/interface.dart#L9-L80) · [转发 controller](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/core/controller.dart#L132-L243)

**产品判断。** Android/desktop 两个 adapter 让平台 seam 成立，但把全部领域能力放进一个总线导致 interface 浅而宽；ZenClash 不应为每个 RPC 增加转发 wrapper。

### 8.2 生命周期与 IPC：最值得借鉴的部分

**源码事实。** `DesktopCoreLifecycleController` 外部只有 state/states/crashEvents/start/restart/stop/close/wait；实现把命令编码为 revision 与 desired state，由单 worker reconcile 最新意图。[窄 interface](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/core/desktop/lifecycle.dart#L7-L23) · [revision worker](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/core/desktop/lifecycle.dart#L195-L290)

**源码事实。** start 管理 transport、session、lease、IPC 和 Windows peer PID；无法确认退出时保存 unconfirmed lease 并阻止 replacement，避免同时留下两个 core。[start session](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/core/desktop/lifecycle.dart#L390-L487) · [stop/lease](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/core/desktop/lifecycle.dart#L530-L638)

**源码事实。** RPC 有 request ID、pending map、连接/空闲/硬 deadline 和断连批量失败；Rust IPC 与 Go 事件队列都有帧/容量上限和退避。[RPC](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/core/desktop/rpc_client.dart#L31-L159) · [Rust queue](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/plugins/rust_api/rust/src/api/ipc.rs#L43-L97) · [Go queue](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/core/message.go#L5-L123)

**产品判断。** latest-intent、lease ownership、generation 与 unconfirmed-exit 是四款中非常强的生命周期参考；应移植行为契约，不复制语言分层。

### 8.3 配置、捕获与代理语义

**源码事实。** URL 订阅先写临时文件并让 core 验证，通过才覆盖 source；但生成后的 `config.yaml` 直接写入再 apply，非原子且无上一代，Go apply 失败会把 runtime 切到默认配置。[订阅验证](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/models/profile.dart#L175-L209) · [generated config](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/providers/actions/setup.dart#L376-L431) · [失败切默认](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/core/common.go#L262-L278)

**源码事实。** System Proxy 写入被串行化，但失败只记 warning；托盘状态由 intent/配置派生，无 OS readback/ownership。桌面 TUN 授权还把整个 Go core 设为 setuid root。[ProxyManager](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/manager/proxy_manager.dart#L18-L60) · [状态派生](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/providers/state.dart#L104-L156) · [setuid](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/common/system.dart#L62-L138)

**源码事实。** 代理模型有 hidden、无 fixed；选择先持久化，忽略 RPC 错误，并默认 close-all。连接轮询失败保留旧列表但不标 stale，日志改用本地时间。[组状态](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/providers/state.dart#L18-L43) · [乐观选择](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/providers/actions/proxies.dart#L75-L88) · [日志时间](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/models/common.dart#L139-L155)

### 8.4 安全、产品设计与测试

**源码事实。** App 更新只检查 GitHub latest、展示说明并打开官方 release 页面，不自行执行安装包；但全局 `HttpOverrides` 接受任意坏证书，影响订阅、更新和 WebDAV 等请求。Windows 提权 Helper 还在 loopback 暴露未认证 `/start/stop/logs`，其中 start 会先终止当前 managed core。[更新提示](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/providers/actions/common.dart#L55-L80) · [TLS 绕过](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/common/http.dart#L8-L30) · [全局安装](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/main.dart#L13-L27) · [Helper routes](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/services/helper/src/service/hub.rs#L497-L554)

**产品判断。** 两者都是 P0 安全问题：不能用全局证书绕过换兼容性；高权限 helper 必须以 OS identity/ACL 或强 secret 验证调用方。

**源码事实。** 窄屏 bottom navigation、桌面 NavigationRail 与 focus preservation 做得好，并有真实键盘行为测试；无 Profile 时 start 消失，空态主动作与说明分离。自由 Dashboard 可删除/拖拽重要卡片，文字缩放被限制到 1.4。[自适应导航](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/pages/home.dart#L31-L139) · [焦点恢复](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/manager/app_manager.dart#L158-L173) · [键盘测试](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/test/widgets/fab_focus_test.dart#L72-L142) · [Dashboard](https://github.com/chen08209/FlClash/blob/62addf738a76b1a492e19af2dbabdb6d572b9e72/lib/views/dashboard/dashboard.dart#L17-L240)

**借鉴：** latest-intent lifecycle、lease/generation/unconfirmed-exit、有界 IPC、Android Service 收敛、订阅先验证、自适应导航、focus preservation 和键盘行为测试。

**避免：** 宽 `CoreInterface`、全局 TLS 绕过、无认证 Helper、setuid 大内核、intent 冒充 actual、非事务 runtime apply、乐观选择 + close-all、自由 Dashboard 和文字缩放硬上限。

## 9. ZenClash 当前基线：优势与 gap analysis

### 9.1 已经领先或方向正确的能力

#### A. `CoreSession` 是值得继续加深的 module

**源码事实。** `CoreSession` 用 `open/apply/maintain/shutdown/snapshot` 表达托管与外部内核的意图和事实，生命周期操作串行化；外部内核不会被 ZenClash 擅自重启或终止。[`core_session.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-core/src/core_session.rs#L100-L207)

**产品判断。** 这比 ClashMi 的全局静态 Manager 更容易证明行为，也比页面直接编排 start/reload/restart 更有 locality。新能力应通过这个 interface 或其内部 seam 进入，不应在 `RuntimePage` 旁路实现生命周期排序。

#### B. `SystemProxySession` 的 intent/actual/ownership 是差异化优势

**源码事实。** 当前 module 区分用户意图、OS 实际值与可验证所有权，并包含事务、reconcile 和退出释放。[`system_proxy.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-core/src/system_proxy.rs#L261-L427)

**产品判断。** 这比 Clash Party 的 config boolean、ClashMi 的简单 self-address 检查和多数客户端退出时无 ownership 清理更可靠。后续 UI 应忠实呈现这三个维度，而不是为了界面简洁重新压成 `enabled: bool`。

#### C. `ControlledConfigStore` 已具备正确事务方向

**源码事实。** ZenClash 保留导入源，生成受控运行副本，验证候选并提交或回滚；不会原地改写订阅/YAML 源。[`controlled_config.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-core/src/controlled_config.rs#L122-L299) · [数据安全约定](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/README.md#L127-L136)

**产品判断。** 这是 ZenClash 应坚持的核心承诺。Clash Party 说明“最终候选 `-t`”值得加强；ClashMi 的异常分支说明任何入口绕过这个 module 都可能破坏 last-known-good。

#### D. Controller 与供应链基础较好

**源码事实。** 托管 Controller 使用 loopback 临时端口和随机 256-bit secret。[`endpoint.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-core/src/endpoint.rs#L35-L52) · [`main.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-ui/src/main.rs#L758-L769)

**源码事实。** 仓库有真实 Mihomo E2E，覆盖运行配置、模式、节点、规则/Provider、连接、traffic WebSocket、订阅与覆写。[`real_mihomo.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-core/tests/real_mihomo.rs#L21-L58)

**产品判断。** 真实内核测试是 ClashMi 公开仓库不具备、Clash Party 高风险链路也覆盖不足的竞争优势。不要用更多 mock 替代；应在这条 harness 上继续增加行为语义。

#### E. 当前 IA 已比功能总台克制

**源码事实。** 当前一级入口是首页、代理组、订阅、连接、规则、用量、日志、设置；DNS/TUN/内核/覆写/资源/网络位于设置高级工具。[`pages.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-ui/src/pages.rs#L46-L55) · [`settings.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-ui/src/pages/runtime/settings.rs#L54-L83)

**产品判断。** 方向正确，但八个入口仍可进一步围绕任务收束；不要因为竞品有独立 DNS/Sniffer/Core 页面而反向扩宽。

### 9.2 P0 gap：会影响“用户能否相信界面”

#### A. 首页是全成或全败

**源码事实。** Dashboard 并发读取 config、proxy catalog、connections、system proxy，随后逐项使用 `?`；任一失败会丢弃其他成功结果。[`loader.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-ui/src/pages/runtime/loader.rs#L72-L90)

**产品判断。** 这违反局部错误恢复：`/connections` 暂时失败不应让当前 Profile、模式和系统代理真相消失。每个状态分片应独立为 `Loading/Fresh/Stale/Failed`，保留最后可信时间与局部重试。

#### B. 观测流容易被误读为接管状态

**源码事实。** 首页头部显示 traffic WebSocket 的“实时流已连接”，System Proxy 与 mode 是主要控制；TUN/capture/path 没有同级摘要。[`view.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-ui/src/pages/runtime/view.rs#L9-L102) · [`home.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-ui/src/pages/runtime/home.rs#L337-L430)

**产品判断。** 文案本身没有直接说“代理已连接”，但视觉层级可能让用户把统计流连通当作产品总状态。应以 L1–L4 运行摘要取代单一绿色流标志。

#### C. 代理组模型不完整

**源码事实。** 当前解析 `hidden` 但只降低排序，仍默认展示；模型没有 Mihomo `fixed`。[`proxy.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-core/src/proxy.rs#L87-L102) · [`proxy.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-core/src/proxy.rs#L265-L272)

**产品判断。** 这是正确性而非装饰缺口。自动组点击后可能已固定，UI 却无法解释；LoadBalance 也不应伪装成唯一手选节点。

#### D. 节点切换默认破坏全部连接

**源码事实。** `ProxyOperations::select` 成功 PUT 后无条件 `DELETE /connections`。[`proxy_operations.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-core/src/proxy_operations.rs#L38-L73)

**产品判断。** 这会中断下载、通话和长连接。默认应只影响新连接；需要时提供显式“切换并重建连接”，后续可像 Clash Verge Rev 一样按旧 chain 精确清理。

#### E. 没有状态驱动的首次成功路径

**源码事实。** 产品已有 Profile 导入、真实内核验证、System Proxy/TUN 和网络探测，但没有把它们组织为可续接闭环。

**产品判断。** 用户不应先学习页面结构。下一步应由真实状态派生：无 Profile → 导入 → 验证 → 选择接管方式 → 权限/actual → 路径探测；不要另存容易漂移的 wizard 进度。

#### F. 主导航还不是完整键盘路径

**源码事实。** Sidebar 项是带 `on_click` 的 `h_flex`，而不是明确可聚焦的 Button/NavigationItem。[`sidebar.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-ui/src/components/sidebar.rs#L123-L153)

**产品判断。** “鼠标能点”不等于键盘可操作。所有一级导航、首页主操作、代理选择和连接详情都应有可见焦点、逻辑 Tab 顺序和一致的激活键。

### 9.3 P1 gap：会增加排障与支持成本

1. **DNS 缺少操作型诊断。** `MihomoClient` 尚无 `/dns/query`、`/cache/dns/flush`、`/cache/fakeip/flush`。[`client/api.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-core/src/client/api.rs#L19-L365)
2. **日志丢失内核结构。** 当前普通 `/logs` 帧被赋本地接收时间；应 structured-first 并保留 server time/fields。[`logs.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-core/src/logs.rs#L74-L95) · [`logs.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-core/src/logs.rs#L219-L260)
3. **网络探测未进入统一运行状态。** 现有 probe 能独立保留 provider/目标错误，是好基础，但还没有形成四层摘要或脱敏支持包。[`network/probe.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-core/src/network/probe.rs#L235-L274)
4. **缺少原生 Provider 运维摘要。** 应显示上次更新时间、结果、数量、手动更新/healthcheck，而不是把所有高级字段搬到首页。
5. **应用更新闭环缺失。** 先做官方 release 通知与签名安装链接；供应链验证和回滚成熟前不做静默安装。

### 9.4 P2 gap：模块 interface 与页面状态仍在变浅

**源码事实。** `RuntimePage` 有 49 个字段，同时持有 session/client/process/store、各页表单、缓存和全局 `loading/mutating/error/notice`。[`runtime.rs`](https://github.com/HaiwenZhang/zenclash/blob/7415b25f4a3e70af9a50f690dd56bfc403cd808e/crates/zenclash-ui/src/pages/runtime.rs#L69-L117)

**产品判断。** 页面已成为工作流协调者：新增操作时，调用者必须理解越来越多的排序、代次与错误规则。这是浅 module 的信号。页面应只拥有输入、筛选、展开与焦点；配置应用、接管变更、代理选择和诊断归入对应深 module。

## 10. 首页与信息架构参考

### 10.1 首页只回答五个问题

1. 当前使用哪个 Profile，最后成功更新时间是什么？
2. 内核和 Controller 是否可控？
3. 当前接管方式是什么，实际是否生效？
4. 当前 mode 与关键代理组是什么，自动组是否 fixed？
5. 有没有需要用户处理的问题，最直接的恢复动作是什么？

推荐信息层级：

```text
┌─────────────────────────────────────────────────────────────┐
│ Profile: Work · updated 3m ago              Mode: Rule      │
├─────────────────────────────────────────────────────────────┤
│ Capture                                                      │
│ System Proxy: Active · owned by ZenClash   [Turn off]        │
│ TUN: Off                                   [Use TUN…]        │
│ Path: Last check passed 2m ago              [Run check]      │
├─────────────────────────────────────────────────────────────┤
│ Primary group: Proxy · Auto → HK 01 · not fixed [Change]     │
├──────────────────────────────┬──────────────────────────────┤
│ ↑ 1.2 MB/s  ↓ 8.4 MB/s       │ 37 active connections        │
│ values + direct labels       │ [Open connections]           │
├─────────────────────────────────────────────────────────────┤
│ Action needed: Provider X update failed; current data kept   │
│ [Retry] [Details]                                             │
└─────────────────────────────────────────────────────────────┘
```

**产品判断。** 首页不需要显示完整 DNS nameserver、Sniffer 规则、内核下载源、每个 Provider 或所有代理组。它需要把“当前仍有效什么”和“失败后能做什么”说清楚。

### 10.2 可操作空状态

`ui-ux-pro-max` 对“Empty States”的实际检索结果要求：没有内容时提供帮助文案和动作，避免空白屏。

ZenClash 应映射为领域动作：

| 空状态 | 主文案 | 主动作 | 次动作 |
|---|---|---|---|
| 无 Profile | 还没有可用配置 | 导入订阅 | 选择本地 YAML |
| Profile 验证失败 | 新配置未应用，旧配置仍在使用 | 查看并修复 | 复制验证错误 |
| 未接管流量 | 内核可用，但系统流量尚未接管 | 开启系统代理 | 设置 TUN |
| 无关键组 | 当前配置没有可选择的普通代理组 | 查看配置 | 显示隐藏组（高级） |
| 无连接 | 当前没有活动连接 | 运行路径检查 | 查看最近关闭连接 |
| 无日志 | 暂无内核日志 | 调整级别 | 检查日志流状态 |

空状态动作必须是真正可聚焦的交互控件，而不是仅有 `on_click` 的布局元素。

### 10.3 局部错误恢复与新鲜度

`ui-ux-pro-max` 对 Error Recovery 的实际检索结果要求给出清晰下一步；表单错误应靠近字段，不能只用顶层错误或 toast。

首页每个数据分片采用：

```text
Loading  -> 首次读取，保留布局
Fresh    -> 当前成功值 + observed_at
Stale    -> 上次成功值 + “多久前” + 正在重试/手动重试
Failed   -> 无可信值 + 原因分类 + 主恢复动作
```

持续故障必须停留在所属卡片，直到恢复：

- System Proxy 被外部修改 → 显示 ownership lost，并提供“重新接管”；
- TUN 权限不足 → 显示实际 off，并提供平台对应授权入口；
- Controller 认证失败 → 不清空 Profile/capture 卡；
- Provider 更新失败 → 显示“当前仍使用上次成功版本”；
- Profile 导入失败 → 保留 URL/token/key 输入，只在字段附近显示具体错误。

### 10.4 键盘焦点与状态编码

`ui-ux-pro-max` 的实际检索结果要求所有可操作控件可用键盘到达、焦点可见，Tab 顺序与视觉顺序一致；不能移除 focus ring 而不提供替代。

ZenClash 主路径验收包括：

- Sidebar、首页 capture、mode、关键组和错误动作全部可 Tab/Shift+Tab 到达；
- Enter/Space 激活 Button、Switch 与代理选项；
- 对话框打开后焦点进入，关闭后回到触发控件；
- 连接行不能只靠 row click，应有可聚焦详情动作；
- destructive 操作不依赖颜色区分，必须有文字和明确动词。

实时图表与状态不只依赖颜色：

- 上传/下载同时显示数值、直接标签，并可用线型或图标辅助；
- `Fresh/Stale/Failed` 同时使用文字/图标/时间，不能只用绿黄红；
- 流量流中断时保留最后值和时间，不把曲线归零伪装成当前事实；
- Tooltip 不能是理解核心状态的唯一途径。

### 10.5 状态驱动首次使用

不创建独立 `wizard_step` 真相源。启动时根据领域事实派生下一步：

```text
NoProfile
  -> Importing
  -> CandidateInvalid / CandidateReady
  -> CoreReady
  -> CaptureNotSelected
  -> PermissionNeeded / CaptureActual
  -> PathUnknown / PathPassed / PathFailed
  -> Ready
```

用户可随时退出；再次进入从事实状态续接。权限只在选择相关能力时申请，失败发生在当前步骤并保留重试，不用长导览解释全部页面。

### 10.6 推荐 IA

一级导航建议收束为五项：

| 一级入口 | 内容 |
|---|---|
| 首页 | Profile、四层状态摘要、mode、关键组、实时摘要、待处理错误 |
| 代理 | 普通组、fixed/auto、测速、节点详情；hidden 在高级开关后 |
| 配置 | Profile/订阅、更新状态、验证结果、少量结构化覆写 |
| 连接 | 活动/最近连接、route explanation、单条/显式全部关闭 |
| 设置 | General、Capture、Diagnostics、Advanced、About |

Rules、Usage、Logs 不必删除：

- Rules 进入连接详情的“为什么走这里”与 Diagnostics 的规则查看；
- Usage 成为首页摘要和 Diagnostics 的流量视图；
- Logs、DNS、Network、Resources、Core、Override、Sniffer 归入 Diagnostics/Advanced；
- 托盘继续提供高频快捷动作，但不成为唯一入口。

这是产品取舍，不是 UI 规则库对“渐进披露”的检索结论。

## 11. 深 module、interface 与 seam 建议

### 11.1 设计原则

目标不是新增一个万能 manager，而是让少量 interface 隐藏更多排序、回滚、所有权和错误分类：

- **Depth** 看调用者获得的 leverage，不看 implementation 行数。
- **Interface 是行为测试表面**；调用者和测试跨同一个 seam。
- 页面只拥有页面状态，工作流进入深 module。
- 两个 adapter 才说明 seam 真实存在；不要为单一实现制造 trait。
- 测试需要替身时用 internal seam，不把测试细节泄漏为公共 interface。

### 11.2 `OperationalStatus`

建议 interface 只有 `snapshot() -> OperationalSnapshot` 与 `subscribe() -> OperationalStatusStream`。Implementation 隐藏 L1 托管/外部 mode、退出与恢复，L2 Controller，L3 System Proxy intent/actual/ownership 和 TUN requested/configured/observed，L4 probe，各流新鲜度，以及分片的 `Loading/Fresh/Stale/Failed`/recovery action。

这是 in-process module；若只有一个聚合实现，不制造 public adapter。测试通过同一 interface 注入受控依赖并断言 snapshot。

### 11.3 `TrafficCaptureSession`

建议 interface 只有 `apply(plan) -> CaptureOutcome`、`reconcile() -> CaptureSnapshot`、`release_owned() -> CaptureOutcome`。普通 UI 把 `CapturePlan` 限制为 `Off/SystemProxy/Tun` preset；高级入口如确有需要再表达组合。

Implementation 复用 `SystemProxySession/CoreSession`，隐藏 readiness、TUN 权限和应用顺序、System Proxy actual/ownership、部分成功后的回滚/`reconcile-needed`、退出时只释放 owned 状态。真实 seam 位于 OS capture 操作：生产为平台 adapter，测试为 fake adapter。

### 11.4 `ProfileApplication`

建议 interface 只有 `preview(change) -> ProfilePreview` 与 `apply(change) -> ProfileApplyOutcome`。Implementation 统一远程更新、本地导入、手工编辑、结构化覆写与受控设置，内部执行：

```text
fetch/read -> size/path checks -> parse -> compose final candidate
-> mihomo -t -> stage -> apply -> readback -> commit
                              └─ failure -> keep source/active/runtime
```

Interface 不暴露 `stage/validate/commit` 让页面排序；outcome 区分 `Rejected { last_known_good }`、`Applied { source_version, runtime_version }`、`PersistedButRuntimeUnknown { recovery }`、`RolledBack { cause }`。

### 11.5 深化现有 `ProxyOperations`

建议围绕领域语义扩展：`catalog(visibility)`、`select(group, member, connection_policy)`、`restore_auto(group)`、`measure(scope, semantics)`。

Implementation 隐藏组类型差异、`hidden/fixed/now/all` readback、delay endpoint 副作用、`KeepExisting/RebuildAffected/RebuildAll` 和快速选择代际。

HTTP Mihomo adapter 与真实测试 adapter 是合理 internal seam；页面不应学习 PUT/DELETE endpoint 细节。

### 11.6 `NetworkDiagnostics`

建议 interface 只有 `run(plan) -> DiagnosticReport` 与 `export(report, SupportSafe) -> SupportBundle`。Implementation 聚合但不混淆 Controller、capture actual、DNS、经 Mihomo/DIRECT 探测、Provider health、route/接口；每步独立结果并标明路径和时间。导出默认移除订阅 URL/query、secret、节点凭据、完整目标、用户目录和可识别日志字段。

### 11.7 `RuntimePage` 的目标职责

页面只拥有 route/tab、输入/筛选/排序/展开/焦点、各操作局部 pending 与只读 snapshot；不再拥有 core apply/restart 决策、capture 排序/回滚、Profile staging/commit、代理组/连接语义、脱敏/诊断编排或阻塞无关操作的全局 `mutating`。

通过删除测试判断 depth：如果删掉 module 后，排序、回滚、错误分类会重新散落到多个页面，它就在提供 leverage；如果删掉后调用者几乎不变，它只是 pass-through，应合并而不是保留。

## 12. 路线图与明确取舍

### P0：先让状态可信、操作不意外

1. **OperationalStatus + 首页分片状态。** 建立 L1–L4 snapshot；Profile、capture、mode、group、connections、streams 独立 `Loading/Fresh/Stale/Failed`。
2. **首页首次成功闭环。** 无 Profile 空状态直接导入；验证后选择 capture；权限完成后运行明确路径探测；全程可中断续接。
3. **代理组语义。** 默认过滤 hidden；解析 fixed；URLTest/Fallback 可恢复自动；LoadBalance 不伪装手选。
4. **连接策略。** 普通切换默认 `KeepExisting`；显式提供 `RebuildAll`，后续加入 `RebuildAffected`。
5. **配置入口统一事务。** 远程、本地、覆写和受控设置都只通过 `ProfileApplication`；失败保留 source/active/runtime。
6. **键盘主路径。** Sidebar、首页、代理、连接详情与错误动作使用可聚焦控件，焦点可见且恢复正确。
7. **持续错误局部呈现。** 不用 toast 替代权限、ownership、Controller、Provider 和配置失败状态。

P0 完成标准不是“页面做完”，而是相关行为测试在 fake adapter 与真实 Mihomo harness 上通过。

### P1：形成可支持性优势

1. **NetworkDiagnostics。** 汇总四层状态、DNS、经 Mihomo/DIRECT 对照和 Provider health，输出结构化步骤。
2. **DNS 运维。** `/dns/query`、DNS cache flush、fake-IP flush；恢复动作有确认并独立反馈。
3. **Structured logs。** 保存 core time/level/message/fields，普通格式兼容回退，显示流新鲜度。
4. **Provider 原生运维。** 最后成功/失败、数量、手动 update/healthcheck；失败不清 last-known-good。
5. **脱敏支持包。** 版本、平台、L1–L4、最近错误、配置摘要与结构化日志，默认安全导出。
6. **TUN observed state。** 按平台补足权限、设备、route 与受控 probe，不把 configured 当 actual。
7. **安全审计自动化。** 可执行下载摘要/签名、依赖审计和 release 产物验证进入 CI/发布 harness。

### P2：收束产品表层与维护成本

1. 把一级导航收敛到首页/代理/配置/连接/设置，Rules/Usage/Logs 进入可发现的诊断结构。
2. 按操作域替换 `RuntimePage` 全局 `loading/mutating/error/notice`，减少不相关阻塞。
3. 首页流量图加入直接标签、当前值、更新时间与 stale 状态，不只使用颜色。
4. 应用更新先做通知、release notes 与官方签名安装链接；自动安装另行安全设计。
5. 高级设置保留 YAML/effective config/diff，避免完整字段表单和来源不明的 JS。
6. 真实 E2E 扩展到系统服务/TUN 的平台 harness；无法自动化的平台行为给出可重复手工协议。

### 明确暂不投入

- Sub-Store、插件市场、主题编辑器和多内核对齐；
- JavaScript/Lua 远程覆写；
- 默认开放 LAN Controller 或把 secret 放 URL；
- 复杂长期流量历史和装饰性 dashboard；
- 全量 DNS/TUN/Sniffer/Rule 字段表单；
- 默认静默下载、安装或重启；
- 在状态真实性完成前继续增加首页卡片。

## 13. 行为测试清单

### 13.1 生命周期与 Controller

1. 托管 core 并发 start/apply/stop 时按代际收敛，不产生第二个进程。
2. 启动期间收到 quit，ready waiter 结束且不会在 shutdown phase 重启。
3. `/version` 一直不 ready，子进程被有界终止，错误包含阶段而不破坏旧配置。
4. core 意外退出后记录退出原因和恢复次数；重试耗尽进入可见 failed，不无限重启。
5. 外部 core 热应用结果不确定时，ZenClash 不 restart/kill 外部进程。
6. 旧 WebSocket generation 的迟到帧不写入新会话；重连耗尽进入 stale/failed。

### 13.2 Profile 与配置事务

7. 远程 YAML 语法正确但最终合并配置 `mihomo -t` 失败，旧 source/active/runtime 保持不变。
8. 本地编辑失败与远程更新失败具有相同 last-known-good 语义。
9. 备用下载成功仍必须经过解析、最终组合、`-t` 与原子提交。
10. 解密/解析失败只删除 staging，不触碰 live 文件。
11. apply 明确拒绝、传输结果不确定、确认成功分别映射不同 outcome 和恢复动作。
12. 导入失败保留 URL/token/key，按钮退出 pending，字段附近显示可重试错误。

### 13.3 System Proxy、TUN 与四层状态

13. traffic WS 正常但 System Proxy/TUN 都 off，首页不得显示“已接管”。
14. System Proxy intent on、OS actual off、ownership lost 三种状态分别呈现。
15. ZenClash 开启 System Proxy 后被外部应用覆盖，退出时不覆盖外部新值。
16. System Proxy disable 成功、enable 失败时恢复旧 OS 状态或进入 reconcile-needed。
17. TUN configured on 但权限、设备或 route 失败，L3 显示 failed 而不是 enabled。
18. L1/L2 正常、L3 正常、L4 probe 失败时，界面明确“已接管但目标路径失败”。
19. 任一状态分片失败不遮住其余成功分片；stale 值包含 observed_at。

### 13.4 代理组、测速与连接

20. `hidden=true` 默认不出现，显式高级选项后出现。
21. Selector 选择后 readback `now`；不显示“恢复自动”。
22. URLTest/Fallback PUT 后显示 fixed；DELETE 后回读为自动。
23. LoadBalance 不显示伪造的唯一当前节点或普通选择动作。
24. 普通逐节点/provider 测速不清 fixed。
25. 显式“测速并恢复自动”调用 group delay 后回读 `fixed/now`，文案说明副作用。
26. 普通节点切换不发送 `DELETE /connections`；现有连接继续存在。
27. `RebuildAffected` 只关闭 chain 含旧节点的连接；`RebuildAll` 必须显式确认。
28. 选择成功、清连接失败显示“新连接已切换；旧连接未重建”，不谎报选择失败。

### 13.5 DNS、日志、诊断与支持包

29. DNS A/AAAA query 独立显示 status/answer/TTL 和错误，不拖垮首页。
30. DNS flush 与 fake-IP flush 是两个独立、可确认动作；不宣称既有连接已刷新。
31. Structured logs 保留 core time/fields；不支持时自动回退普通格式并标明时间来源。
32. logs/traffic/connections 流中断保留最后值和时间，不把数值归零冒充 fresh。
33. NetworkDiagnostics 的每个步骤独立成功/失败，并明确经 Mihomo 或 DIRECT。
34. Support bundle 不包含 Controller secret、订阅 query/token、节点凭据或未脱敏用户路径。

### 13.6 首次使用、空状态与可访问性

35. 无 Profile 首页提供可聚焦“导入订阅”和“选择本地 YAML”，不是空白或只读占位。
36. 首次流程在导入、授权或 probe 任一步退出后，重开从领域事实续接。
37. 仅用键盘可完成一级导航、Profile 导入、capture 开关、mode、代理选择和连接详情。
38. 焦点顺序与视觉顺序一致；对话框关闭后回到触发控件；任何焦点不被持久浮层完全遮挡。
39. 错误卡的主要恢复动作有 accessible name，不能依赖 hover、右键或颜色。
40. 流量上传/下载、Fresh/Stale/Failed 同时用文字/数值/图标或线型表达，不只靠颜色。

### 13.7 更新与高权限边界

41. 可执行文件缺少摘要/签名、摘要不匹配或计算失败时拒绝执行，旧版本仍可启动。
42. staging 替换失败或新版本启动失败时回滚；临时文件最终清理。
43. TUN/helper 提权只在用户触发相关能力时发生；GUI 不长期以管理员身份运行。
44. 外链只允许批准 scheme，复制错误和诊断输出默认脱敏。

## 14. 决策汇总

| 决策 | 结论 | 主要依据 |
|---|---|---|
| 是否重写现有 core 架构 | 否 | `CoreSession/SystemProxySession/ControlledConfigStore` 已是正确深 module 基础 |
| 是否追平竞品功能数量 | 否 | 功能宽度不能补偿状态虚假、恢复和安全缺口 |
| 首页主状态 | Capture/Path 为主，Process/Controller 可展开 | Mihomo 控制面与数据面分离 |
| 首次使用 | 状态驱动、可续接，不用长导览 | Clash Party tour 与 ClashMi 失败后跳转都不完整 |
| 自动组 | 完整建模 hidden/fixed/restore-auto | Mihomo 官方 API 与两款成熟实现 |
| 节点切换 | 默认保留现有连接 | 最小惊讶；ClashMi 与精确清理实践 |
| DNS 优先级 | 先诊断 query/flush，后全量表单 | 高频排障价值高于字段可视化 |
| 日志 | structured-first + fallback | 保留 core 时间与字段，提升可支持性 |
| JS 覆写 | 暂不支持 | 执行边界、资源限制、来源信任成本过高 |
| 应用更新 | 先通知与签名链接，不默认静默安装 | 用户控制权与供应链验证优先 |
| IA | 五个任务入口 | 保留可发现性，避免功能模块直接等于导航 |
| 测试策略 | interface 即行为测试表面，真实 Mihomo harness 优先 | Harness Engineering 与现有优势 |

## 15. 最终结论

四款客户端没有一款可以整体复制：成熟控制系统通常带来更宽的表层和更大的权限面；短主路径又常以压缩状态、隐藏核心实现或牺牲桌面交互为代价。

ZenClash 的最佳路线不是成为“第五个功能总台”，而是成为状态最诚实、恢复最明确、主路径最短的 Mihomo 桌面客户端：后端以深 module 拥有生命周期、配置事务和 OS ownership；UI 用少量任务 interface 展示 L1–L4、last-known-good 和局部恢复；严格遵守 hidden/fixed/restore-auto 与 endpoint 副作用；把 DNS、structured logs、Provider 和 probe 组织成诊断闭环；用行为测试证明失败状态。

如果只能选择一个近期成果，应选择“用户可以准确知道流量是否被接管、失败在哪一层、点击什么恢复”。这比增加任何一个高级页面都更能决定 ZenClash 是否合格、好用且简洁。

## 16. 核心一手资料索引

### Mihomo

- 版本与总览：[v1.19.30](https://github.com/MetaCubeX/mihomo/releases/tag/v1.19.30) · [README](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/README.md)
- 协议与配置：[RESTful API](https://wiki.metacubex.one/en/api/) · [General](https://wiki.metacubex.one/en/config/general/) · [Inbound](https://wiki.metacubex.one/en/config/inbound/) · [TUN](https://wiki.metacubex.one/en/config/inbound/tun/) · [Proxy groups](https://wiki.metacubex.one/en/config/proxy-groups/) · [DNS](https://wiki.metacubex.one/en/config/dns/)
- 源码：[`tunnel.go`](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/tunnel/tunnel.go) · [`executor.go`](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/hub/executor/executor.go) · [`server.go`](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/hub/route/server.go)

### Clients and ZenClash

- [Clash Verge Rev locked commit](https://github.com/clash-verge-rev/clash-verge-rev/commit/3503a2da29d68a4398c0b8e9234cffb711e65783)
- [Clash Party locked commit](https://github.com/mihomo-party-org/clash-party/commit/061faeefd15849d31062e78cbd4084bad7f0f497)
- [ClashMi locked commit](https://github.com/KaringX/clashmi/commit/917fd46085d71e8a1caa91f018681337283d5162)
- [FlClash locked commit](https://github.com/chen08209/FlClash/commit/62addf738a76b1a492e19af2dbabdb6d572b9e72)
- [ZenClash locked commit](https://github.com/HaiwenZhang/zenclash/commit/7415b25f4a3e70af9a50f690dd56bfc403cd808e)
