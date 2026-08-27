# ZenClash Mihomo 客户端开发计划

> 状态：In Progress
>
> 制订日期：2026-08-27
>
> 代码基线：`7415b25f4a3e70af9a50f690dd56bfc403cd808e`
>
> M0 记录：[行为基线](mihomo-client-m0-behavior-baseline.md)
>
> 验收映射：[44 项行为证据](mihomo-client-behavior-acceptance.md)
>
> 平台协议：[真实平台验收步骤与记录](mihomo-client-platform-acceptance.md)
>
> 当前实现：M1–M8 已完成代码与行为测试；M1–M7 已通过 Mihomo v1.19.30 真实进程验收。
> 首页从 Profile、L1–L4、capture 与路径探测事实派生首次使用步骤；普通接管统一经过
> `TrafficCaptureSession`。诊断、结构化日志、安全支持包、Provider 运维、TUN 平台证据、
> 可验证内核更新、只通知的应用更新以及五入口信息架构均已落地。macOS 已完成不提权的
> 实包路径验收；实际 TUN 授权及 macOS/Windows/Linux 完整平台矩阵仍是发布前验收项。
>
> 研究依据：[四款 Mihomo 客户端代码与设计比较](../research/mihomo-client-code-design-comparison.md)

## 1. 目标

本计划把竞品研究结论转化为 ZenClash 的可执行开发顺序。第一目标不是追平其他客户端的功能数量，而是交付一个满足以下承诺的 Mihomo 桌面客户端：

1. 状态真实：区分内核进程、Controller、流量接管和端到端路径；
2. 操作可恢复：配置、接管或系统操作失败时保留最后可用状态；
3. 副作用明确：代理切换、连接清理、TUN、System Proxy 和更新不会静默产生破坏性行为；
4. 主路径简短：用户能从导入 Profile 直接走到可验证的流量接管；
5. 行为可证明：高风险路径通过 module interface 和真实 Mihomo harness 验证。

P0 的产品完成标准是：用户可以准确知道流量是否被接管、失败在哪一层，以及应执行什么恢复动作。

## 2. 计划约束

### 2.1 保持不变

- 保留 Rust、GPUI、`zenclash-core` 和 `zenclash-ui` 的现有架构方向；
- 继续以 Mihomo 为默认正式内核；
- meow-rs 仅保留为显式启用的实验后端；
- 保留导入的订阅和 YAML 源文件，不原地注入 ZenClash 受控字段；
- 延续 [`CoreSession`](../../crates/zenclash-core/src/core_session.rs)、[`SystemProxySession`](../../crates/zenclash-core/src/system_proxy.rs) 和 [`ControlledConfigStore`](../../crates/zenclash-core/src/controlled_config.rs) 的深 module 方向；
- P0 不改变现有 Profile 持久化格式，因此不安排数据迁移；
- 默认不新增第三方依赖。

### 2.2 明确不做

- 不按竞品功能数量制定完成度；
- 不引入 JS/Lua 远程覆写、插件市场、多内核或第二套 Web dashboard；
- 不把所有 Mihomo 配置字段制作成表单；
- 不默认开放 LAN Controller；
- 不默认静默下载、安装或重启；
- 在状态真实性完成前，不继续增加首页卡片；
- P0 不开展自由 Dashboard、主题编辑器或复杂长期流量历史。

### 2.3 工期假设

下文估算以一名熟悉 Rust、Tokio 和 GPUI 的主要开发者为基准，不含签名证书、商店审核和难以复现的平台驱动问题。

- P0：约 7–10 周；
- P1：约 4–6 周；
- P2：约 3–4 周。

两名开发者可以并行推进 M1 和 M2；M4、M5 必须等待前置状态 interface 稳定。

## 3. 架构原则

本计划遵循以下 module 设计规则：

- module 的 interface 同时是调用面和行为测试面；
- 页面只拥有输入、筛选、展开、焦点和局部 pending 状态；
- 生命周期排序、配置事务、接管回滚和副作用进入深 module；
- 新 module 应提供 leverage 和 locality，而不是包装现有调用形成浅转发；
- 只有存在生产与测试、或多个平台实现时才建立 adapter seam；
- 迁移采用 replace-don't-layer：新 interface 验证完成后删除旧编排和重复测试；
- `Unknown`、`Stale` 和“结果不确定”是一等状态，不能强制折叠为成功或失败。

## 4. 开发顺序

```text
M0 行为基线
├─> M1 代理组语义
├─> M2 ProfileApplication
└─> M3 OperationalStatus
        └─> M4 TrafficCaptureSession
M1 + M2 + M3 + M4
        └─> M5 首页、首次使用、键盘路径
              └─> P0 发布候选

P0 发布候选
├─> M6 NetworkDiagnostics 与支持能力
└─> M7 平台真相与供应链
        └─> P1 发布候选

P1 发布候选
└─> M8 信息架构与 RuntimePage 收束
        └─> P2
```

不要先做首页视觉重构。底层仍只有单一 `connected` 或全成全败数据时，新首页只会把不准确状态包装得更漂亮。

## 5. P0：状态可信、操作不意外

