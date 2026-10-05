# ZenClash 服务应用集成

本目录是在复制的 Clash Verge Rev 应用源码基础上维护的 ZenClash 本地实现。
`crates/zenclash-service` 同样是独立维护的本地 Fork。
构建使用 workspace 中的这两个 crate；`examples` 只用于人工对照，不参与构建，
没有对上游应用或服务仓库的 Git/path 依赖，也不会动态同步上游代码。
通用第三方 Rust 库依赖仍按 Cargo.lock 获取。

复制与来源声明：GPL-3.0-only；保留原作者、完整原始 LICENSE、NOTICE.md 和 UPSTREAM.json。
后续可以自行修改、扩展和维护，在修改版本中持续记录来源、修改日期，并按 GPL 分发完整对应源码。
具体发布流程见 [GPL 分发文档](../../docs/development/gpl-distribution.md)。

## 当前开发范围：Windows（2026-10-05）

按当前要求先完成 Windows；macOS/Linux 的进一步接线和验证留给对应平台开发。
已经复制的跨平台基础源码保留，不以它证明其他系统已经验收。本轮新增的 macOS
代理接线已撤回。两个 Fork 都在本仓库独立维护，examples 与上游仓库不参与构建。

Windows 主应用使用原生所有者会话、Start/Stage/Stop、命名管道 HTTP/WebSocket、
共享 RunState、后台授权与 Service/Sidecar 互斥。服务可用时走认证服务，不可用时
依上游 Windows 策略选择 Sidecar，不能绕过服务/残留内核的执行锁。GUI 始终由
zenclash.exe 启动；服务启动或维修失败也保留 GUI。系统代理由普通用户桌面后端
控制，TUN 的特权内核操作走服务；此次没有运行真实 TUN。

本轮复验：Fork 服务 123 项通过、1 项忽略；集成库 118 项通过；主应用真实隔离
Fork IPC 生命周期 1 项通过；默认生产 GUI 编译及全 target/feature 严格 Clippy
通过。Windows 打包 9 个用例验证 GUI/服务载荷、构建失败、版本失败和入口。
安装脚本显式要求非空 GUI，Inno 清单独立列出 zenclash.exe；开始菜单、桌面及
安装后启动均指向 GUI。隔离模拟内核测试不等于真实 TUN、订阅或安装后 UI 验收。

GPL 原始许可、作者与修改来源继续保留。旧 dist/gpl-2026-10-05-native-owner
是不可变的同版二进制/源码配对；本轮更新不得冒充已包含在旧源码中。
新增交付计划放在 dist/gpl-2026-10-05-windows-focus，只有配套安装包、完整源码和
BUILD-MANIFEST.json 全部完成才构成交付。真实安装与原生 GUI 操作仍未验收。

## 已复制与适配

- 所有者身份/SID/UID、私有令牌与 Windows ACL。
- RuntimeBundle 的本地资源、远程 provider 与路径冲突处理。
- 完整 RunStateStore 状态与操作串行化、健康/TUN 能力判定、PAC 与运行模式同步。
- Windows SCM、macOS launchd/helper 和 Linux systemd 注册探测，移除旧 Clash Verge 标识。
- OwnerWatch 的所有权失效、短暂故障容忍和恢复决策。
- Windows 启动新会话、暂存配置、停止和缓存读回的会话凭证接线；保留上游 macOS 代理 API，主应用该平台接线留待后续。

Tauri 全局副作用改为 `NativeEnv<H: RunStateHost>`，由 ZenClash 的 core/PAC/GPUI 所有者提供。
上游 RunStateStore 的 API 要求按应用会话持有，发布结果时不持有内部锁。
主程序 CoreSession 已持有一个共享的持久 store；服务健康、pending action 和维护操作槽共用它。仅在创建会话之前的启动后端判定使用临时只读 store。实际内核运行模式和应用 PAC 通过 CoreSession 的后台观察与保留事务同步，GPUI 订阅内存状态并在前台刷新。
`RunStateHost::publish` 应投递状态/事件；不能从后台线程直接操作 GPUI 对象。
调用方在执行维护前用 `begin_operation` 保留操作槽，并完成真实内核的停止/恢复交接；
`perform` 在 blocking worker 中等待系统授权，避免占用异步或 GUI 线程。

## Mihomo 控制通道

