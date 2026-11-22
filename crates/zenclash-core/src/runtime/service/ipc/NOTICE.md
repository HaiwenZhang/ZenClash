# Fork 来源与修改声明

本模块（原 zenclash-service-integration crate）的应用服务集成源码复制自 Clash Verge Rev 的 `src-tauri/src/core`，本地上游清单版本为 2.5.7。原清单作者为 zzzgydi、Tunglies、wonfen、MystiPanda，保留其他原贡献者的权利及已有声明。来源 https://github.com/clash-verge-rev/clash-verge-rev 。完整 GPL 第 3 版许可证原样保存在 LICENSE。

ZenClash contributors 于 **2026-10-04** 修改本源码：服务依赖名称改为 zenclash-service；将日志接入 tracing、YAML 接入现有 workspace 的 serde_yaml；应用数据根目录由调用方显式传入；运行模式类型移到集成 crate；按 ZenClash 的独立会话实例管理上游所有者会话，不依赖 Tauri 窗口或全局配置对象。临时文件与缓存分配前缀改为 ZenClash。

UPSTREAM.json 记录每个原始文件的 SHA-256、复制符号及适配流程；不虚构未验证的 Git 提交号。本 Fork 及修改以 GPL-3.0-only 发布；无担保，完整条款以 LICENSE 为准。二进制分发时必须保留原始及修改声明，并按 GPL 提供包含 ZenClash 集成改动的完整对应源码。

原服务 IPC 项目的 mock_binary 模拟内核复制用于隔离 IPC 回归；其上游作者包括 Tunglies，来源和文件哈希见 UPSTREAM.json。私有目录重建时的保留/重建文件列表适配为 ZenClash 的令牌及 instance.lock；不使用 Tauri 的 singleton-instance 文件名。

追加复制完整 runstate/mod.rs 与 owner.rs、env.rs 的接口和测试环境，并复制三平台注册探测。Tauri 全局副作用通过 NativeEnv/RunStateHost 交由 ZenClash core/PAC/UI 所有者实现；状态按应用会话持有，权限提升使用后台 blocking worker。macOS 探测只检查 ZenClash，不检测或维护旧 Clash Verge helper。构建只使用本仓库维护的 crates，不依赖 examples 或上游服务仓库。

Mihomo 控制通道使用上游应用的 LocalSocket 接入方式；移入此前 ZenClash 自己的 native HTTP 传输和测试，增加直接 native WebSocket，删去旧服务请求转发的协议依赖。服务凭据/会话检查和实际内核 PID 来自本地 Fork。原生传输适配属于 ZenClash 修改，不伪称为 tauri-plugin-mihomo 复制代码，构建不依赖该插件。

2026-10-04 追加 ZenClash 客户端适配：HTTP 保留原始响应字节，WebSocket 直接使用内核 socket；原生错误保留发送边界及不确定结果；只向当前会话公开已认证的内核状态缓存。该缓存不发起 GUI 同步 I/O，并在会话替换、停止或状态查询失败时失效。

2026-10-04 追加主程序运行配置适配：按原生 Start/Stage/HTTP load 流程对接；ZenClash 自己持有配置保存和回滚状态，不向服务虚构版本或 Commit RPC。provider 缓存分块读回、长度/mtime 校验和 hex 解码从上游 service.rs 复制，改为有预算的内存读回，并在 Stop 清除会话证明前完成导出；额外拒绝非 ASCII hex，避免字符串切片 panic。

2026-10-05 追加安装与卸载接入：从上游 service.rs 复制安装/重装与三平台卸载流程，改为使用本地 Fork 的三个工具和后台 worker。Windows 只把系统显式取消码 1223 判为授权取消；Linux 保留 pkexec/sudo 选择，macOS 将脚本与提示作为 argv 传入。客户端保留未确认 Start 的提议令牌，只允许经服务验证后的 Stop，不将 status 的 generation 当作新的控制授权。GUI 接入使用实际五种健康状态，并让健康检查和启动使用同一个所选内核。

2026-10-05 追加上游 service.rs 的 OwnerRecoveryPolicy、owner_recovery_policy 和
mark_service_unavailable_after_owner_loss，接入 ZenClash 的后台 owner supervisor。
Windows/Linux 清理仍属于本应用的代理；macOS 在 displaced/transport failure 时保留
机器级代理，只在 same-owner failure 时允许清理。持续控制通道失败发布 Unavailable，
不将别的会话的 generation 当作新控制授权。本修改继续按 GPL-3.0-only 分发。

2026-10-05 将隔离 native-controller fixture 提取为测试 feature 专属模块，共用 CLI
验证及 HTTP/WebSocket，实现主应用启动、配置保存/重载、PAC、所有权替换和退出回归。
生产包不得启用 service-ipc-tests/ipc-tests；模拟内核不代表真实 Mihomo 或 TUN 验收。

2026-10-08（修改者：ZenClash contributors）：按用户要求将原独立 crate 合并为
zenclash-core::service，迁移 source、RunState、认证、原生传输和所有测试；调整模块路径及
测试特性，不改变服务协议或产品身份。模拟内核统一由 core 的 service-ipc-tests 门控。
保留完整 LICENSE、原作者及 UPSTREAM.json 的原始来源 hash；补充公开接口文档以遵循
core 的 missing_docs 检查。许可打包位置改为 licenses/zenclash-core-service。