| 里程碑 | 预计 | 主要交付 | 行为测试 |
|---|---:|---|---|
| M0 行为基线 | 2–3 天 | 验证基线、测试映射、平台矩阵 | 1–6 基线审计 |
| M1 代理组正确性 | 4–6 天 | `hidden/fixed/restore-auto`、默认不断连接 | 20–28 |
| M2 配置统一事务 | 8–12 天 | Profile 入口统一 stage/validate/apply/commit | 7–12 |
| M3 四层运行状态 | 7–10 天 | L1–L4、分片状态、流新鲜度 | 13–19、32 |
| M4 流量接管事务 | 8–12 天 | System Proxy/TUN 统一应用、回滚和 ownership | 14–18 |
| M5 首页与首次使用 | 6–9 天 | 状态驱动引导、空状态、键盘主路径 | 35–40 |

### 5.1 M0：建立行为基线

#### 工作内容

1. 在开始修改前运行 workspace 完整验证，记录现有失败；
2. 将研究报告第 13 节的 44 条行为测试映射到对应里程碑；
3. 每个里程碑只先添加该里程碑的失败测试，不一次性创建全部测试；
4. 真实进程行为继续进入 [`real_mihomo.rs`](../../crates/zenclash-core/tests/real_mihomo.rs)；
5. System Proxy、TUN 和退出恢复建立 macOS、Windows、Linux 手工验证矩阵；
6. 明确每项能力在 Mihomo 和 meow-rs 下的 capability。

#### 测试分层

- 纯状态转换：普通单元测试；
- 文件与 Profile 事务：临时目录和真实原子写入；
- Mihomo Controller：受控 HTTP adapter 与真实 Mihomo E2E；
- 平台接管：平台 adapter 测试加可重复手工验证；
- GPUI 交互：行为测试覆盖焦点、激活键和异步结果回写。

#### 完成门槛

- 基线命令结果已记录；
- 每个 P0 里程碑都有明确测试文件和验收负责人；
- 不以 mock 替代真实内核进程行为。

### 5.2 M1：代理组语义与连接策略

这是第一个代码里程碑：范围小、风险可控，并能立即消除用户可感知的错误副作用。

#### Module 设计

深化 [`ProxyOperations`](../../crates/zenclash-core/src/proxy_operations.rs)，建议 interface：

```rust
catalog(visibility) -> ProxyCatalog
select(group, member, connection_policy) -> ProxySelectionOutcome
restore_auto(group) -> ProxySelectionOutcome
measure(scope, semantics) -> ProxyMeasurementOutcome
```

Implementation 隐藏：

- Mihomo 组类型、`hidden/fixed/now/all` readback；
- PUT、DELETE 和 delay endpoint 的差异；
- 测速是否会清 fixed；
- `KeepExisting/RebuildAffected/RebuildAll` 的连接处理；
- 选择成功但 readback 或连接清理失败的结果分型。

#### 数据模型

在 [`proxy.rs`](../../crates/zenclash-core/src/proxy.rs) 中使用领域枚举表达组行为，避免继续堆叠布尔值：

```rust
enum ProxyGroupBehavior {
    Selector,
    Automatic { fixed: bool },
    LoadBalance,
    Unknown(String),
}
```

`hidden` 保留为可见性信息；未知组类型必须保留原始类型供诊断。

#### 实施切片

1. 解析 `fixed`、组类型和 hidden，补纯模型测试；
2. 新增 `ConnectionPolicy`，将默认行为改为 `KeepExisting`；
3. 新增恢复自动操作及 readback；
4. UI 默认过滤 hidden，自动组显示 fixed 和“恢复自动”；
5. LoadBalance 不显示普通手选动作；
6. 需要重建连接时使用明确动作和确认文案；
7. 后续实现 `RebuildAffected` 时，只关闭 chain 包含旧节点的连接。

#### 完成门槛

- 普通代理切换不再发送 `DELETE /connections`；
- URLTest/Fallback 的 fixed 状态可观察、可恢复；
- group delay 的 fixed 副作用有明确文案；
- Selector、自动组、LoadBalance 和未知组都有测试；
- 真实 Mihomo readback 行为通过。

#### 当前实现记录（2026-08-27）

- `ProxyOperations` 已拥有可见性过滤、普通选择、恢复自动、显式“测速并恢复自动”及
  `KeepExisting/RebuildAffected/RebuildAll` 连接策略；选择成功后的清理或回读失败以
  warning 返回，不把已经生效的操作误报为失败。
- 默认代理目录过滤 `hidden`，高级开关可显式加载；URLTest/Fallback 展示 auto/fixed
  状态并支持 DELETE 后 readback；LoadBalance 在代理页、首页和托盘均不提供普通手选。
- “全部测速”继续逐节点/provider 测量，不改变 fixed；独立的“测速并恢复自动”动作明确
  使用 `/group/:name/delay` 的清 fixed 副作用并在完成后回读。
- mock Controller 行为测试已覆盖请求顺序、readback、hidden、fixed、精准关闭旧 chain
  和部分成功 warning；真实 Mihomo v1.19.30 已通过 Selector 选择、自动组 fixed/
  restore-auto 及保留连接验证。
