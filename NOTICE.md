# ZenClash — 作者、许可与修改声明

Copyright © 2026 Haiwen Zhang and ZenClash contributors.
ZenClash 整体按 GNU General Public License version 3（GPL-3.0-only）发布。
允许依据该许可使用、复制、修改和再分发；在法律允许范围内不提供保证。
完整许可文本见 LICENSE。

2026-10-07 TUN 实现对照更新（修改者：ZenClash contributors）：从用户提供的 Clash
Verge Rev enhance/tun.rs、config/clash.rs 和 utils/init.rs 适配 TUN 基础字段、条件
fake IP/IPv6 DNS 增强，以及仅缺失时使用的 DNS 服务器默认值；由 ZenClash 有效 YAML
和冻结服务资源共同使用。同步 IPC 服务的原生 Windows 授权及有界内核停止链路。
保留原作者及 GPL-3.0-only；详见 docs/research/clash-verge-tun-implementation.md
和 clash-verge-tun-sources.json。旧阶段二进制与源码配对不包含本次修改。

`crates/zenclash-service` Fork 自
[clash-verge-rev/clash-verge-service-ipc](https://github.com/clash-verge-rev/clash-verge-service-ipc)
的本地 2.7.5 源码副本。保留上游作者 Tunglies、其他贡献者和原始版权、许可及无担保声明。
ZenClash 修改日期：2026-10-04。修改包括 ZenClash 命名、三平台标识和安装路径、内核名、
版本查询及构建适配；详细声明见 crates/zenclash-service/NOTICE.md。

`crates/zenclash-core/src/service`（原独立服务集成 crate）复制并适配
[Clash Verge Rev](https://github.com/clash-verge-rev/clash-verge-rev)
本地 2.5.7 的应用集成代码。上游 Cargo 作者为 zzzgydi、Tunglies、wonfen、MystiPanda；
保留其他原贡献者权利。ZenClash 修改日期：2026-10-04。
源文件、基线哈希和修改说明见该目录的 NOTICE.md 与 UPSTREAM.json。

两个 Fork 均保留各自完整的原始 GPL v3 LICENSE。没有经过验证的上游 Git 提交号，
不把本地快照伪称为特定提交。源文件、锁文件、构建/安装脚本和依赖源码应包含在对应源码归档中。
安装后各 Fork 文件位于 licenses/ 中，依赖声明位于 licenses/dependencies/ 中。

本项目依赖 Mihomo、GPUI、GPUI Kit 等独立第三方项目；其原作者、声明和适用许可证保留。
依赖许可不会因为本项目采用 GPL 而被改写；分发文件须同时履行其适用声明义务。
对应源码的获取和发布操作见 docs/development/gpl-distribution.md。

2026-10-05 追加应用接入与分发适配：ZenClash core/GUI 使用本地 Fork 的所有者会话、原生控制通道及三个维护工具；三平台主程序打包携带原始许可和修改声明，Windows 快捷方式启动 GUI。设置页提供可离线阅读的完整 GPL 及两个 Fork 的来源声明。修改者为 ZenClash contributors，完整对应源码的提供要求见 GPL 分发文档。

2026-10-05 追加离线服务维修接入（修改者：ZenClash contributors）：主程序启动失败后使用
显式应用离线会话保留内核来源，允许独立于 TUN/内核运行的原生安装、维修和卸载；外部
控制器没有该能力，重新绑定或关闭会话使旧请求失效。GUI 在确认维修成功后重新尝试正常
启动，取消或失败保留窗口。本次代码与整个应用继续采用 GPL-3.0-only。此前交付目录中的
安装包及对应源码为不可变阶段快照，不包含这次后续修复；新二进制必须重新配套导出源码。

2026-10-05 追加 PAC 暂停接口（修改者：ZenClash contributors）：参考 Clash Verge Rev
`src-tauri/src/utils/server.rs` 的不可用响应，将 HTTP 503 与原错误正文适配到 ZenClash
已有的 PAC 监听器。共享可用性覆盖当前、替换和恢复保留监听器；暂停不改写系统代理或
PAC URL。来源项目作者包括 zzzgydi、Tunglies、wonfen、MystiPanda 及其他贡献者，
保留原权利，适用 GPL-3.0-only。生命周期调用接入须另外验收，本声明不表示已完成。

2026-10-05 追加持久 RunState 接入（修改者：ZenClash contributors）：CoreSession 持有
复制的 RunStateStore，环境通过当前用户内核文件、原生服务证据及 IPC 观察健康，操作槽
由整个会话共享。运行模式由实际子进程或当前已认证服务缓存决定；启动、切换、维护、
恢复和退出的保留事务控制应用 PAC 实例。GPUI 订阅后台状态并在前台投递刷新，不从后台
直接访问实体。新增胶水代码与 Fork 继续遵循 GPL-3.0-only；本次修改须配套新版本源码。


2026-10-05 追加启动状态与 Sidecar 会话选择接入（修改者：ZenClash contributors）：
将后台启动的原生服务健康观察交给持久 RunStateStore，已选用的本地内核记录上游
Sidecar allowance；Windows Start 拒绝保留其原因，不沿用此前 Ready 结果。GUI 初始化
不重复撤销该选择，并读取复制的 TUN capability/attention 策略。明确取消或失败的维护
按 Clash Verge Rev `src-tauri/src/core/service.rs` 的逻辑恢复之前的 Sidecar allowance，
ZenClash 另外保持同一会话/绑定校验和未知结果锁定。相关来源作者和原 GPL 声明继续保留；
本次修改与整个组合程序继续遵循 GPL-3.0-only，新二进制须配套其完整对应源码。

2026-10-05 追加上游 service.rs 的 OwnerRecoveryPolicy、owner_recovery_policy 和
mark_service_unavailable_after_owner_loss，接入 ZenClash 的后台 owner supervisor。
Windows/Linux 清理仍属于本应用的代理；macOS 在 displaced/transport failure 时保留
机器级代理，只在 same-owner failure 时允许清理。持续控制通道失败发布 Unavailable，
不将别的会话的 generation 当作新控制授权。本修改继续按 GPL-3.0-only 分发。

2026-10-05 将隔离 native-controller fixture 提取为测试 feature 专属模块，共用 CLI
验证及 HTTP/WebSocket，实现主应用启动、配置保存/重载、PAC、所有权替换和退出回归。
生产包不得启用 service-ipc-tests/ipc-tests；模拟内核不代表真实 Mihomo 或 TUN 验收。

2026-10-05 追加 Windows GUI 安装载荷回归（修改者：ZenClash contributors）：打包流程
明确拒绝缺失或空 GUI；Inno 清单单独列出 zenclash.exe，回归检查 GUI 快捷方式和
安装后启动动作，并覆盖 GUI 构建失败。清理只删除已验证在临时根目录中的本次
打包目录。当前开发范围限定 Windows，其他平台留待对应系统开发；许可仍为
GPL-3.0-only，新二进制随附其同版完整对应源码。

2026-10-08 服务模块合并（修改者：ZenClash contributors）：原独立服务集成 crate 已
迁入 zenclash-core::service。原作者、许可证及源文件 hash 在该模块 LICENSE、NOTICE.md
和 UPSTREAM.json 中保留；测试、依赖、GUI 许可展示及安装包许可路径同步迁移。
