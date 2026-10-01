# 三平台 TUN 服务实施记录

本记录对应 [开发计划](tun-service-plan.md)。记录日期：2026-10-01；工作环境：Windows，Rust `x86_64-pc-windows-msvc`，debug 构建。尚未完成整个计划，阶段清单保持未勾选。

## 开发方向调整与当前差异

后续采用“直接移植上游有用代码，再按 ZenClash 当前实现适配”的方式，见 [上游代码移植计划](tun-service-upstream-migration.md)。已经完成源码比对，资源差异规划、路径规则、有界重试、provider 缓存回读及平台服务补缺列为优先事项。尚未完成这些移植；本次修订仅更新开发文档，不改变源码、依赖或发布流程。

当前未收尾阶段的实际证据如下；下文已通过结果仍只对应各自历史阶段：

- CoreSession 实际 owner 生命周期测试最近一次为 **8 通过、3 失败**。重启、网络恢复和自动恢复暴露真实 launch config 写入作用域遗漏；后续修复已保存，尚未重新编译、测试或完成本阶段审查。
- UI 实际 owner 来源测试编译成功，**0 通过、1 失败**：普通子进程 A 切换到 B 后，页面内核来源仍为 `None`。这是界面绑定行为证据，不是 Mihomo/TUN 验收。
- 服务 partial PATCH 阶段已有协议 v2、candidate 和资源共享相关改动，以及三项修前行为失败证据；尚无本阶段完整绿结果。当前 `server.rs` 声明的 `patch_tests` 模块文件尚缺，需补齐后再检查最新树；此前 service check 不覆盖这些后续差异。

实施先收尾上述差异，再按移植批次推进。不能将源码比对、文档调整或已保存但未验证的修复标记为功能完成。

## 已落地的基础实现

- 新增 `crates/zenclash-service`，客户端默认 feature 与服务端 `server` feature 分开。第三方包复用已有锁定版本；服务端将已有 `hyper`、`hyper-util`、`http-body-util` 作为直接依赖，用于在经过身份核验的同一原生连接上发送 HTTP，请求不经过 TCP 网关。
- 共享协议实现有界帧、协议握手、OS 身份绑定、随机会话凭证、generation、严格递增序号、30 秒租约及单所有者占用。客户端心跳、连接取消和状态缓存不在 GPUI 渲染路径执行。
- 暂存配置及资源分别有 4 MiB、单资源 128 MiB、总资源 256 MiB 预算；资源最多 256 项、每块 256 KiB。路径限定在 `assets/`，拒绝路径穿越、设备名及不受控 TLS 文件路径。配置移除用户指定控制器和外部 UI，由服务生成私有内核控制器。
- 安装元数据原子写入，未知 schema 拒绝覆盖。维护入口限定安装、修复、启动和卸载；安装副本具有受保护路径和管理员批准的摘要。
- 维护事务记录采用 schema 2，并严格读取旧 schema 1。显式修复在没有完整批准归档时保存原文件或缺失状态作为诊断回退；失败后恢复诊断文件和原元数据、注销服务，不启动损坏文件。完整批准归档存在时仍优先使用归档回退。
- Windows 模块实现 SCM、UAC、命名管道身份与 ACL 校验、固定源文件锁定，以及创建时原子加入 Job 的子进程。SCM 停止后等待真实宿主退出，再允许替换服务文件。
- Unix 模块实现 root 保护目录、socket 对端身份、systemd/Polkit 和 launchd 管理入口及对应资源。Linux 子进程设置父进程死亡信号；macOS 保留 launchd 的进程组回收语义。
- 服务执行循环实现会话所有者存活检测、过期回收、active/candidate 配置和一个有界待清理槽、批准内核启动、停止、状态及有界日志。普通 API 转发使用业务白名单和私密字段过滤。
- 配置校验使用独立的受保护文件；重复校验不覆盖正式运行配置、私有控制器凭据和资源。正式配置已生成的 revision 不再允许追加上传，继续修改需要新的 revision。取消和失败清理、残留保护以及两份配置的预算已有行为测试，独立首审未发现新的功能性 bug 或重大漏洞。
- Linux DEB/RPM 构建已增加服务二进制、unit 和 Polkit policy，缺失服务产物时构建失败。暂存行为和独立首审通过；原生包的安装、升级、卸载协调仍未验收，见 [服务打包记录](tun-service-packaging.md)。
- Windows 普通用户安装包携带服务 helper；macOS App 携带 helper 与 LaunchDaemon 资源，helper 单独签名验证后再签名 App。两平台暂存及失败阻断行为、各自独立首审通过。实际 ISCC、Mach-O 签名、授权及安装流程仍待平台验收。