- 真实响应使用 `fixed: "DIRECT"` / `fixed: ""`，而不是旧 mock 的布尔值；模型现兼容
  boolean、成员名字符串与 null，并以成员名是否非空推导 fixed，不再因单字段编码差异丢弃
  整个 catalog。

### 5.3 M2：`ProfileApplication` 配置事务

当前 [`profiles/workflow.rs`](../../crates/zenclash-ui/src/pages/runtime/profiles/workflow.rs) 已实现可逆操作，但下载、持久化、激活、内核应用和回滚仍由 UI 编排。M2 的目标是把这些排序收回 `zenclash-core`。

#### Module 设计

新增 concrete module `ProfileApplication`。P0 外部 interface 先控制在一个主要任务入口：

```rust
apply(change) -> ProfileApplyOutcome
```

现有配置预览能力可以继续复用；只有出现“预览后必须应用完全相同候选”的明确需求时，才增加由 opaque prepared token 连接的 prepare/apply interface，避免重复下载造成 TOCTOU。

Implementation 负责：

```text
读取/下载
-> staging
-> 解析并合并 overrides/controlled config
-> mihomo -t
-> apply/readback
-> commit source、active 和 runtime version

任一步失败
-> 保留 source、active、runtime
-> 清理 staging
-> 返回可恢复的 typed outcome
```

Outcome 至少区分：

- `Rejected { last_known_good }`；
- `Applied { source_version, runtime_version }`；
- `PersistedButRuntimeUnknown { recovery }`；
- `RolledBack { cause }`。

#### Seam 与 adapter

- ProfileStore 和临时文件属于 local-substitutable 依赖，使用真实临时目录测试；
- Mihomo 验证与应用属于 true external 依赖，在 module 内设置 internal seam；
- 生产使用 Mihomo adapter，测试使用受控 adapter；
- 不为了测试把内部 `stage/validate/commit` 暴露为公共 interface；
- core 返回 typed error，用户可见文案在 i18n 层映射。

#### 实施切片

1. 先把现有 workflow 迁入 module，保持行为不变；
2. 将现有 workflow 测试迁移到新 interface；
3. 引入 staging candidate，最终候选通过 `mihomo -t` 后才能提交；
4. 统一本地导入、远程新增/更新、手工编辑和 Profile 切换；
5. 接入 overrides 与受控设置；
6. 删除 UI 旧排序、回滚逻辑和重复测试。

#### 完成门槛

- 所有 Profile 入口遵守同一事务；
- 下载备用通道不能绕过最终验证；
- 解密、解析或验证失败只删除 staging；
- source、active、runtime 的 last-known-good 行为有测试；
- P0 不改变现有持久化格式。

#### 当前实现记录（2026-08-27）

- `zenclash-core::ProfileApplication` 以单一 `apply(ProfileChange)` 统一 Profile 切换、本地
  导入、远程新增/更新和 YAML 编辑；inactive 更新以 `Stored` 表示，不伪造 runtime generation。
- 候选源先进入私有 staging；controlled config 与有序 overrides 合并后的最终 YAML 通过
  目标内核 validator 后才应用。运行时令牌在 source/active 提交完成前持有 CoreSession 与
  controlled mutation gate，仓库提交失败会恢复旧缓存和旧运行时。
- `Applied/Rejected/RolledBack/RuntimeUnknown/PersistedButRuntimeUnknown` 区分明确拒绝、
  已恢复失败与传输结果不确定；成功同时返回 source/runtime version。
- UI 已删除远程更新和 YAML 编辑中的下载端口判断、reload、rollback 排序，所有 Profile
  写入口改由 core 事务负责；未改变 `profiles.json` 与托管 YAML 的持久化格式。
- 临时 ProfileStore、受控 Controller 和目标 validator 测试已覆盖：运行时接受前 active
  不可见、最终合并验证失败、远程/本地/编辑 LKG、提交竞争后的运行时恢复、fallback 下载
  后仍走最终验证，以及传输中断映射为 runtime unknown。
- 真实 Mihomo harness 已通过 `ProfileApplication` 执行本地导入与远程新增；M2 的
  mock/validator 与 Mihomo v1.19.30 真实进程事务验收均已通过。

### 5.4 M3：`OperationalStatus` 与首页分片

#### Module 设计

新增 concrete module `OperationalStatus`：

```rust
snapshot() -> OperationalSnapshot
subscribe() -> OperationalStatusStream
```

它是 in-process 聚合 module；只有一个聚合 implementation 时不创建 public trait。测试通过 internal seam 注入受控观察结果。

Snapshot 聚合：

| 层 | 内容 |
|---|---|
| L1 Process | 托管进程、外部内核、退出原因、恢复状态 |
| L2 Controller | `/version`、认证、协议兼容和 generation |
| L3 Capture | System Proxy intent/actual/ownership；TUN requested/configured/observed |
| L4 Path | 最后一次明确走 Mihomo 或 DIRECT 的路径探测 |
| Streams | traffic/logs/connections 的 generation、最后成功时间和新鲜度 |

所有分片使用统一观察状态：