使用与上游应用相同的 LocalSocket 方式：Windows 命名管道、Unix socket 上的 HTTP/WebSocket。
本项目此前自己的 native HTTP 传输与测试移入此处，适配会话证明和新 Fork 的实际内核 PID，
并增加 native WebSocket。不是 tauri-plugin-mihomo 的源码复制，也不依赖该插件。

调用前后确认所有者 generation 和内核 PID，连接时校验 native peer；错误不会伪造成功回执。
保留响应/请求大小、超时及取消清理限制；日志 WebSocket 帧和消息均限 128 KiB。
`stage_runtime` 只暂存，返回 `config_path` 后必须用 Mihomo `PUT /configs` 加载并检查结果。
这两个实际操作不能混同为已经应用成功。

## 当前验证与接入边界（2026-10-05）

Windows 的复制模块及本地传输已有 115 项单元测试通过（2026-10-05）。
1 项真实隔离 IPC 测试贯穿 Fork 服务、模拟内核 native HTTP/WebSocket、Stage 后 Reload，
并验证旧会话无法控制/停止替换会话、Service/Sidecar 互斥及日志帧限制。
后台授权等待不阻塞单线程异步执行的适配有单独回归测试。
模拟内核测试不等同于真实 Mihomo 或三平台实机验收。

生产包禁止启用 `ipc-tests`（该 feature 才启用模拟内核和测试 IPC）。
`application_ipc_config()` 复制上游应用的 1 秒超时、500 ms 间隔、20 次连接重试；
应在主程序 bootstrap 设置。正式 Windows 注册探测先检查停止状态，避免长时间无效等待。

主仓库 `zenclash-core` 的配置事务、HTTP/WebSocket、安装维护和 GUI 接入已迁移到实际 Fork 接口，Windows GUI 编译通过。配置保存由应用持有，服务只执行真实 Start/Stage/Stop；没有虚构 Commit RPC 或服务配置版本。追加离线维修后，核心库 641 项单元测试通过，GUI 19 项启动测试和 319 项组件/交互测试通过；workspace 严格 Clippy 与 Windows 生产发布构建通过。新增测试覆盖丢失/取消 Start 回执后令牌仍可用于认证 Stop，以及旧令牌不能停止替换会话。

三平台打包携带 `zenclash-service`、`zenclash-service-install` 和 `zenclash-service-uninstall`；Linux 注册单元和 macOS helper 由原生安装工具生成。维护操作只在用户确认后调用，GUI 快捷方式始终启动 `zenclash`。

离线 GUI 维修入口已接通：应用明确创建的离线会话保留正常用户内核来源，维修不依赖
运行中的内核或 TUN；选定内核验证失败、外部控制器、过期及其他会话请求不提交授权。
确认维修成功后重新启动 GUI 并走正常启动判定，失败或取消保留窗口。没有实际安装或 TUN 测试。
持久共享 RunState/PAC、维护 pending action 和核心事务已接通。Sidecar 会话许可/能力策略、
真实 Service 模式的应用层 IPC 验收及失去服务所有权后的恢复仍需完善与验收。阶段安装包及同版
源码已实际交付并通过解压后离线编译检查，但后续离线维修修复尚需重新打包、导出对应源码。
独立测试和编译不能视作三平台实机、真实安装或 TUN 验收。


## PAC 接入进度（2026-10-05）

`zenclash-core/src/system_proxy/pac.rs` 已增加共享可用性开关，参考上游
`src-tauri/src/utils/server.rs` 的暂停行为。不可用的 `/pac` GET/HEAD 返回 HTTP 503，
正文为 `PAC endpoint is inactive`，不返回配置脚本。HEAD 只发送对应响应头。
当前、替换及恢复保留监听器共享开关；暂停/恢复保持监听器地址、脚本和系统代理设置。
HTTP 响应继续使用 `Cache-Control: no-store`。

独立 PAC 服务默认可用，保留外部控制器功能的现有行为。应用会话绑定实际 PAC 实例后，
受管理内核在得到运行观察之前保持不可用；真正的启动/切换、配置提交/回滚、维护、恢复、
网络暂停/恢复和退出持有可嵌套的暂停 guard。外部控制器保持原有 PAC 可用性，但其运行
模式仍为 NotRunning，不伪装为本应用拥有的 Service/Sidecar。来源及修改日期记录在源码
与根 NOTICE.md，继续采用 GPL-3.0-only；新二进制须重新配套导出对应源码。


