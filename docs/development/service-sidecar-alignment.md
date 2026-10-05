# Clash Verge 服务与 Sidecar 行为对齐

> 2026-10-04 迁移说明：下文记录旧自研服务阶段的设计/验收。当前主仓库已替换为本地维护的
> clash-verge-service-ipc Fork，原有 ServiceClient、协议 4、服务端配置 revision 等描述不能代表新 Fork。
> 复制的应用接入代码、已验证模块和未完成主程序迁移见 crates/zenclash-service-integration/README.md。
> 2026-10-05 新 Fork 的 Windows GUI/服务已完成阶段 release 构建和对应源码交付；
> 后续修复仍需重新打包。此前旧实现的交叉编译与 GUI 验收不能用于证明本次 Fork 的生命周期迁移完成。


本轮对照 `examples/clash-verge-rev` 与 `examples/clash-verge-service-ipc` 中的 RunningMode、服务可用性判断、TUN 权限检查、execution guard、Windows 启动等待与延迟接管实现。GUI、特权服务和 Mihomo 是独立角色；启动应用始终创建 GUI，普通代理允许 GUI 直接管理内核。

## 启动策略

| 状态 | Windows | macOS / Linux |
| --- | --- | --- |
| 服务可用 | 默认优先服务，即使 TUN 关闭 | 同左 |
| 未安装服务 | 直接启动普通内核；未提权时关闭已保存的 TUN 开关 | 同左 |
| 已安装但停止、需修复、版本不匹配或连接不可用 | 确认无服务所有者、维护或残留内核后自动 Sidecar | 显示服务状态；用户明确选择“直接运行内核”后确认空闲再 Sidecar |
| 授权失败、维护中或无法确认执行空闲 | 显示 GUI 与错误，禁止启动竞争内核 | 同左 |
| 未提权本地模式启用 TUN | 进入现有服务安装 / 启动和事务接管流程 | 同左 |
| 当前 GUI 原生管理员 / root | 可直接在本地模式启用 TUN | 同左 |

Windows 保存 TUN、服务尚未就绪且 GUI 未提权时，后台最多等候 30 秒，每 200 ms 检查；GUI 不等待这一步。临时 Sidecar 的最终配置投影关闭 TUN，但保留原始请求。后续 120 秒内每 2 秒尝试已就绪服务的事务接管，先停止本地内核；失败走已有恢复流程。明确选择本地、用户改变接管意图或开始退出时停止自动接管。

配置投影覆盖 YAML override 最后生成的 TUN.enable，并持续作用于普通本地模式的配置编辑；显式服务 TUN 事务使用原始配置请求。Unix 的明确本地选择在未提权时持久关闭已保存 TUN，与参考实现一致。`--continue-local` 只作用于当前启动会话。

## 执行互斥与退出

GUI 子进程和服务内核共享机器级执行锁。Windows 使用带显式 ACL 的 Global 命名 mutex，专用线程持有，以满足 mutex 的线程归属；Unix 使用 /tmp 只读文件、O_NOFOLLOW 和排他文件锁。只有确认所拥有内核已退出并回收才释放锁。取消或退出清理遇到失败时，后台回收器保留锁，避免误报空闲。

允许 Sidecar 前先读取服务 Inspection；不可连接时检查原生服务宿主 / Windows SCM 和受保护服务目录的残留 Mihomo，再探测执行锁。Inspection 不获取、续期或释放服务租约。协议由 3 升级至 4；旧服务需通过现有服务修复操作更新，不能把协议不兼容等同于安全空闲。

GUI 使用不连接其他控制器的临时离线会话先创建窗口。实际初始化在线程池执行；完成后在 GPUI 线程发布服务、启动回执和后台监控。临时窗口不启动自动内核运行或系统代理恢复。应用关闭后，主进程等待初始化收尾并清理其拥有的会话，再允许重启。

## 验证范围

Windows 执行工作区测试、真实跨线程 / 跨进程锁测试与严格 Clippy。三平台启动策略矩阵使用确定性测试；服务库分别通过 Linux x86_64、macOS ARM64 / x86_64 的交叉编译检查。Windows GUI 与真实普通 Mihomo 的结果记录于 `windows-acceptance-2026-10-04.md`。

按用户要求，本轮不做真实 TUN 操作。macOS 和 Linux 没有可用实机，交叉编译与策略测试不等于原生 GUI、launchd / systemd 或 TUN 实机验收。


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

### PAC HTTP 基础层验证