```text
Loading -> 首次读取
Fresh   -> 当前成功值 + observed_at
Stale   -> 上次成功值 + observed_at + failure
Failed  -> 无可信值 + failure + recovery action
```

#### 实施切片

1. 为 [`SystemProxySession`](../../crates/zenclash-core/src/system_proxy.rs) 增加只读 snapshot，统一 intent/actual/ownership；
2. 建立 `OperationalSnapshot` 和分片 observation 类型；
3. 加入 CoreSession、Controller、System Proxy 和 stream freshness；
4. 将 [`load_dashboard`](../../crates/zenclash-ui/src/pages/runtime/loader.rs) 从全成全败结果改为独立分片；
5. 旧 generation 的异步结果和 WebSocket 帧不得污染新会话；
6. 首页先接入状态数据，不在此里程碑完成最终视觉重构。

#### 完成门槛

- traffic WebSocket 正常但没有 capture 时，不显示“已接管”；
- Controller 失败不清空 Profile、System Proxy 和最后成功连接数据；
- stale 值保留时间，不归零冒充 fresh；
- System Proxy intent、actual 和 ownership lost 分别呈现；
- meow-rs 不支持的状态显示 unsupported/unknown，而不是伪造 Mihomo 结果。

#### 当前实现记录（2026-08-27）

- `zenclash-core::OperationalStatus` 以 `snapshot/subscribe` 聚合 L1 process、L2 controller、
  L3 System Proxy/TUN、L4 path 占位与 traffic/logs/connections freshness；统一
  `Loading/Fresh/Stale/Failed` observation 会保留最后成功值和 `observed_at_ms`。
- `SystemProxySession::snapshot` 在共享 native operation lock 下只读聚合持久化 intent、OS
  actual 与 `Owned/Unowned/Lost`；首页开关绑定 intent，状态文案分别呈现 actual off、外部
  启用、ownership lost 与不可用，不再用 traffic WebSocket 推断接管。
- 首页 config、proxy catalog 与 connections 已改为并行独立分片；失败分片合并为 stale，
  Controller 或 connections 失败不会清空 Profile、System Proxy 或其他成功数据。
- traffic 断线保留最后速率和时间；traffic/log monitor 接受 CoreSession generation 并在
  切代时重连，旧 socket 的迟到帧会在写 snapshot/buffer 前被拒绝；connections 异步响应也
  以 generation 校验后才进入 snapshot。
- Mihomo TUN configured-on 只报告 `Unknown`，不会伪造 Active；meow-rs 明确报告
  `Unsupported`。真实 Mihomo v1.19.30 harness 已验证 controller/traffic fresh 且无 capture
  时 `CaptureStatus::is_active()` 为 false。
- `CoreSession` 现在只启动一个托管内核 supervisor：异常退出先记录安全 exit status、释放仍由
  ZenClash 拥有的原生接管，再以有界次数等待 `/version` 恢复；成功后递增 generation 并按
  持久 intent reconcile，耗尽后进入可见 Failed 并停止自动重试，但 supervisor 保持观察；
  用户手动恢复后下一次崩溃仍受监管。quit 会在等待 transition lock 前发布取消标志，正在启动
  的未确认子进程会被停止回收，shutdown 后不会被迟到任务复活。
- 生命周期行为测试覆盖并发重启不重叠、启动期间 quit、readiness 超时回收、退出原因/恢复次数、
  捕获 release→reconcile 排序与外部 core 永不获得 restart/kill 所有权；首页用文字显示
  Recovering/Failed/Recovered、尝试次数和安全退出原因，不只依赖颜色。macOS 签名实包已验证
  直属 Mihomo 被终止后生成新 PID、L1/L2 收敛，正常退出同步回收应用与恢复后的 core。

### 5.5 M4：`TrafficCaptureSession`

#### Module 设计

建议 interface：

```rust
apply(plan) -> CaptureOutcome
reconcile() -> CaptureSnapshot
release_owned() -> CaptureOutcome
```

普通 UI 使用以下计划：

```rust
enum CapturePlan {
    Off,
    SystemProxy,
    Tun,
}
```

如果启动时发现现有 System Proxy 与 TUN 组合，不得静默标准化；应显示为高级组合并保持现状，直到用户主动选择新的 plan。

#### Implementation 隐藏

- TUN 权限检查和请求；
- controlled config patch 与 CoreSession apply；
- 内核 readiness；
- System Proxy native readback 与 ownership；
- 部分成功后的回滚或 `reconcile-needed`；
- 应用退出时只释放 ZenClash 仍拥有的系统状态。

平台接管存在多个生产 adapter 和测试 fake，因此 platform capture 是真实 seam。

#### 调用方迁移

以下入口必须依次迁移到同一 module：

1. 首页；
2. System Proxy 页面；
3. TUN 页面；
4. 系统托盘；
5. 启动恢复与应用退出。

迁移完成后删除页面直接调用 `SystemProxyController`、TUN patch 和维护重启的旧路径。

#### 完成门槛