`zenclash-core` 已有共享控制器绑定、服务传输及完整配置事务。配置持久化的完成任务持有事务权限，调用方取消等待不会使保存和回滚并行。提交确认丢失时保留已保存结果，UI 提供独立确认入口。备份外层事务已覆盖取消、退出协调及恢复权限隔离，失败恢复保留可重试快照；后续部分配置修改与旧快照的一致性仍待完成。控制器绑定的写入租约和旧 owner 释放已修复，`CoreSession` 旧进程字段已移除，新的所有权、动态权限和 UI 消费接口正在迁移。本轮先取得 core all-targets check 通过，生命周期测试仍有失败，追加修复后尚未重新验证；详细结果见上方当前差异。尚未形成可用的安装并开启流程，不能据此宣称应用已支持服务 TUN。调用点、事务和未完成边界见 [core 接入文档](tun-service-core-integration.md)，资源布局见 [资源与状态目录](tun-service-resource-layout.md)，部分修改的实施约束见 [部分配置事务](tun-service-partial-config.md)。

## 分阶段验证证据

下列结果对应已完成检查的阶段。工作区仍在修改，不能把前一阶段的通过结果视为后续差异或整个 workspace 已通过。

| 验证 | 结果与证据边界 |
| --- | --- |
| `cargo check -p zenclash-service --all-targets --all-features --locked` | Windows 集成编译通过 |
| `cargo clippy -p zenclash-service --all-targets --all-features --locked -- -D warnings` | Windows 通过，无需放宽 lint |
| `cargo clippy -p zenclash-service --all-targets --locked -- -D warnings` | Windows 默认客户端 feature 通过 |
| `cargo test -p zenclash-service --all-features --locked` | 诊断修复及严格事务记录解码合并后，107 项通过；包括真实 Windows 普通用户命名管道、进程身份、文件锁定、ACL 和 Job 测试。未运行 UAC 或真实服务安装 |
| 维护诊断回退阶段 | 事务记录测试 22 项通过；包含损坏/缺失文件、无批准归档、旧 schema、重复字段、恢复与保留证据。check、严格 clippy 和相关格式检查通过；独立首审未发现新的功能性 bug 或重大漏洞 |
| 配置校验隔离阶段 | 服务 all-targets/all-features check、严格 clippy 和相关格式检查通过，服务测试 117 项全部通过。10 项新增行为测试包含正式配置及资源保持、取消清理、残留保护、读取边界、上传冻结与配置预算；本阶段独立首审未发现新的功能性 bug 或重大漏洞，尚无真实 Mihomo 校验与服务安装证据 |
| 原生 HTTP 传输阶段 | Windows service 全 targets/features check 与严格 clippy 通过；8 项传输行为测试通过，包括伪管道服务零秘密/请求体泄漏、响应预算、连接取消和截止时间。独立首审未发现新的功能性 bug 或重大漏洞 |
| core 共享传输阶段 | core/service check 与严格 clippy 通过；core client 23 项与 service client 7 项通过。超时结果未知和跨绑定缓存已有回归测试；后续完整配置事务仍需单独验证与审查 |
| core 完整配置事务阶段 | 普通恢复与备份恢复权限隔离、未知修改保留失败恢复上下文修复后，core 单元测试 439 项通过、2 项忽略，core all-targets check 与严格 clippy 通过。备份相关测试 21 项通过；包含先失败的行为测试及实际公共调用路径复核。该结果不包含后续控制器所有权差异，也不等于整个 P3 已完成 |
| UI 服务传输适配阶段 | UI all-targets check、严格 clippy 通过；ProfileService 测试 8 项、既有恢复测试 5 项及 i18n 测试 3 项通过。确认标识在模式改变后保持有效、托盘保存结果保留警告的两项回归分别通过；两轮审查完成。未执行真实窗口和服务安装流程验收 |
| UI 备份恢复重试阶段 | 新增 6 项行为测试通过；备份测试 15 项、ProfileService 8 项、busy 4 项、i18n 3 项通过。UI all-targets check、严格 clippy 和相关格式检查通过；本阶段独立首审未发现功能性 bug 或重大漏洞。覆盖重复请求、取消等待、导航、旧结果覆盖、恢复成功但刷新失败及区域内 Tab/Enter；真实窗口布局与初始焦点尚未验收 |
| 控制器与内核所有权阶段 | 首审发现的写入租约授权失配与绑定锁内进程析构阻塞已通过实际失败用例修复。所有权测试 12 项通过，包含仍持旧引用时停止旧 child 及同 owner 重绑保留运行；caller 调度 4 项通过。第二次最终审查另发现 CoreSession 旧进程引用及带 profile 模式零发送分类问题。后者已有 3 项先失败的实际调度测试修复通过，最新 core 450 项通过、2 项忽略，core/UI check、严格 clippy、fmt 通过；i18n 最近 3 项通过。旧进程引用仍需完整生命周期接入修复，不能标记本阶段完成；fixture 不等于真实 Mihomo 或服务 TUN 验收 |
| Linux/macOS 默认客户端 `cargo check --all-targets --target ... --locked` | 两目标均通过；不是服务端整体构建或实机验收 |
| Linux/macOS 服务端整体交叉检查 | `x86_64-unknown-linux-gnu`、`x86_64-apple-darwin` 的服务 all-targets/all-features check 和严格 clippy 均通过。服务使用已有 `url` 类型与本地 WebSocket 握手，不引入外部控制器 TLS；core 仍保留 HTTP(S)/WebSocket TLS 功能。未执行链接、原生安装或 TUN 验收；macOS ARM 未验证 |
| 现有 `traffic_capture::tests` | 11 项通过，尚未改造服务执行方式 |
| 现有 `core_session::tests` | Windows 19 项通过；Unix 专属测试未在本机执行 |
| Linux 服务 payload 打包阶段 | Git Bash 下 shell 语法检查、DEB 暂存内容与 helper 缺失测试、RPM spec 安装命令与 helper 缺失测试通过；独立首审未发现新的功能性 bug 或重大漏洞。受控打包工具替身不生成原生包，未执行系统安装 |
| Linux helper 版本校验阶段 | DEB/RPM 的版本空输出各先失败，修复后成功、缺失、空输出、非零退出四种行为分别通过，失败分支没有进入打包工具或产生安装包。服务 all-targets/all-features check、shell 语法及差异检查通过；本阶段独立首审未发现功能性 bug 或重大漏洞。普通 shell fixture 不证明真实 Linux 服务可运行 |
| macOS 服务 payload 打包阶段 | Git Bash 下 shell 语法检查及 App 暂存行为测试通过；缺少 helper 的实际失败用例修复后，ad-hoc/指定签名身份的调用顺序，以及缺失、空文件、版本失败分支通过。独立首审未发现功能性 bug 或重大漏洞；签名、架构探测与 plist 工具替身不证明真实平台签名或授权 |
| Windows 服务 payload 打包阶段 | 实际暂存 helper 字节及版本命令通过；缺失、空文件、构建失败、版本非零退出及空输出共 6 项行为通过，脚本解析和差异检查通过。独立首审未发现功能性 bug 或重大漏洞。Cargo、图标检查及 ISCC 使用替身，普通原生程序只作打包 fixture；尚未生成真实安装包或操作系统服务 |