2026-10-05 Windows 主仓库验证：13 项 PAC 本地真实 HTTP 测试通过；核心库完整回归
650 通过、0 失败、5 忽略；核心所有目标/特性严格 Clippy、Windows GUI cargo check
和 19 项 GPL/对应源码打包回归通过。保留服务 LICENSE 的 SHA-256 与原始上游示例一致。
测试覆盖暂停/恢复、clone 共享、监听器替换/恢复保留、HEAD、未知路由、分段请求、
停止后重建及最后 owner 的监听器回收。本次没有安装服务、改写系统代理或运行 TUN。
上述结果不表示持久 RunState 和应用生命周期接线已完成，也不表示既有阶段安装包含有
这些新增代码；新包必须重新构建并配套导出同版完整对应源码。

## 持久 RunState 应用接入验证（2026-10-05）

本轮 CoreSession 使用复制的 RunStateStore 作为服务健康、pending action、运行模式与
操作槽的会话状态源。环境在后台验证当前所选内核 SHA-256，并委托已有 NativeEnv 进行
三平台注册和真实 IPC 探测；重新绑定使旧健康观察失效。多个独立 ServiceManager 不能
绕过同一会话的操作槽；前台取消等待仍保留后台完成，原生完成未知或完成任务异常使
整个会话保持 Unconfirmed。明确授权取消继续保留原生类型，不被泛化为成功或普通失败。

应用 PAC 的可用性由实际子进程观察或当前认证的原生服务缓存决定，后者还须确认
ServiceLifecycleState::Running 且运行事务可观察。没有把服务健康 Ready 直接当作
内核已运行。嵌套事务须全部结束才能恢复 PAC；丢失所有权先关闭 PAC 再异步释放捕获。
GPUI 通过现有前台任务订阅状态刷新，读取状态不发起 I/O 或直接访问后台实体。

Windows 主仓库回归：核心 662 通过、0 失败、5 忽略；GUI 319 通过、0 失败、1 忽略，
GUI 启动 19 通过；core/UI 全部目标和特性严格 Clippy、生产 GUI cargo check、19 项
GPL/对应源码回归通过。新增 12 项测试覆盖真实子进程停止与 HTTP PAC 503、嵌套暂停、
外部/离线模式、重新绑定、共享状态发布、独立管理器、取消等待及未知授权结果。
子进程测试使用自编译的隔离 fixture，不等于真实 Mihomo、已安装服务或 TUN 实机测试。

仍需完成/验收：Sidecar 会话许可与能力策略的所有调用点、真实原生 Service 模式在
应用层的 IPC 验收，以及服务所有权丢失后的恢复流程。当前修改没有进入此前阶段
安装包/对应源码；必须重新构建和导出匹配源码。Windows/macOS/Linux 实机及真实安装
不能仅用单元测试或编译结果代替。本轮未执行系统服务安装、系统代理写入或真实 TUN。


## 启动 Sidecar 与维护失败回滚验证（2026-10-05）

启动健康观察已经传入当前 CoreSession，已选用的 Sidecar 记录复制的上游会话许可；
GUI 初始化只在健康 Unknown 时补充探测，避免立即撤销此前的选择。Windows 从
Start 失败后回退会记录真实失败原因，外部控制器不能借此获得服务能力。离线维修
保留健康事实，但不宣布内核运行或接受 Sidecar。GUI 的授权标签和继续本地运行
入口读取复制的 tun_capable/service_needs_attention；已接受 Sidecar 不重复询问，
服务维修保留键盘可达性。明确取消/失败后恢复原 allowance 的上游行为也已接通；
已 Ready、会话过期或原生结果未知时不恢复。修改日期、作者和来源见根 NOTICE.md。

主仓库最终验证：核心 667 通过、0 失败、5 忽略；GUI 320 通过、0 失败、1 忽略，
GUI 启动 19 通过；core/UI 全部目标和特性严格 Clippy、生产默认 GUI cargo check、
19 项 GPL/对应源码回归及 diff 空白检查通过。GUI 全并行首轮出现两个旧等待超时，
降低为 4 个测试线程后整套通过；没有改动这两项业务实现或放宽其超时断言。
新增 6 项测试覆盖实际子进程所属会话的启动状态、外部/离线边界、取消和未知维护结果，
以及真实 GPUI 窗口中已接受 Sidecar 后继续本地入口的隐藏与维修按钮键盘可达性。

服务 LICENSE 与原始示例的 SHA-256 均为
`81cbae84a29ce7e770bf2bc7b178e50bda0ce8de6067aba661b0bc7b05b562f8`。
本次仍未安装系统服务、写入系统代理或执行真实 TUN。服务模式应用层真实 IPC、
失去所有权后的恢复、其他生命周期策略调用点和新安装包/同版源码重建仍需收尾；
macOS/Linux 实机结果不能从 Windows 回归推断。旧阶段安装包不包含本次后续修改。


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