- TUN configured on 但权限、设备或 route 不明确时不能显示 Active；
- System Proxy 部分失败会恢复旧 OS 状态或进入 reconcile-needed；
- 外部应用覆盖 System Proxy 后，ZenClash 退出不覆盖外部新值；
- GUI 不长期以管理员权限运行；
- 正常退出、core 崩溃和重启路径都有恢复测试。

#### 当前实现记录（2026-08-27）

- `zenclash-core::TrafficCaptureSession` 已统一 `Off/SystemProxy/Tun` 三种普通计划，串行执行
  TUN permission、controlled-config/CoreSession apply、System Proxy write/readback 与失败回滚；
  结果明确区分 `Applied/Unchanged/RolledBack/ReconcileNeeded`，授权失败发生在任何接管写入前。
- 首页、System Proxy 页面、TUN 页面、系统托盘、启动恢复和应用退出均已迁移到同一 session；
  页面不再直接编排 System Proxy Controller、TUN enable patch 或授权后的内核重启。
- 启动发现 System Proxy + TUN 时保留 `Advanced` 组合；普通 plan 只有在 intent、actual
  与 ownership 同时匹配时才视为完成。关闭与退出只释放仍匹配 ZenClash ownership 的原生
  状态，第三方覆盖不会被清除或被启动 reconcile 静默覆盖。
- TUN configured-on 但设备、路由或路径未验证时仍为 `Unknown`；首次 Unix 授权成功后由
  session 重启受管内核，再应用 TUN 配置。正常退出、外部覆盖、部分失败/回滚失败、core
  崩溃与恢复均已有 adapter 行为测试。
- workspace 自动化门禁通过；macOS/Windows/Linux 的真实 System Proxy、TUN 与退出矩阵仍
  属于发布前平台验收。M7 已删除 Windows 整 GUI 的 RunAs 路径；安全 helper 可用前明确
  capability unavailable，不以扩大管理员边界伪装支持。

### 5.6 M5：首页、首次使用与键盘主路径

#### 首页信息层级

首页只回答五个问题：

1. 当前使用哪个 Profile，最后成功更新时间是什么；
2. 内核和 Controller 是否可控；
3. 当前接管方式是什么，实际是否生效；
4. 当前 mode 与关键代理组是什么，自动组是否 fixed；
5. 是否有需要用户处理的问题，最直接的恢复动作是什么。

#### 首次使用状态

不创建独立 `wizard_step`，从领域事实派生：

```text
NoProfile
-> CandidateInvalid / CandidateReady
-> CoreReady
-> CaptureNotSelected
-> PermissionNeeded / CaptureActual
-> PathUnknown / PathPassed / PathFailed
-> Ready
```

用户退出后再次进入，从真实状态继续，不重播长导览。

#### 实施切片

1. 无 Profile 时提供“导入订阅”和“选择本地 YAML”；
2. 验证失败时说明旧配置仍在使用，并保留输入；
3. 首页接入 L1–L4、Profile、mode、关键组和局部错误；
4. 以局部 pending/error 替换首页相关的全局 `mutating`；
5. Sidebar 使用可聚焦的语义控件；
6. Enter/Space 可执行主操作；
7. 对话框关闭后焦点回到触发控件；
8. 状态和图表使用文字、数值、图标或线型，不只依赖颜色。

#### 完成门槛

- 仅用键盘可完成导入、capture、mode、代理选择和连接详情；
- 所有空状态都有可聚焦主动作；
- 持续故障停留在所属卡片，不只显示 toast；
- L1/L2 正常、L3 正常、L4 失败时明确显示“已接管但路径失败”；
- 首页不展示无法由当前证据支持的绿色总状态。

#### 当前实现记录（2026-08-27）

- `OperationalSnapshot::first_run_stage` 从活动 Profile、L1 process、L2 controller、L3
  capture 与 L4 显式路径观测推导下一步，不持久化 `wizard_step`；路径结果带 CoreSession
  generation，旧会话的迟到探测不能推进新会话状态。
- 首页新增证据条和可恢复步骤卡：无 Profile 时提供“导入订阅”和“选择本地 YAML”，随后按
  core、capture 实际状态和经 Mihomo 路径探测逐步推进；capture 失败保留在步骤/控制卡中，
  代理切换失败保留在节点卡中，不以绿色总状态掩盖 L4 未验证或失败。
- 本地 YAML 文件选择沿用 `ProfileApplication`，候选失败不会替换 last-known-good；选择器
  取消或关闭后恢复触发按钮焦点。在线订阅入口展开并聚焦已有 URL 输入，失败时不清空输入。
- Sidebar 已由鼠标点击容器替换为 gpui-component `Button`，按视觉顺序参与 Tab 导航并支持
  Enter/Space；导入、capture、mode、代理选择和连接详情均使用原生可聚焦控件，连接关闭也有
  可见文本标签。
- 首页流量状态、上传/下载图例、当前数值和 stale 时间均有文字，不只依赖颜色；界面继续使用
  语义主题 token。macOS 实包已手工验证 Tab 焦点环、Tab+Enter 侧栏导航，以及文件选择器
  Escape 后焦点恢复。
- 单元/行为测试覆盖首次使用事实恢复、路径代际拒绝、显式探测成功条件与双语文案；workspace
  全量测试、严格 clippy 和 Mihomo v1.19.30 真实 harness 均通过。