## 启动 Sidecar 会话策略接入（2026-10-05）

主程序 bootstrap 的服务健康观察已交给实际 CoreSession 的持久 store；成功选择本地
内核时记录复制的 accept/allow_sidecar_for_session 策略。GUI 初始化只在 Unknown 时
补做后台探测，避免立即撤销已接受的选择。Windows 从服务 Start 失败后回退时记录
真实失败原因，不把缓存 Ready 作为新 Sidecar 的服务能力。普通外部控制器不能通过
此接线取得服务能力，离线维修只继承健康事实、不冒充已选用或运行中的本地内核。

GUI 的授权标签和继续本地运行入口读取复制的 tun_capable/service_needs_attention；
已接受的 Sidecar 不重复询问，而服务维修仍可用。维护明确取消/失败后恢复该会话此前
被 pending action 清除的 allowance；服务已 Ready、绑定过期或原生结果未知时不恢复。
未知结果仍锁定整个会话。来源与修改日期见根 NOTICE.md，继续按 GPL-3.0-only 分发。
这次后续修改尚未进入 dist/gpl-2026-10-05 的不可变阶段安装包/对应源码。


### 启动策略验证补充（2026-10-05）

主仓库已补齐 bootstrap 服务健康、接受的 Sidecar 会话许可、GUI capability/attention
读取，以及明确失败/取消维护后的 allowance 恢复。核心 667 项、GUI 320 项及启动
19 项回归通过，严格 Clippy、生产 GUI 编译和 19 项 GPL/对应源码检查通过。GUI
整套在 4 个测试线程下复验通过；首轮全并行的两项旧等待超时与验证边界见
`docs/development/service-sidecar-alignment.md`。本次后续修改未进入旧阶段安装包；
新的二进制仍须配套导出同版完整对应源码，没有执行真实 TUN 或三平台安装验收。


## 主应用原生 IPC 与所有权恢复验证（2026-10-05）

新增 `zenclash-core/tests/native_service_session.rs`，通过 `service-ipc-tests` 启动
真实 Fork IPC supervisor 和隔离模拟内核，使用与 GUI 相同的 CoreSession 初始化与
配置事务。验证启动保存、Service 运行模式、原生 HTTP、Stage/Reload 保持 PID 和 owner
generation、PAC 503/200/503 与地址保持，以及旧应用失去所有权后不能控制或停止替换
会话；原生服务运行时 Sidecar 锁被拒绝，退出后可获取。

测试最初只启动监听器，服务 lifecycle 保持 Starting，已改为调用生产使用的 supervisor；
没有为这一 fixture 问题更改产品的运行事实判定。

复制上游 service.rs 的 owner_recovery_policy 与持续控制通道失败健康观察。后台 owner
supervisor 按三平台策略清理属于本应用的代理，macOS 在被替换或服务不可达时保留机器级
代理。上游原始 LICENSE、作者、UPSTREAM 文件哈希仍保留，新增符号和日期已记录于
两个 NOTICE 和集成 Fork 的 UPSTREAM.json。旧会话不把外部 generation 当作新授权。

验证结果：核心 667 项通过、5 项忽略；集成库 118 项通过；主应用原生 IPC 1 项通过，
共享 fixture 的集成库原生 IPC 1 项复验通过。core/integration/UI 全 target 和 feature
严格 Clippy、默认生产 GUI cargo check，以及 19 项 GPL/源码/GeoData 回归通过。
此前 GUI 320 项与启动 19 项通过，本轮未修改 GUI 源码。

生产打包禁止启用 `service-ipc-tests`、`ipc-tests`、服务 `test` 或 development-channel；
不能使用 `--all-features` 构建发布二进制。隔离测试不修改系统服务注册、不安装服务、不
启动真实 TUN，不等同于真实 Mihomo、Windows 整包安装或 macOS/Linux 实机验收。

当前后续源码计划另存于 `dist/gpl-2026-10-05-native-owner/`，只有配套安装包、完整对应
源码及 BUILD-MANIFEST.json 全部生成并验证后才构成交付。旧 `dist/gpl-2026-10-05/`
保持为以前的匹配文件，不将旧归档重新标为当前二进制的对应源码。
