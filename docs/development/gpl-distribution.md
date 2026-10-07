# GPL-3.0 分发与对应源码

ZenClash 整体、服务 Fork 和应用集成 Fork 均按 GPL-3.0-only 发布。
完整文本优先于本文：[GNU GPL v3](https://www.gnu.org/licenses/gpl-3.0.html)。
版权、许可、无担保声明不能因改名删除；修改者和日期已记录在根 NOTICE.md 和两个 Fork 的 NOTICE.md 中。
每个 Fork 的原始 LICENSE 原样保留，UPSTREAM.json 是来源追溯记录，不能替代对应源码。

网上分发采用 GPL 第 6(d) 节的方式：在二进制下载处同时免费提供该构建的完整对应源码附件。
必须包括 ZenClash 主程序、服务与集成修改、Cargo.lock、构建/安装脚本和必要资源，以及非系统依赖源码。
只链接未修改的上游仓库、只提供服务 crate、或者只有尚不存在的源码链接均不够。
源代码的接收者保有 GPL 的复制、修改和再分发权利，安装协议不得另加限制。

## 发布与手工构建

许可证暂存、对应源码导出及 GeoData 源码收集脚本已移除。原生打包直接复制根 LICENSE；GeoData 通过下载脚本获取，或由 ZENCLASH_GEODATA_FILE 指定。发布所需的依赖声明和对应源码附件需另行准备。

## 法律声明与修改版本安装

保留源作品的法律声明；交互界面的声明遵循 GPL 第 5(d) 节，包括原作品已有界面的例外。
安装包展示完整 GPL，安装后文件说明许可、原作者和无担保条款。服务本身没有交互式 GUI。
GUI 设置页的“许可证与来源”入口已嵌入完整 GPL 和两个 Fork 的声明，无需联网或运行内核即可阅读；这不代表整个 GUI 已实机验收。

适用第 6 节 User Product 条款时，提供其要求的 Installation Information。
当前打包脚本包含安装路径、服务注册、权限提升和签名方法；macOS 支持自行 ad-hoc 签名，
Windows/Linux 不要求持有本项目的发行签名私钥即可安装自编译版本。
正常 OS 管理员权限和系统信任设置仍适用；后续不能增加只允许发行者签名的限制而不给出所需安装信息。

## 当前验证边界（2026-10-05）

Fork 服务本身的命名适配和独立测试已完成，主程序已改用实际 Fork API，Windows GUI 编译及 17 项启动回归通过。共享生命周期、异常维修及最终安装包/对应源码重建验收仍未完成，不能发布为全部验收通过的应用。
许可暂存、错误拒绝、源码快照及打包测试不替代整应用构建或三平台实机验证。
此文描述的源码归档 CI 需要在已提交的干净发行树实际运行；未宣称当前未提交源码已经生成、发布或通过重建验收。


## 补充依赖声明与 GeoData 核对（2026-10-05）

当前 Windows 锁文件解析的 623 个第三方包已核对实际声明文件。
`resources/licenses/rust-supplemental` 补齐 27 个 crate 发布包省略的独立许可文本；
清单包含原始 manifest、版本/源码修订及文件哈希。多数文件从明确提交原样取得；
只声明 MIT 而未提供独立全文的包保留发布者 manifest/README/实际声明，另附标准 MIT 条款，
不伪称这些条款是上游原始 LICENSE。terminfo 的完整 WTFPL 从实际源码文件头保留。
源码导出会核对补充内容与 vendored 包的 manifest、VCS 修订和哈希，升级依赖时必须重新审计。

GeoData 不能根据服务 crate 的 GPL 标记统一改写许可。当前默认的
MetaCubeX/meta-rules-dat 上游工作流把 Loyalsoldier/geoip 的 geoip.dat 转为 geoip.metadb；
前者仓库包含 GPL，后者声明 CC-BY-SA-4.0 和 GPL-3.0，并要求保留 MaxMind GeoLite2 来源。
在正式安装包分发前，必须记录实际 GeoData 字节的哈希和下载来源，带齐实际数据/转换来源的
声明、许可证和适用对应内容；不能只附 MetaCubeX 仓库的 GPL 而漏掉底层数据声明。
`latest` 是可变引用，应一次取得并在各平台复用相同资源，不能视作不可变源码版本。
当前未完成 GeoData 分发档案与整个对应源码包的重建验收，尚未发布新安装包。

参考上游原始声明：
- https://github.com/MetaCubeX/meta-rules-dat/blob/master/.github/workflows/run.yml
- https://github.com/MetaCubeX/meta-rules-dat/blob/master/LICENSE
- https://github.com/Loyalsoldier/geoip#license


## 本地交付验证结果（2026-10-05）

`dist/gpl-2026-10-05/` 现已实际生成 Windows 安装包和同版对应源码附件、SHA256SUMS.txt、
BUILD-MANIFEST.json。没有发布 GitHub Release，也没有提交或重写主仓库 Git 历史/索引。
当前未提交源文件先复制到独立归档仓库，LOCAL-SNAPSHOT.json 明确区分该快照与主仓库提交。

完整三平台依赖图为 972 个 Rust 第三方包；补充声明覆盖 90 个省略独立许可文本的包。
源码附件约 229 MB，包含 2,112 个实际声明文件、Mihomo 源码/Go vendor，以及 GeoData 原始 dat、
可编辑 CIDR JSON、明确修订的转换工具源码/Go vendor 和原始 CC-BY-SA/GPL/MaxMind 来源声明。
原始数据 CIDR 未修改；格式转换者和日期记入 GeoData/SOURCE.json 与 NOTICE.md。
三平台 packager 优先使用源码作业生成的同一资源，并拒绝其哈希或声明不匹配的 GeoData。

实际压缩包解压后，763 个原仓库文件哈希一致，离线锁定依赖解析和 Windows GUI/服务
`cargo check --offline --locked` 通过。实际 Mihomo 对带 GEOIP 规则的新资源配置校验通过。
许可/源码/GeoData 19 项回归及 Windows、DEB、RPM、macOS 模拟载荷回归通过。
Windows release 和 Inno 安装包构建成功；这不声称发布二进制逐字节可复现、实际安装验收、
三平台实机或真实 TUN 测试已完成。共享生命周期/PAC 和失去服务所有权后的恢复仍需完成；阶段文件未覆盖后续修复。
较早的“尚未生成归档”说明描述的是本地交付执行前的边界，本节及交付目录清单记录实际结果。

## 离线维修后续修改与源码版本（2026-10-05）

当前主仓库已接入显式离线维修能力，原生维护前验证正常用户内核来源，外部控制器与
过期/其他会话请求被拒绝。维修成功后 GUI 再走正常启动流程，失败/取消保留窗口。
核心 641 项、GUI 319 项与启动 19 项测试通过，core/UI 严格 Clippy 通过，
19 项许可/源码/GeoData 回归通过。原始服务 LICENSE 哈希仍与导入样例完全一致。

`dist/gpl-2026-10-05/` 的安装包与源码归档保持为上一阶段的匹配文件，未覆盖或冒充最新树。
这次后续修复尚未重新生成安装包及对应源码。之后交付更新二进制时，必须从更新源码树
重新生成配套源码、构建清单及哈希，不能将旧归档标注为新二进制的对应源码。
完整 RunState/PAC、失去所有权后的恢复和三平台实机验收仍有剩余工作；未运行真实 TUN。


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