## 6. P0 发布门槛

### 6.1 行为门槛

- 研究报告中的测试 1–28、32、35–40 全部覆盖；
- Controller 正常但未接管时，界面不得显示“已连接”；
- 所有配置入口保留 last-known-good；
- 普通代理切换默认不破坏现有连接；
- System Proxy 只恢复和释放仍由 ZenClash 拥有的状态；
- TUN 未确认设备、路由或路径时显示 Unknown/Failed；
- 首页局部失败不遮挡其他成功分片。

### 6.2 工程门槛

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

真实内核行为还需：

```bash
ZENCLASH_MIHOMO_BINARY=/path/to/mihomo \
  cargo test -p zenclash-core --test real_mihomo -- --ignored
```

所有新增用户可见文案必须同步更新中文和英文 locale。

### 6.3 平台门槛

| 场景 | macOS | Windows | Linux |
|---|---|---|---|
| System Proxy enable/readback/release | 必测 | 必测 | 必测 |
| ownership 被外部覆盖 | 必测 | 必测 | 必测 |
| TUN 权限拒绝/授权/重启 | 必测 | 必测 | 必测 |
| core 崩溃与应用退出 | 必测 | 必测 | 必测 |
| 路径探测明确经 Mihomo | 必测 | 必测 | 必测 |
| 安装包升级回归 | 发布前 | 发布前 | 发布前 |

无法自动化的平台行为必须记录可重复步骤、环境和预期结果。
具体执行顺序、原生 readback、清理步骤和当前状态见
[真实平台验收协议](mihomo-client-platform-acceptance.md)。Windows 在安全 helper 落地前的
发布预期是明确 Unsupported 且不提升整个 GUI，不能以管理员运行应用代替授权分支。

## 7. P1：诊断与可支持性

### 7.1 M6：`NetworkDiagnostics`

建议 interface：

```rust
run(plan) -> DiagnosticReport
export(report, SupportSafe) -> SupportBundle
```

Implementation 复用现有 [`NetworkProbeService`](../../crates/zenclash-core/src/network/probe.rs)，并加入：

1. DNS A/AAAA query；
2. DNS cache flush 与 fake-IP flush；
3. 经 Mihomo 与 DIRECT 的明确对照；
4. Controller/capture/path 分层；
5. Provider update/healthcheck 状态；
6. 每一步的独立成功、失败、时间和 route；
7. 默认脱敏支持包。

支持包不得包含 Controller secret、订阅 token/query、节点凭据、未脱敏用户路径或完整敏感日志字段。

### 7.2 Structured logs

- 优先请求 `format=structured`；
- 保存 core time、level、message 和 fields；
- 不兼容内核回退普通格式，并标注时间来源；
- 日志流中断时保留最后数据和 observed_at；
- 提供字段级筛选与安全复制。

### 7.3 Provider 运维

- 展示最后成功、最后失败、数量和当前 last-known-good；
- 手动 update 与 healthcheck 独立反馈；
- 更新失败不得清空上次成功 Provider 数据；
- 首页只显示需要处理的摘要，完整能力位于 Diagnostics。

### 7.4 M6 当前实现记录（2026-08-27）

- `zenclash-core::NetworkDiagnostics` 以单一 `run(DiagnosticPlan)` 并发执行 Controller、
  capture、DNS A、DNS AAAA、DIRECT、Mihomo、代理 Provider 和规则 Provider 八个稳定步骤；
  每一步独立保留 kind、route、完成时间、耗时和 typed 成功/失败，单步失败不会中断其余步骤。
- Controller client 已增加 typed DNS A/AAAA 查询、DNS cache flush、fake-IP flush 和代理
  Provider healthcheck。网络页始终对照 DIRECT 与 Mihomo 路径，分别展示 DNS 答案/TTL、
  capture、Provider 数量和失败位置；两类缓存清理使用独立二次确认，并明确不会刷新已有连接。
- `export(report, SupportSafe)` 使用严格 allow-list 生成脱敏 JSON，只保留步骤状态、路由、数量、
  耗时和延迟；Controller endpoint/secret、DNS 名称与答案、公网 IP、探测目标、Provider 名称/
  URL 和原始错误均不会进入支持包。
- 日志流优先请求 Mihomo `format=structured`，保留 core time、level、message 和原始 fields；
  不支持时回退普通格式并标注本地接收时间来源。中断会保留最后数据与 observed time，UI 支持
  message/字段筛选和不含正文、字段、core time 的“安全复制”。真实 Mihomo v1.19.30 返回的
  `time` 是本地 `HH:mm:ss`、`fields` 是数组；解析器将时间锚定到最接近接收时刻的本地日期，
  跨午夜也不会跳错一天，并按任意 JSON 值保留 fields，而不误设为 object。
- `ProviderOperations` 分别保留 update/healthcheck 的成功与失败历史、item count 和
  last-known-good；失败更新不清空旧 Provider。资源页提供独立健康检查/更新动作，并把
  Mihomo 的零时间哨兵显示为未知。页面普通刷新也统一经过 catalog observation，避免丢失
  初始运维状态。
