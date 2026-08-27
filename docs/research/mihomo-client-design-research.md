# Mihomo 原理与简洁桌面客户端设计研究

> 调研日期：2026-08-27
>
> Mihomo 基线：`v1.19.30`（release commit `ac017cd`，也是 ZenClash 当前固定的正式内核版本）
>
> 适用对象：ZenClash 产品与工程设计
>
> 证据范围：Mihomo 官方文档、MetaCubeX/mihomo 官方仓库及源码；竞品部分仅引用各客户端官方仓库。动态文档均以调研日期页面为准。

## 结论摘要

1. **Mihomo 是规则驱动的网络流量处理内核，不是完整桌面应用。** 它提供入站监听、TUN、DNS、嗅探、规则路由、代理组、provider、连接统计和 RESTful 控制面；桌面客户端的核心职责是把内核生命周期、配置、操作系统流量接管和故障恢复组织成可信状态，而不是把全部 YAML 字段平铺成表单。这个定位同时得到官方功能说明和核心数据路径源码支持。[Mihomo README](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/README.md) · [核心数据路径 `tunnel/tunnel.go`](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/tunnel/tunnel.go)
2. **“控制器正常”不等于“流量已接管”。** `/traffic` WebSocket 只证明客户端能收到内核统计流；用户流量还必须先经过系统代理、TUN、显式 HTTP/SOCKS 配置或透明代理等入站路径。系统代理和 TUN 是流量接管层，外部控制器是控制与观测层，两者必须分别建模。[入站配置](https://wiki.metacubex.one/en/config/inbound/) · [TUN 配置](https://wiki.metacubex.one/en/config/inbound/tun/) · [RESTful API：`/traffic`](https://wiki.metacubex.one/en/api/)
3. **简洁客户端首页只需回答五个问题：** 当前使用哪个配置？内核是否可控？系统代理/TUN 是否实际生效？当前模式与关键代理组选择是什么？是否存在需要用户处理的错误？DNS、嗅探、provider、规则详情和 YAML 增强适合留在高级或诊断入口。
4. **ZenClash 现有方向是可靠的。** `CoreSession` 串行化配置切换、托管/外部内核边界、系统代理 intent/actual/ownership 三态、受控配置与回滚，都比继续堆叠可视化配置项更有产品价值。近期优先级应是补足“状态真实性”和代理组语义，而不是扩大功能面。
5. **最值得优先修正的交互是自动代理组。** Mihomo API 已公开 `hidden`、`fixed`，并允许用 `DELETE /proxies/:name` 清除 URLTest/Fallback 的固定选择。当前 ZenClash 虽解析 `hidden`，仍默认展示；也未建模 `fixed`。这会让用户看见本应隐藏的内部组，并在点击自动组节点时不知不觉把自动策略固定住。[RESTful API：Proxies 与 Proxy Groups](https://wiki.metacubex.one/en/api/)
6. **当前首页和 UI 状态所有权仍需收束。** 首页加载是“全成或全败”，任一数据源失败都可能遮住其他可用状态；`RuntimePage` 同时持有运行时、持久化、各页面表单和全局操作标记。下一阶段应让独立状态分片可降级，并把运行事实聚合到小接口的深模块中，而不是继续让页面协调底层依赖。

## 1. 研究边界与证据分级

本文使用两类明确标记的信息：

- **来源事实**：由 Mihomo 官方文档、`v1.19.30` 源码或客户端官方仓库直接支持。
- **设计推断**：根据来源事实、ZenClash 当前实现和桌面产品目标得出的建议；它不是 Mihomo 规范。

竞品官方仓库只能证明“该客户端公开声称或实现了某能力”，不能证明 Mihomo 要求所有客户端都这样做。本文不使用第三方评测、教程、论坛帖或聚合文档作为事实来源。

## 2. Mihomo 的定位与数据流

### 2.1 定位

**来源事实。** Mihomo 提供本地 HTTP/HTTPS/SOCKS 服务、内置 DNS（含 DoH/DoT 与 fake-IP）、规则路由、自动选择/故障转移/负载均衡代理组、远程 provider、透明代理以及 RESTful API。官方将这些能力描述为内核特性，而不包含桌面托盘、系统代理写入、订阅管理或升级界面等 GUI 职责。[Mihomo `v1.19.30` README](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/README.md)

**设计推断。** 一个桌面客户端应被设计成“内核编排器 + 操作系统集成层 + 可解释控制面”，而不是 YAML 编辑器的图形外壳：

- 内核负责转发与路由语义；
- 客户端负责进程、配置来源、系统代理/TUN 权限、持久化意图、状态对账和用户反馈；
- UI 只暴露高频且可安全解释的能力，完整配置继续保留 YAML/覆写入口。

### 2.2 数据面与控制面

```text
应用流量
  │
  ├─ 系统代理 ────────> HTTP / mixed / SOCKS 入站
  ├─ TUN 路由与 DNS 劫持 ─> TUN 入站
  └─ 应用显式配置 / redirect / TProxy / listener
                             │
                             ▼
           入站元数据归一化、fake-IP 反查、可选嗅探
                             │
                             ▼
                Direct / Global / Rule 模式
                             │
                 Rule 模式按配置自上而下匹配
                             │
                             ▼
              目标代理组或出站代理 / DIRECT
                             │
            Selector / URLTest / Fallback / LoadBalance
                             │
                             ▼
                       建立出站连接
                             │
                    连接、流量、日志统计

桌面客户端 ──配置/控制/查询──> external-controller REST / WebSocket
桌面客户端 ──操作系统调用────> 系统代理、TUN 权限与路由、进程生命周期
```

**来源事实。** `tunnel/tunnel.go` 中的 TCP/UDP 处理会对 metadata 做预处理，反查 fake-IP，按配置决定是否嗅探，再依据 Direct、Global 或 Rule 模式选路；Rule 模式遍历规则得到目标适配器，最后建立出站并挂接统计 tracker。[核心数据路径](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/tunnel/tunnel.go)

**来源事实。** 配置应用并不是单一字段赋值：`executor.ApplyConfig` 在互斥保护下暂停 tunnel，依次更新代理、规则、嗅探、hosts、通用设置、DNS、listeners、TUN、providers 与 profile 等运行组件，再恢复运行。[配置应用源码](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/hub/executor/executor.go)

**关键设计推断。** `/version`、`/configs` 或 `/traffic` 成功仅能说明控制面可达；即使 `/traffic` WebSocket 持续连接且显示零流量，也不能推断系统代理/TUN 成功，更不能把“流量通道已连接”显示成“代理已开启”。客户端至少应分开呈现：

1. 内核进程状态；
2. 控制器可达状态；
3. 流量接管状态；
4. 可选的端到端路径探测结果。

## 3. 核心机制与客户端职责

### 3.1 入站、系统代理与 TUN

**来源事实。** Mihomo 可提供 HTTP、SOCKS、mixed、redir、TProxy、TUN 以及自定义 listeners 等入站。监听地址、路由、防火墙和平台权限决定这些入站是否真的可达；官方特别警告，不应把未加密的 HTTP/SOCKS/mixed 监听直接暴露在互联网。[入站配置](https://wiki.metacubex.one/en/config/inbound/)

**来源事实。** TUN 可以配置 `auto-route`、`auto-detect-interface`、`dns-hijack` 和 `strict-route` 等。平台行为并不完全一致；例如 macOS/Windows 无法通过自动路由劫持发往局域网的 DNS 请求，`strict-route` 也可能与部分虚拟化网络冲突。[TUN 配置](https://wiki.metacubex.one/en/config/inbound/tun/)

**设计推断。** 系统代理与 TUN 都是“接管方法”，但不能合成一个没有细节的总开关：

| 接管方式 | 能接管什么 | 常见失败 | 客户端应显示什么 |
|---|---|---|---|
| 系统代理 | 遵循 OS HTTP/HTTPS/SOCKS 设置的应用 | 设置被别的软件覆盖、目标端口未监听、PAC/绕过规则差异 | 用户意图、系统实际值、所有权/是否仍由本客户端管理 |
| TUN | 通过虚拟网卡和路由进入内核的 IP 流量 | 权限不足、路由/DNS 劫持失败、与 VPN/虚拟机冲突 | 配置是否启用、权限、设备/路由是否创建、失败原因 |
| 应用显式代理 | 单个应用主动连接 Mihomo 入站 | 应用配置错误、端口或认证不匹配 | 监听地址与端口，不能冒充系统级接管 |

最小产品状态应采用“系统代理：已接管/未接管/被外部修改”和“TUN：已启用/未启用/启动失败”两行，而不是单一的“代理：开”。

### 3.2 规则匹配与运行模式

**来源事实。** 通用模式包含 `rule`、`global`、`direct`，默认是 `rule`。[通用配置](https://wiki.metacubex.one/en/config/general/) Rule 模式中的规则按配置从上到下匹配，首个适用规则决定目标。某些 IP 类规则会触发 DNS 解析，可用 `no-resolve` 抑制；如果目标代理不支持 UDP，内核会继续尝试后续规则。[规则配置](https://wiki.metacubex.one/en/config/rules/) 核心源码也明确分支处理 Direct、Global 和 Rule。[`tunnel/tunnel.go`](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/tunnel/tunnel.go)

**设计推断。** 客户端需要：

- 在主界面显示并允许安全切换运行模式；
- 在连接详情中展示最终 rule、rule payload、chain/provider chain，帮助回答“为什么走这个节点”；
- 对规则禁用明确标注为临时状态，因为 API 的 rule disable 会在内核重启后恢复。[RESTful API：Rules](https://wiki.metacubex.one/en/api/)

不建议把每种规则类型都做成首页可视化编辑器。规则顺序、`no-resolve`、provider 与覆写共同决定语义，表单化很容易产生“看起来简单、实际改错”的体验。

### 3.3 代理组：手动选择、自动选择与隐藏语义

**来源事实。** 代理组可以从显式 `proxies` 或 provider 的 `use` 获取节点，并支持健康检查 URL、interval、lazy、filter/exclude、`hidden`、`icon`、`default-selected`、`empty-fallback` 等字段。[代理组配置](https://wiki.metacubex.one/en/config/proxy-groups/)

**来源事实。** RESTful API 中代理组对象包含 `now`、`all`、`testUrl`、`hidden`、`icon`、`emptyFallback`、`expectedStatus` 和 `fixed`；`fixed` 只适用于 URLTest/Fallback。对 `/proxies/:name` 执行 `PUT` 可选择节点；对非 Selector 自动组执行 `DELETE` 可清除固定选择，使其恢复自动策略。`GET /group/:name/delay` 会对组内节点测速，并清除自动策略组的固定选择。[RESTful API：Proxies / Proxy Groups](https://wiki.metacubex.one/en/api/)

**来源事实。** `URLTest` 的实现会从可用代理中按延迟与 tolerance 自动选择，但一旦设置 `selected`，该组会进入固定状态；其 API 序列化也包含 `fixed`/`hidden` 等信息。[URLTest 源码](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/adapter/outboundgroup/urltest.go)

**设计推断。** 组类型必须影响交互，而不能把所有组都做成相同的“选择节点”按钮：

- Selector：选中节点是组的本职行为；不显示“自动”。
- URLTest/Fallback：点击具体节点会固定自动组，应显示“已固定到 X”，并提供“恢复自动选择”。
- LoadBalance：重点展示策略和可用性，不应暗示存在唯一持久选中节点。
- `hidden: true`：默认不出现在普通列表；在高级设置提供“显示隐藏策略组”，以便排障。
- `GET /group/:name/delay` 不是无副作用的普通测速。UI 应将其命名为“测速并恢复自动”，或继续使用逐节点 delay API 做不改变固定状态的批量测速。

最后一点尤其重要：为了性能直接把现有“测试全部节点”改成 `/group/:name/delay`，会意外解除用户的固定选择。API 的副作用必须进入产品文案与行为测试。

### 3.4 Proxy Provider 与 Rule Provider

**来源事实。** Proxy provider 支持 `http`、`file`、`inline`，可配置更新周期、健康检查、过滤/排除、覆写和订阅信息；远程或文件内容解析失败时可回退到 `payload`。[Proxy Provider 配置](https://wiki.metacubex.one/en/config/proxy-providers/) Provider 实现负责初始化、更新、健康检查与订阅信息，重新初始化 provider 后会关闭相关连接。[Provider 源码](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/adapter/provider/provider.go)

**来源事实。** Rule provider 同样支持 `http`、`file`、`inline`，并区分 `domain`、`ipcidr`、`classical` 行为以及 `yaml`、`text`、`mrs` 格式。[Rule Provider 配置](https://wiki.metacubex.one/en/config/rule-providers/)

**设计推断。** 简洁客户端需要显示 provider 的状态、上次更新时间、节点/规则数量、错误、手动更新和健康检查；不需要默认暴露所有 filter、header、override 和路径字段。远程 provider 更新属于“可失败但不应破坏当前可用状态”的维护动作，UI 应保留最后成功版本，并清楚区分“订阅源下载失败”和“内核当前配置不可用”。

### 3.5 DNS 与 fake-IP

**来源事实。** DNS 关闭时使用系统 DNS；增强模式包含 `fake-ip` 与 `redir-host`。fake-IP 模式为域名分配映射地址，随后在流量进入时反查原始域名。`default-nameserver` 用于解析 DNS 服务器自身的域名，`proxy-server-nameserver` 只用于代理节点域名，`nameserver-policy` 可按域名选择解析器；`respect-rules` 与 `proxy-server-nameserver` 存在配置关系。[DNS 配置](https://wiki.metacubex.one/en/config/dns/) 数据路径中的 fake-IP 反查可在 [`tunnel/tunnel.go`](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/tunnel/tunnel.go) 核对。

**来源事实。** API 提供 `/dns/query?name=&type=` 查询 DNS message，也提供 `POST /cache/dns/flush` 与 `POST /cache/fakeip/flush`。[RESTful API：DNS / Cache](https://wiki.metacubex.one/en/api/)

**设计推断。** DNS 页面应优先做诊断而不是“所有字段表单化”：

- 推荐增加 `/dns/query`，默认支持 A/AAAA，展示 status、answer、TTL 和使用的查询类型；这是排查 policy、节点域名解析与 fake-IP 的高价值工具，但不必放首页。
- 推荐增加 DNS cache 与 fake-IP cache 清理，但放在高级恢复区并二次确认。清缓存不是普通刷新；它不会重建既有连接，fake-IP 映射变化还可能让仍在途的旧映射暂时失效。
- 首页只需显示 DNS 模式与异常，不要展示完整 nameserver 树。

### 3.6 嗅探

**来源事实。** Mihomo 可对 HTTP、TLS 和 QUIC 等流量进行 sniff，支持强制域名、跳过域名、覆盖目标，以及按源/目标 IP 跳过。[Sniffer 配置](https://wiki.metacubex.one/en/config/sniff/)

**设计推断。** Sniff 会改变用于规则匹配或目标覆盖的 metadata，兼具兼容性与隐私含义。客户端应提供总开关、当前协议和简明解释，把 force/skip/override 细节留给高级设置或 YAML。修改时应提示它可能改变路由结果，而不是把它描述成无条件的“性能优化”。

### 3.7 连接、流量与日志

**来源事实。** `/connections` 可通过 GET 或 WebSocket 返回连接 metadata、chains、providerChains、rule 等，也支持删除全部连接或按 ID 删除单条连接。`/traffic` 通过 GET/WebSocket 每秒返回上传、下载与累计值。`/logs` 支持普通消息和 `?format=structured`，后者包含服务端 `time`、`level`、`message` 与 `fields`。[RESTful API：Connections / Traffic / Logs](https://wiki.metacubex.one/en/api/)

**设计推断。**

- 连接列表是“解释实际路由”的核心诊断入口，应支持过滤、查看规则与 chain、关闭单条和明确的“关闭全部”。
- 流量图只是内核观测数据，不是接管健康探针。断流时应显示“统计连接中断”，而不是直接显示“代理断开”。
- Structured logs 是中优先级但高性价比能力：能保留内核时间和字段，便于级别筛选、复制诊断和脱敏。客户端应先尝试 structured，并为旧版兼容内核或实验后端保留普通格式回退。
- 切换代理组后，Mihomo 只保证新选择生效；是否强制关闭既有连接是客户端策略。默认无提示地删除全部连接会造成下载、通话或长连接中断。更稳妥的是默认只影响新连接，提供按受影响 chain 精确关闭或显式“重建全部连接”。

### 3.8 外部控制器与持久化

**来源事实。** 官方路由包含日志、流量、内存、版本、配置、代理、代理组、规则、连接、providers、cache、DNS、storage、重启与升级等接口。设置非空 `secret` 后 HTTP 请求使用 Bearer token；浏览器 WebSocket 还允许 query `token`。Unix socket/named pipe 接口不使用该 secret。[控制器路由源码](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/hub/route/server.go) · [通用配置](https://wiki.metacubex.one/en/config/general/) · [RESTful API](https://wiki.metacubex.one/en/api/)

**来源事实。** Profile 的 `store-selected` 控制代理组选择是否持久化，`store-fake-ip` 控制 fake-IP 映射是否持久化。[通用配置：Profile](https://wiki.metacubex.one/en/config/general/) `cachefile` 实现使用 bbolt 存储 selected、fakeip/fakeip6、etag、subscription info 和 storage 等 bucket；只有启用 StoreSelected 才保存选择。[持久化源码](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/component/profile/cachefile/cache.go)

**设计推断。** 客户端自己的“用户意图”与内核运行状态应分开持久化：

- 用户选择的 profile、是否期望启用系统代理、窗口偏好属于客户端状态；
- 节点实际选择、当前连接、provider 运行状态属于内核状态，应以 API readback 为准；
- 不要在客户端数据库与内核 cache 中各维护一份“当前节点真相”；
- 导入的 YAML/订阅保持只读原件，生成的 effective config 放在受控存储并原子替换。

### 3.9 健康检查与进程生命周期

**来源事实。** Mihomo 启动参数包含 `-d` HomeDir、`-f` 配置文件以及 `-t` 验证配置并退出；进程接收 SIGINT/SIGTERM 时执行 shutdown，SIGHUP 可触发配置重载。[`main.go`](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/main.go) 官方 systemd 示例使用自动重启、HUP 重载和日志管理，但这只是 Linux 部署示例，不是桌面客户端规范。[作为服务运行](https://wiki.metacubex.one/en/startup/service/)

**设计推断。** “健康”应拆成四层，避免一个绿点掩盖问题：

| 层级 | 能证明什么 | 推荐检查 |
|---|---|---|
| L1 进程 | 托管内核仍在运行 | child handle、退出码；外部内核不得伪装成客户端子进程 |
| L2 控制器 | API 兼容且可达 | `/version`、必要时 `/configs`；认证错误与连接错误分开 |
| L3 接管 | 用户流量有进入内核的有效路径 | 系统代理 actual/ownership；TUN 设备、权限、路由与启动错误 |
| L4 路径 | 某个目标确实按预期连通 | 用户触发或低频端到端 probe，注明走 Mihomo 还是 DIRECT |

稳定的托管生命周期应是：定位并验证二进制 → `-t` 验证生成配置 → 启动并等待控制器 ready → 提交客户端状态 → 正常退出先恢复自己拥有的系统代理再终止内核 → 超时才强杀。热更新失败时，只有在托管内核且控制器状态不确定时才考虑重启；外部内核绝不能被客户端擅自重启。

## 4. 一个合格、好用且简洁的客户端应有哪些功能

### 4.1 必须有：产品成立的最小闭环

| 能力 | 最小交互 | 理由 |
|---|---|---|
| 内核生命周期 | 启动、ready、退出、崩溃说明；托管/外部明确区分 | 没有可靠内核就没有数据面 |
| 配置/订阅 | 导入、更新、验证、切换、失败回滚；显示最后成功状态 | 配置是路由行为来源 |
| 流量接管 | 系统代理和 TUN 分开控制、分开显示实际状态 | 防止“API 正常但没有流量” |
| 模式与代理组 | Rule/Global/Direct；Selector 与自动组语义正确；API readback | 高频控制面 |
| 基本健康检查 | 节点延迟、provider 状态、明确失败原因 | 用户需要判断“节点坏了还是接管坏了” |
| 连接与基础流量 | 当前连接、route chain、单条/全部关闭；实时速率 | 可解释与可恢复 |
| 错误与恢复 | 可操作错误、重试、恢复上次可用状态 | 桌面常驻应用不能把失败留给用户猜 |
| 最小安全边界 | loopback controller、随机 secret、凭据脱敏、权限最小化 | 控制器和订阅都具有敏感权限/数据 |

### 4.2 推荐有：高价值但可放二级入口

| 能力 | 建议入口 | 价值判断 |
|---|---|---|
| DNS query 与缓存恢复 | DNS/诊断页 | 高诊断价值，低日常频率 |
| Structured logs | 日志页 | 中优先级，显著提升筛选与支持效率 |
| Provider 手动更新/健康检查 | 资源页 | 订阅失败时必要 |
| 规则命中解释 | 连接详情 | 比完整规则编辑器更有用 |
| 端到端网络探测 | 网络诊断页 | 区分节点、DNS、接管与目标站故障 |
| 覆写/增强预览 | Profile 高级入口 | 解决用户定制，但必须能看 effective config 与来源 |
| 备份与恢复 | 设置 | 有状态客户端的重要兜底，恢复前验证 |
| 托盘快速操作 | 托盘 | 切配置、模式、系统代理、关键组的低摩擦入口 |
| 支持包 | 诊断页 | 汇总版本、状态和脱敏日志，不包含 secret/订阅 URL |

### 4.3 应克制：内核支持不等于应该可视化

- 不把所有 DNS、TUN 路由、sniffer、rule/provider 字段做成常驻表单；优先 YAML + 可验证覆写。
- 不默认开放 LAN 监听、远程 controller 或外部访问；这些属于高风险高级功能。
- 不把自动脚本、Smart 路由、多内核、主题市场、插件系统当成“合格客户端”的门槛。
- 不直接修改导入订阅原文来实现覆写；保留来源、生成 effective config，并能解释差异。
- 不提供一个同时改变系统代理、TUN、模式、DNS、连接的万能开关；失败后的部分状态无法解释和回滚。
- 不在 UI 中隐藏高影响副作用，例如“切换节点后自动关闭全部连接”或“组测速同时解除 fixed”。

## 5. 安全、可靠性与失败恢复

### 5.1 安全基线

**来源事实。** HTTP controller 支持 Bearer secret，官方示例以 loopback 监听；Unix socket/named pipe 不走 secret。普通 HTTP/SOCKS/mixed 入站没有传输加密，不应暴露到公网。[通用配置](https://wiki.metacubex.one/en/config/general/) · [入站配置](https://wiki.metacubex.one/en/config/inbound/) · [控制器源码](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/hub/route/server.go)

**设计推断。**

- 默认只绑定 loopback，并生成非空、随机 controller secret；原生客户端使用 Authorization header，不把 token 放 URL、日志或错误报告。
- Unix socket/named pipe 的安全依赖文件系统/命名管道边界，不能因为“不需要 secret”就视为天然安全；必须限制目录和对象权限。
- 订阅 URL、provider header、controller secret、节点认证、连接目标和日志都按敏感数据处理，复制/导出前默认脱敏。
- TUN 提权应限定为安装/启动所需的最小 helper 能力，GUI 不应长期以管理员身份运行。
- Profile 和 provider 是不可信输入：先限制文件/响应大小与路径，再调用与运行版本相同的内核执行 `-t` 验证。
- 内核升级应固定 release/tag 与校验值，下载、验证、替换和回退相互独立；不要把 controller 的 `/upgrade` 直接变成无确认自动升级。

### 5.2 失败恢复矩阵

| 失败 | 用户可见状态 | 自动动作 | 不应做什么 |
|---|---|---|---|
| 新配置验证失败 | 显示具体验证错误，仍使用旧配置 | 保留旧 effective config | 覆盖最后可用配置 |
| 热应用明确拒绝 | 显示未应用，状态回读 | 回滚客户端意图 | 假装成功或盲目重启 |
| 控制器传输结果不确定 | 显示“状态待确认” | 重新探测；托管内核必要时受控重启 | 对外部内核执行重启 |
| 内核崩溃 | 区分退出码/配置/权限 | 有界退避重启；连续失败停下 | 无限重启风暴 |
| 系统代理被外部修改 | 显示所有权丢失 | 停止声称“已接管”，允许重新获取 | 退出时恢复并非自己拥有的值 |
| TUN 启动失败 | 显示权限/设备/路由原因 | 保持 UI 与实际一致，提供重试 | 把 controller ready 当成 TUN ready |
| Provider 更新失败 | 显示更新时间与错误 | 继续使用最后成功内容 | 清空当前可用 provider |
| 流量/日志 WS 中断 | 显示统计流中断 | 有界重连并保留最后采样时间 | 推断用户网络已断 |
| 组选择成功、清连接失败 | 显示“选择已生效；旧连接未重建” | 读回 `now`，允许再次清理 | 把整个选择报告为失败 |

## 6. 官方客户端仓库能提供什么参考

以下只是**客户端实现/产品范围的观察**，不是 Mihomo 规范：

- Clash Verge Rev 官方 README 列出内置 Mihomo、配置管理与增强、系统代理守护/TUN、节点与规则可视化、WebDAV 等能力。[Clash Verge Rev README，commit `3503a2da`](https://github.com/clash-verge-rev/clash-verge-rev/blob/3503a2da29d68a4398c0b8e9234cffb711e65783/README.md)
- FlClash 官方 README 强调跨平台、自适应布局、Material UI、WebDAV 与订阅链接导入。[FlClash 官方仓库](https://github.com/chen08209/FlClash)
- Clash Party（原 Mihomo Party）官方 README 列出 TUN、常用配置编辑、Mihomo/Smart 内核、WebDAV、覆写和 Sub-Store 等能力。[Clash Party 官方仓库](https://github.com/mihomo-party-org/clash-party)
- Clash Nyanpasu 官方 README 列出多内核、配置管理、YAML/JavaScript/Lua 增强与 provider 管理。[Clash Nyanpasu 官方仓库](https://github.com/LibNyanpasu/clash-nyanpasu)

**设计推断。** 这些项目共同说明配置管理、系统接管、代理组、诊断和托盘是成熟桌面客户端的常见范围；WebDAV、多内核、脚本增强、Sub-Store 等只是产品选择。ZenClash 不应为“功能对齐”牺牲其更有区分度的状态可信、失败恢复和原生轻量体验。

## 7. 面向 ZenClash 的具体建议

### 7.1 现有设计中应保留的部分

对当前仓库的审阅表明，下列设计已经符合本文推导出的可靠性原则：

- [`CONTEXT.md`](../../CONTEXT.md) 已把托管/外部内核、effective config、系统代理 intent/actual/ownership 和以核心回读为准的代理选择定义为领域概念。
- [`ADR 0001`](../adr/0001-core-session-and-runtime-seams.md) 与 [`core_session.rs`](../../crates/zenclash-core/src/core_session.rs) 将 apply/maintain/shutdown 串行化，并限制“结果不确定才重启”的边界。
- [`system_proxy.rs`](../../crates/zenclash-core/src/system_proxy.rs) 已为系统代理提供事务、回滚和所有权对账。
- [`controlled_config.rs`](../../crates/zenclash-core/src/controlled_config.rs) 及 profile/backup workflow 已采用验证与回滚路径。
- [`process.rs`](../../crates/zenclash-core/src/process.rs) 有 readiness 等待、优雅终止与强杀兜底，且 child drop 会停止进程。

这些深层模块比增加更多页面或表单更值得继续强化。

### 7.2 P0：先解决真实性和代理组语义

#### A. 首页改成“接管状态摘要”

**当前观察。** [`home.rs`](../../crates/zenclash-ui/src/pages/runtime/home.rs) 显示 system proxy、mode、traffic WebSocket 与连接数，但没有同等层级的 TUN 实际状态。

**建议。** 首页分开显示：

- Core：运行中 / 外部可达 / 不可达；
- System Proxy：已接管 / 关闭 / 被外部修改；
- TUN：已启用 / 关闭 / 权限或路由失败；
- Statistics：实时 / 重连中 / 最后采样时间。

“Statistics 实时”绝不能代替“System Proxy/TUN 已接管”。如暂时无法验证 TUN 设备/路由，至少明确显示“配置已启用，实际接管未验证”。

#### B. 首次使用直接形成可恢复闭环

**当前观察。** 仓库已有订阅导入、真实内核验证、直连恢复配置、TUN 授权和网络探测，但没有把它们组织成首次使用路径。

**建议。** 不需要维护一套独立且容易过期的“向导进度”；根据运行事实推导下一步：无 profile 时引导添加订阅或本地 YAML → 验证失败时就地修复 → 验证成功后选择 System Proxy 或 TUN → 最后执行一次明确标注路径的连通性检查。每一步都允许退出，重新打开后从事实状态继续，而不是重新开始向导。

#### C. 首页从“全成或全败”改为分片快照

**当前观察。** [`loader.rs`](../../crates/zenclash-ui/src/pages/runtime/loader.rs) 并行读取 config、proxy catalog、connections 和 system proxy，但随后对每项使用 `?`；任一端点或操作系统检测失败，整个 dashboard 都退化成错误页。

**建议。** 让首页每个分片独立表达 `Loading / Fresh / Stale / Failed`，保留最后一次可信值和时间戳，并在卡片内给出重试动作。例如 `/connections` 失败时，profile、capture 和 mode 仍应可见、可操作。错误摘要可以聚合，但不能取代字段或卡片附近的具体错误与恢复入口。

#### D. 完整建模 `hidden` 与 `fixed`

**当前观察。** [`proxy.rs`](../../crates/zenclash-core/src/proxy.rs) 已解析 `hidden`，但当前 UI 只是把 hidden 组排序靠后，仍默认展示；模型没有 `fixed`。

**建议。**

1. 普通列表过滤 `hidden: true`；高级设置可显示隐藏组。
2. 为 URLTest/Fallback 建模 `fixed` 并显示 badge。
3. 点击自动组中的具体节点前，文案明确“固定到此节点”。
4. 提供“恢复自动”动作，调用 `DELETE /proxies/:name` 后必须 GET readback。
5. Selector 不显示“恢复自动”，因为官方 API 明确把它排除在清除 fixed 的语义之外。[RESTful API](https://wiki.metacubex.one/en/api/)

#### E. 不把 group delay 当成无副作用优化

**当前观察。** 当前测试全部节点采用逐节点/provider delay 请求；尚未使用 `/group/:name/delay`。

**建议。** 不要仅为了减少请求就替换。保留“仅测速”现有语义；另外为自动组增加显式“测速并恢复自动”，调用 `/group/:name/delay`，完成后回读 `fixed` 和 `now`。为此补行为测试：一个固定的 URLTest 组执行普通批量测速后仍 fixed；执行“测速并恢复自动”后 fixed 被清除。

#### F. 调整切换节点后的连接策略

**当前观察。** [`proxy_operations.rs`](../../crates/zenclash-core/src/proxy_operations.rs) 在选择后无条件 `DELETE /connections`，即关闭全部连接。

**建议。** 首选默认只让新连接使用新节点，并提供显式“切换并重建全部连接”；如果产品必须保持自动重建，至少在设置和操作反馈中披露。进一步可利用 connection chains 只关闭受影响连接。保留现有“选择成功但清连接失败时不谎报选择失败”的事务语义。

### 7.3 P1：补齐高价值诊断

#### G. DNS query 与缓存恢复

[`client/api.rs`](../../crates/zenclash-core/src/client/api.rs) 当前没有 `/dns/query`、`/cache/dns/flush`、`/cache/fakeip/flush`。建议把查询放 DNS 页的主诊断区；两个 flush 放“恢复工具”区并二次确认。它们是高价值诊断/恢复能力，但不是首页必须项。

#### H. Structured logs

[`logs.rs`](../../crates/zenclash-core/src/logs.rs) 当前消费普通 `/logs` 消息并使用本地接收时间。建议支持 `format=structured`，保留内核时间和 fields，实现 level/模块筛选与脱敏复制；旧兼容内核或 meow-rs 不支持时回退普通格式。这是中优先级的 API adapter 改进，不应阻塞 P0。

#### I. 端到端诊断与支持包

[`network.rs`](../../crates/zenclash-ui/src/pages/runtime/network.rs) 已能区分经 Mihomo 与 DIRECT 的公共 IP/延迟检查。建议把它与四层健康模型结合，输出“控制器可达、接管状态、DNS 查询、经代理探测、直连对照”的结果，并提供默认脱敏支持包。

#### J. 应用版本更新提示

当前仓库已有固定版本、校验下载和事务替换的 Mihomo 内核更新流程，但没有同等的 ZenClash 应用更新入口。简洁方案先做“检查新版本 → 展示 release notes → 打开官方签名安装包”的通知闭环；只有在各平台签名、替换失败回滚和权限模型都成熟后，再做静默或自动安装。它是供应链安全和问题修复能力，但不应挤进 P0。

### 7.4 P2：体验收束

- 首页只保留 profile、capture、mode、关键组、错误；连接/规则/流量/日志继续作为一级诊断入口。
- DNS、Sniffer、Resources、Override、Network 收入“高级与诊断”，而不是和高频页面竞争。
- Proxy 页优先展示 profile 作者未隐藏的组、当前策略、fixed/auto 和健康；协议细节放节点详情。
- 错误提示始终包含“哪一层失败、当前仍然有效什么、用户可做什么”，避免只显示 HTTP 状态或底层 I/O 文本。
- Sidebar 的导航项应使用可聚焦控件并提供清晰的键盘焦点态；当前实现是带 `on_click` 的布局行，不能把鼠标可点等同于完整键盘导航。
- 实时流量继续使用带数值标签的简洁面积图；颜色不能成为区分上传/下载或异常的唯一方式，统计流中断时保留最后更新时间。

### 7.5 暂不建议优先投入

- 全量 YAML 字段的可视化编辑器；
- 为对齐竞品而引入多内核、脚本市场或 Sub-Store；
- 可换肤 dashboard、复杂图表或长期保留的大量历史流量；
- 默认开启 LAN/远程 controller；
- 在尚未解决 capture/fixed/hidden 真实性前继续增加首页状态卡。

### 7.6 模块与 seam 建议

[`RuntimePage`](../../crates/zenclash-ui/src/pages/runtime.rs) 当前同时拥有 `CoreSession`、`MihomoClient`、进程、多个 store、页面表单、网络探测、备份状态以及全局 `loading/mutating/error/notice`。这使页面既是渲染入口，也是工作流协调者；新增一个操作时，调用方必须理解越来越多的排序、代次和失败规则，模块的 interface 正在变浅。

建议沿用现有深模块方向，小步调整而不是再建一层万能 manager：

1. **新增只读 `OperationalStatus` 模块。** 小 interface 只需 `snapshot()` 与受控刷新/订阅入口，implementation 内部聚合 L1 进程、L2 controller、L3 capture、L4 probe 和统计流状态。它是 in-process seam，不需要为了测试公开一组 trait。
2. **把接管变更集中到 `TrafficCaptureSession`。** interface 表达一份接管计划；普通 UI 只提供 `Off / SystemProxy / Tun`，确有需要时再在高级入口保留显式组合模式。结果同时返回 System Proxy 的 actual/ownership 与 TUN 的 permission/device/route 细节；implementation 复用现有 `SystemProxySession` 和 `CoreSession`。这不是把二者伪装成同一个布尔开关，而是把跨模块排序、回滚和互斥规则藏在一个深模块后。
3. **页面保留页面状态，工作流离开页面。** 输入框、展开项、筛选词属于页面；配置激活、接管切换、节点选择、核心维护属于对应深模块。用按操作域划分的状态替代全局 `mutating`，避免一次日志操作阻塞无关页面。
4. **interface 就是行为测试表面。** 新测试通过 `OperationalStatus` 和 `TrafficCaptureSession` 的结果验证可见行为；形成新 interface 后，删除穿透内部实现的重复测试，不在旧测试上继续叠层。

这一调整的目标是提高 leverage 和 locality：UI 学习更少的 interface，却能获得完整状态；接管或恢复规则改变时集中在一个 implementation 中验证。

## 8. 建议的验收行为

以下行为测试最能保护产品语义：

1. **Controller 与 capture 分离：** `/traffic` 正常但系统代理关闭、TUN 关闭时，首页不得显示“代理已接管”。
2. **系统代理所有权：** 开启后被外部应用覆盖，ZenClash 显示所有权丢失；退出时不覆盖外部新值。
3. **TUN 失败：** 配置含 `tun.enable=true` 但权限或路由失败，显示失败而不是“已启用”。
4. **Hidden group：** `hidden=true` 默认不出现，开启“显示隐藏组”后出现。
5. **Fixed group：** URLTest/Fallback 的 `fixed` 正确显示；“恢复自动”调用 DELETE 并以 GET 回读为准。
6. **Group delay 副作用：** 普通批量测速不清 fixed；“测速并恢复自动”明确清除 fixed。
7. **Selector 差异：** Selector 不提供清 fixed 操作。
8. **连接不中断默认值：** 普通选择不静默删除全部连接；显式重建才执行全量删除。
9. **配置原子性：** 新 profile 验证失败时原 profile、进程和接管状态保持可用。
10. **外部内核边界：** 外部 controller 热应用不确定时不重启、不终止外部进程。
11. **DNS 恢复：** query 正确显示返回状态；flush 有确认且不会宣称既有连接已刷新。
12. **日志兼容：** structured 成功保留服务端时间/fields；不支持时回退普通格式且持续重连。
13. **首页分片降级：** `/connections` 或 system proxy 检测失败时，profile、mode 和其他成功分片仍可见，失败卡片显示最后可信时间与重试。
14. **首次使用续接：** 用户在导入、授权或探测任一步退出后，重新打开会根据事实状态进入下一步，而不是重复已完成步骤。
15. **键盘导航：** 不使用指针也能到达并触发所有一级导航与首页主操作，焦点顺序与视觉顺序一致且焦点可见。

## 9. 实施顺序建议

```text
P0 状态真实性
  ├─ capture 摘要：System Proxy / TUN / Statistics 分离
  ├─ 首次使用闭环 + 首页分片降级
  ├─ group hidden / fixed / restore-auto
  └─ 节点切换的连接中断策略
          │
          ▼
P1 诊断闭环
  ├─ DNS query + cache recovery
  ├─ structured logs
  ├─ 四层健康检查 + 脱敏支持包
  └─ 应用版本更新提示
          │
          ▼
P2 信息架构收束
  ├─ 首页只回答关键状态
  ├─ 高级配置集中到诊断/高级入口
  └─ RuntimePage 状态所有权下沉到深模块
```

这条顺序的判断依据不是“功能数量”，而是用户是否能相信客户端显示的状态。对代理客户端来说，一个准确说明“内核可达，但流量尚未接管”的界面，比一个功能更多、却把 controller 流量流当成代理开关的界面更合格。

## 10. 实际核对的来源

### Mihomo 官方资料

- [Mihomo `v1.19.30` release（commit `ac017cd`）](https://github.com/MetaCubeX/mihomo/releases/tag/v1.19.30)
- [`v1.19.30` README](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/README.md)
- [RESTful API](https://wiki.metacubex.one/en/api/)
- [通用配置](https://wiki.metacubex.one/en/config/general/)
- [入站配置](https://wiki.metacubex.one/en/config/inbound/)
- [TUN 配置](https://wiki.metacubex.one/en/config/inbound/tun/)
- [规则配置](https://wiki.metacubex.one/en/config/rules/)
- [代理组配置](https://wiki.metacubex.one/en/config/proxy-groups/)
- [Proxy Provider 配置](https://wiki.metacubex.one/en/config/proxy-providers/)
- [Rule Provider 配置](https://wiki.metacubex.one/en/config/rule-providers/)
- [DNS 配置](https://wiki.metacubex.one/en/config/dns/)
- [Sniffer 配置](https://wiki.metacubex.one/en/config/sniff/)
- [服务运行示例](https://wiki.metacubex.one/en/startup/service/)
- [`tunnel/tunnel.go`](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/tunnel/tunnel.go)
- [`hub/executor/executor.go`](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/hub/executor/executor.go)
- [`hub/route/server.go`](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/hub/route/server.go)
- [`adapter/outboundgroup/urltest.go`](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/adapter/outboundgroup/urltest.go)
- [`adapter/provider/provider.go`](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/adapter/provider/provider.go)
- [`component/profile/cachefile/cache.go`](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/component/profile/cachefile/cache.go)
- [`main.go`](https://github.com/MetaCubeX/mihomo/blob/v1.19.30/main.go)

### 客户端官方仓库

- [Clash Verge Rev，commit `3503a2da`](https://github.com/clash-verge-rev/clash-verge-rev/blob/3503a2da29d68a4398c0b8e9234cffb711e65783/README.md)
- [FlClash](https://github.com/chen08209/FlClash)
- [Clash Party](https://github.com/mihomo-party-org/clash-party)
- [Clash Nyanpasu](https://github.com/LibNyanpasu/clash-nyanpasu)

动态官方文档和未固定 commit 的客户端仓库内容均于 2026-08-27 实际核对；后续实现前应再次检查 API 副作用和字段定义是否发生变化。