## 审查修复

集成 `cargo check` 通过后进行了两轮子代理审查。修正了额外 TLS 控制器绕过、部分上传写入失败后重试、同一配置重复启动、Windows 父目录替换、正常停止缺少内核清理、升级前置失败不恢复旧服务、遗留暂存目录回收，以及日志分段绕过秘密脱敏的问题。

日志不对超长行输出秘密片段；跨读取块与行预算边界已有回归测试。正常停止先请求释放 TUN 捕获；Unix 先发送 SIGTERM，有界等待后才强杀。真实网卡、路由和驱动清理仍需平台实机证明。

控制器所有权接入的两轮审查分别发现租约授权、锁内析构，以及旧进程引用和模式错误分类问题。发送前 `StaleBinding` 仅恢复缓存，不发控制器回滚、不推进业务 generation；缓存恢复出现真实 I/O 故障时也不伪报运行时已变更。PATCH 已发送成功、后续回读因绑定改变而被拒绝时，改为结果未知的 `StaleTransport`。根代理已复核这两种阶段的生产路径与实际请求测试；该修改未进行第三次独立审查。

## 后续工作与未验证项

先按 [移植计划](tun-service-upstream-migration.md) 收尾当前差异并记录来源；以下剩余目标继续适用，优先移植能满足目标的上游代码及行为测试后适配。

1. 完成备份恢复上下文与后续部分配置修改的一致性，确保旧恢复请求不会覆盖已经提交的新模式或 TUN 设置。
2. 为 `CoreSession` 接入本地与服务内核所有权，并与控制器绑定一起发布；覆盖停止结果未知、仅按已接受版本重启、正常退出与异常断线，防止服务内核被当成外部控制器。
3. 完成配置部分更新与已接受资源包的一致性、内核升级、持久节点身份迁移及运行目录预算；迁移旧 setuid 权限。
4. 实现应用级安装任务与待执行 TUN 意图、安装并开启对话框、服务状态、修复和卸载，补双语文案与键盘流程。
5. 接入三平台打包和维护脚本，更新 README 双语说明及平台安装文档。
6. 在 Windows、macOS 和 Linux 对真实服务管理器、管理员授权、真实 Mihomo 和 TUN 执行计划中的完整验收矩阵，包括异常退出、升级及卸载恢复。

本阶段没有安装系统服务、修改主机 TUN 或部署系统目录文件。macOS bootstrap 的系统工具、FIFO/符号链接拒绝和授权行为仍需 macOS 实机验收。性能和 UI 响应性尚未测量，不作资源占用或交互性能结论。