- 行为测试覆盖八步独立状态、支持包 allow-list、结构化/普通日志与中断、Provider LKG 和独立
  操作状态。真实 Mihomo harness 已通过结构化日志、A/AAAA 查询和两类缓存清理；macOS 实包已
  手工验证诊断八步、结构化日志、安全复制入口和 Provider 操作状态。Controller endpoint 事实
  依据 Mihomo 官方 [API 文档](https://wiki.metacubex.one/en/api/)。

### 7.5 M7：平台真相与供应链

- 补足各平台 TUN permission/device/route observed state；
- 验证 helper 调用方身份、ACL 或强 secret；
- 所有可执行下载要求摘要或签名；
- 摘要缺失、计算失败或不匹配时拒绝执行；
- 新版本启动失败时保留旧版本；
- App 更新先实现通知、release notes 和官方签名下载入口。

P1 对应研究行为测试 29–31、33–34、41–44；测试 32 已在 M3 完成。

### 7.6 M7 当前实现记录（2026-08-27）

- `TunRuntimeObserver` 按平台读取 Mihomo 回报的设备名和代表性 IPv4 route：macOS 只接受
  `utun*` 并使用 `ifconfig`/`route -n get`，Linux 使用 `ip link`/`ip route get`，Windows
  使用 `Get-NetAdapter`/`Find-NetRoute`。设备名为空时保持 `Unknown`，不会把其他 VPN 的
  虚拟网卡归给 Mihomo。`TunCaptureStatus` 只有在 permission、device、route 三项均为
  `Active` 时才聚合为 Active；页面同时展示三项与系统证据。命令选择依据 Mihomo
  [TUN 文档](https://wiki.metacubex.one/en/config/inbound/tun/)及对应平台原生工具。
- TUN 授权只可从显式 `CapturePlan::Tun` 进入；reconcile、启动读回、System Proxy、Off 和
  退出释放都不会弹出授权。macOS/Linux 的一次性系统授权同时绑定 canonical core 名称、
  路径和授权前后 SHA-256。Windows 的旧 RunAs 整 GUI 重启路径已删除；具备调用方 ACL 的
  按需 helper 出现前返回明确 Unsupported，因此当前没有未认证的高权限 IPC 面。
- Mihomo 在线更新只选择 MetaCubeX 官方 Release 中带 GitHub `sha256:` digest 的平台资产；
  下载大小、scheme、凭据、redirect host、摘要、压缩包内容、候选 `-v`、候选配置 `-t` 和
  重启后 `/version` 均受验证。staging 与 backup 位于目标同目录，启用或新版本启动失败时
  自动恢复旧 core，drop/失败路径清理 staging、backup 和 rejected 候选。官方 Release 目录
  与真实当前归档已实际下载验证，旧 core 在准备阶段持续运行。
- `AppUpdateService` 只访问固定 `api.github.com/HaiwenZhang/zenclash/releases/latest`，限制
  元数据大小、redirect 最终地址和官方 tag 页面；只展示稳定版通知与有界 release notes，
  交给操作系统打开无凭据 HTTPS 官方页面，不下载、安装或重启应用。当前官方仓库无稳定
  Release 时展示稳定空状态。发布 workflow 生成 `SHA256SUMS` 并为公开仓库产物生成 GitHub
  build attestation。
- 行为测试覆盖缺失/错误摘要拒绝、下载 URL 策略、候选回滚和临时文件清理、权限仅显式触发、
  外链 scheme/credentials/host/path、应用更新通知/空状态、支持包与日志安全复制。macOS 实包
  已验证 TUN intent 与 permission/device/route 未生效可同时辨识，以及应用更新空状态；实际
  授权后的设备/route、System Proxy 与退出恢复继续由发布前平台矩阵验收。

## 8. P2：产品表层和维护成本收束

### 8.1 信息架构

一级导航收敛为：

| 一级入口 | 内容 |
|---|---|
| 首页 | Profile、四层状态、mode、关键组、错误与实时摘要 |
| 代理 | 代理组、fixed/auto、测速和节点详情 |
| 配置 | Profile、订阅、更新、验证和少量结构化覆写 |
| 连接 | 活动/最近连接、route explanation 和显式关闭 |
| 设置 | General、Capture、Diagnostics、Advanced、About |

Rules、Traffic、Logs、DNS、Network、Resources、Core、Override 和 Sniffer 不删除，但进入 Diagnostics/Advanced，并从相关任务提供上下文入口。

### 8.2 `RuntimePage` 收束

当前 [`RuntimePage`](../../crates/zenclash-ui/src/pages/runtime.rs) 同时持有生命周期、数据缓存、表单和全局操作状态。迁移遵循按操作域替换，不进行大爆炸重写：

1. Profile 状态归 `ProfileApplication` 调用模型；
2. capture 状态归 `TrafficCaptureSession`；
3. 首页只读 `OperationalSnapshot`；
4. proxy 状态归深化后的 `ProxyOperations`；
5. diagnostics 状态归 `NetworkDiagnostics`；
6. 页面仅保留 route、输入、筛选、展开、焦点和局部 pending。

删除测试用于判断 module depth：删除 module 后，如果排序、回滚和错误分类重新散落到多个调用方，module 提供了 leverage；如果调用方几乎不变，它只是 pass-through，应合并。

### 8.3 视觉与更新

- 流量图增加直接标签、当前值、更新时间和 stale 状态；
- 上传、下载不只靠颜色区分；
- 应用更新先保持用户确认，不默认静默安装；
- 自动安装需要独立的签名、替换失败回滚和权限设计，不并入当前计划。

### 8.4 M8 当前实现记录（2026-08-27）

- 一级导航已严格收敛为首页、代理组、订阅管理、连接和应用设置五项，并有行为测试锁定顺序；
  Rules、Traffic、Logs、DNS、Network、Resources、Mihomo、Override、Sniffer 与两种 capture
  细节页都保留实现，通过应用设置中的“配置处理 / 诊断维护 / 代理接管”上下文入口进入。
- 首页只读取 `OperationalStatus` 的 L1–L4/capture/stream 事实作为运行证据；Profile 应用、
  capture 事务、代理操作和 diagnostics 分别由 `ProfileApplication`、`TrafficCaptureSession`、
  `ProxyOperations` 和 `NetworkDiagnostics` 保持排序与错误分类。`RuntimePage` 内剩余首页、
  connection、logs、rules、network、traffic、provider、backup/update 等状态按页面域聚合，
  输入、筛选、展开、确认、pending 与导航 generation 不再散成同名顶层字段。
- 实时流量图直接标注上传/下载名称和当前值，并显示 Fresh/Stale/Failed 文本状态与上次更新时间；
  历史流量图新增独立 loading/fresh/stale/failed 及更新时间，在刷新失败时保留上次成功趋势，
  两条序列同时使用文字与数值区分，不只依赖颜色。
- 应用更新保持用户确认：Settings 展示当前版本、稳定版状态和 release notes，只允许打开经过
  host/path 验证的官方 HTTPS tag 页面；自动安装仍明确不在当前计划内。
- M8 完成后 workspace 工程门禁、Mihomo v1.19.30 真实 harness 与 macOS 签名实包构建均已
  通过。macOS 实包已手工验证五入口侧栏、Settings 上下文工具入口、历史流量 Fresh 文案、
  更新时间与双序列图例；行为测试继续锁定入口顺序和失败后保留 stale 趋势。

## 9. PR 与提交策略

每个里程碑拆为可独立审查的行为切片：

1. 先提交失败行为测试；
2. 再提交最小实现；
3. 接入一个调用方并验证；
4. 逐个迁移剩余调用方；
5. 删除旧路径和重复测试；
6. 最后更新 README、i18n 和专项文档。

单个 PR 不同时跨越以下多个高风险域：

- Profile 持久化；
- Core 生命周期；
- System Proxy；
- TUN/权限；
- 应用更新。

如果一个改动必须跨越两个以上高风险域，应先写清失败恢复和退出路径，再进入实现。

## 10. 风险与控制

| 风险 | 控制措施 |
|---|---|
| 状态类型膨胀 | 统一 Observation；Unknown/Stale 一等化，不为页面再造布尔状态 |
| RuntimePage 大爆炸重构 | 按操作域迁移，一个调用路径稳定后再删除旧路径 |
| Profile 事务破坏 live | staging + final candidate validation + 原子 commit + rollback test |
| TUN 跨平台差异 | capability gating、平台 adapter 和发布前矩阵 |
| System Proxy 覆盖外部设置 | actual/ownership readback；只释放仍 owned 的状态 |
| meow-rs 冒充 Mihomo 能力 | capability 明示 unsupported，不静默降级 |
| 新旧 workflow 长期共存 | replace-don't-layer，里程碑完成门槛包含删除旧路径 |
| 功能范围继续膨胀 | P0/P1/P2 固定范围；暂不投入项需要单独决策才能进入 |

## 11. Definition of Done

一个里程碑只有同时满足以下条件才能标记完成：

- interface、排序约束、错误模式和副作用有文档；
- 对应行为测试先失败、后通过；
- 实现没有绕过既有 core module 或形成第二条编排路径；
- 所有用户可见文案中英文同步；
- 相关单元测试、真实 Mihomo 测试和平台验证完成；
- 失败恢复、应用退出和 core 重启路径均有结论；
- 没有提交订阅地址、Controller secret、用户路径或日志隐私数据；
- 旧实现、重复测试和本次产生的冗余已经删除；
- 产品行为变化已经更新 README 或专项文档。

## 12. 首个执行批次

建议第一个开发批次只包含 M0 和 M1：

1. 固定测试基线；
2. 增加代理组类型、hidden、fixed 解析测试；
3. 给 `ProxyOperations::select` 增加连接策略；
4. 将默认策略改为 `KeepExisting`；
5. 增加 `restore_auto` 与 readback；
6. 更新代理页和首页关键组摘要；
7. 使用真实 Mihomo 验证选择、恢复自动和现有连接保留。

这个批次不改 Profile、System Proxy、TUN 或导航，因此影响面最小，也最适合作为后续深 module 迁移的行为测试样板。
