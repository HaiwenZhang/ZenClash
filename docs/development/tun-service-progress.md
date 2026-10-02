# 三平台 TUN 服务实施记录

本记录对应 [开发计划](tun-service-plan.md)。修订日期：2026-10-02；主工作环境：Windows，Rust `x86_64-pc-windows-msvc`。行为测试使用 debug 构建，release helper 与普通 WSL Linux 执行证据分别注明。尚未完成整个计划，阶段清单保持未勾选。测试计数对应下述阶段终态；后续差异仍需重新验证。

## 开发方向调整与当前差异

采用“直接移植上游有用代码，再按 ZenClash 当前实现适配”的方式，见 [上游代码移植计划](tun-service-upstream-migration.md)。Linux SELinux、Windows SCM、资源差异规划及同会话缓存继承已有适配、行为测试与来源记录；provider 底层回读已通过两轮审查，高层保存与恢复、跨会话稳定目录和节点身份导入仍未完成。源码落地、阶段检查通过和三平台验收分别记录，不合并为功能完成。

| 范围 | 最新阶段结果 | 尚缺的交付条件 |
| --- | --- | --- |
| Service 日志流参数 | 只读核对：core 请求等级及 structured 格式，Service transport 只传 stream kind，服务内核固定请求 debug；尚未修改实现或补充回归 | 受限日志选项贯通协议、客户端与服务端；协议字段兼容方案需确认。验证实际请求、格式回退、失败、预算及旧 generation 边界 |
| 当前 Windows release helper | 当前源码重新构建退出 0，版本入口返回 `zenclash-service 0.1.2`；文件大小及 SHA-256 见 [打包记录](tun-service-packaging.md#windows-产物) | 当前环境无 Inno Setup 编译器；不代表安装包、UAC、SCM 或 TUN 验收 |
| macOS helper 版本非空白门禁 | 新脚本批次实际先失败再通过：完整受控打包 fixture、bash 语法和差异检查通过，独立首审 PASS（1/2）；拒绝时不签名、不替换已有暂存内容 | 使用工具替身，不代表真实 Mach-O、codesign 或 Gatekeeper 验收 |
| Managed Local 配置 TUN 准入 | 只读调用链核对发现最终 effective `tun.enable=true` 可进入 Local 热载/PATCH/重启；未发现相应准入回归，本轮未改代码或执行 TUN | 确认拒绝并提示显式服务开启，或纳入服务交接事务；覆盖配置、override、备份、更新与自动恢复，见 [core §5.6](tun-service-core-integration.md#56-managed-local-配置的-tun-准入缺口) |
| 首次 Start 与 Local→Service 交接核心事务 | core 514 通过、0 失败、2 忽略；check、完整 CI 附加 lint 和首次独立审查通过 | 真实管理员服务、Mihomo、TUN 与路由验收 |
| 首次安装并开启的 Manager、页面与托盘 | 收据分类 P2 已修复，第二轮最终审查通过；相关行为、core/UI check 与标准严格 clippy 通过；随后 Windows 完整 UI 库 294 项、binary 11 项通过 | UI CI 附加 lint 及实机验收 |
| provider 底层回读 | 修后相关行为 19 项、完整服务 198 项、check/完整 CI lint 通过；两轮审查结束，最后增量 Linux/macOS service 交叉 check 通过 | 高层 Stop→导出→普通权限保存→启动或释放，以及真实内核验收 |
| Windows SCM 注册身份 | 移植固定注册路径及 owner/DACL 门栓；新增 11 项、完整 Windows service 209 项通过；check、完整 CI lint、fmt、首次独立审查和 Linux/macOS service 交叉 check 通过 | 真实 SCM 注册、维护恢复及删除验收 |
| macOS 服务查询分类 | 上游明确不存在分类已接三维护入口；Windows 分类/调度 10 项、Linux 整组 16 项通过，check/完整 CI lint、相关 fmt 和首审通过 | 真实 launchd 与已加载 label 归属验收 |
| macOS 固定 plist 维护准入 | 修前 3 通过/5 失败，修后 Windows 11 项、普通 WSL 14 项通过；该阶段完整 Windows service 230 项通过。Windows/macOS Intel check 与完整 CI lint、相关 fmt/diff 和首次独立审查 PASS（1/2） | 已加载 job 的实际执行归属与原生验收；本批不是管理员 CLI 或 launchd 实测 |
| 首页 TUN 统一入口 | P2 反馈修复的完整 Windows UI 299 项通过，第二轮最终审查 PASS；随后详情改用已有 NavigateTun，Home 5 项、相关 service_tun 7 项及 UI check/标准严格 lint/fmt/diff 通过。服务未变，沿用 219 项结果 | 完整 CI lint 仍为原有五项；299 项完整回归早于四行导航尾修，真实侧栏、授权与 TUN 未验收 |
| Windows runtime 删除竞争重试 | 移植适配上游 cleanup retry；真实行为红 1 通过/2 失败，修后新增相关 7 项及完整 Windows service 237 项通过。check、默认客户端/server 完整 CI 严格 lint、所属 fmt/diff、Linux/macOS service 交叉 check 与首次独立审查 PASS（1/2） | 每次退休清理波次与 stale-stage 扫描共享三次等待，累计 sleep 175ms；权限/保护错误零重试，失败保留 retired/shared。不是 Release/RPC 总截止或原生服务维护验收；原子替换重试尚未纳入本批 |
| Windows runtime 原子替换竞争重试 | 源占用原生 32、目标占用/ACL 原生 5 的实际基线；2 项行为红修后替换相关 15 项、完整 Windows service 252 项通过。check、默认/server 完整 CI 严格 lint、所属 fmt/diff、Linux/macOS service 交叉 check 通过；首审 P2（exists 吞查询错误）已修复，第二轮最终 PASS（2/2） | 目标共享探测不证明权限已通过；明确保护/只读/身份失败零移动，纯 ACL 无共享证据零等待，混合障碍最多有界等待后仍按原生结果失败。资源与配置一次 wave 共用 175ms sleep；与 cache open 独立预算合计最多 350ms，不是 RPC 截止。真实 Mihomo/SYSTEM 目录验收仍待完成 |
| Unix rename 后失败边界 | 普通 WSL UID1000 下真实父目录 open 拒绝：专项 1 项、当前 Linux 完整 service 207 项通过；Windows/macOS check/完整 CI lint、fmt/diff及本验证批次首审 PASS（1/2）。仅新增测试，无生产改动 | 目标已是新字节而返回原 PermissionDenied；不是 sync_all syscall 故障、崩溃持久性或 macOS 原生验收，见 [移植记录](tun-service-upstream-migration.md#unix-rename-后失败边界回归) |
| 修复与卸载 | 原生维护入口与结果分类已有阶段验证 | CoreSession/捕获/Manager 的完整恢复和授权接线；本地资源物化方案待确认 |
| 重启后直接使用服务 | main/UI 生产接线的首审 P1/P2 已修复并通过第二轮审查；Windows 完整 core 534、Linux 完整 core 577 项通过，各 2 项忽略；初始化 9 项、ProfileService 14 项及 i18n 4 项通过，core/UI check、标准严格 clippy、core 附加 lint 和 Linux core 完整 CI lint 通过 | UI CI 附加 lint 的五项问题、无 owner 恢复界面和三平台真实服务启动仍待完成 |
| 资源持久性与发布 | 三平台 payload 暂存、Linux 包协调已有阶段证据 | 跨会话状态、旧 setuid、内核副本升级、Windows/macOS 维护协调及三平台真实验收 |

已收尾阶段与正在开发的差异如下；下文历史结果仍只对应各自阶段：

- CoreSession 唯一 owner 生命周期阶段完整 core 回归 **483 通过、0 失败、2 忽略**，owner 生命周期 **18 项**、ServiceRuntimeSession **20 项**、捕获门栓 **15 项**通过；Windows all-targets/all-features check、严格 clippy 和两轮独立审查完成。已确认停止意图、停止后维护失败的 generation、授权取消和捕获任务取消均有实际失败再通过的证据。该阶段只保证捕获收尾后发布新 owner；后续首次开启交接的证据见下文，其他切换与维护不在该阶段范围内。
- UI 的实际 A→B 来源切换、停止后的页面状态和跨 generation 延迟回调回归已通过，该阶段两轮独立审查结束。此前完整 UI 为 **252 通过、1 失败**，失败涉及 profile 列表投影；最新 Windows 完整 UI 库已重跑为 **294 通过、0 失败、0 忽略**，binary **11 项通过**，此前失败不是当前结果。check 与标准严格 clippy 通过；完整 CI 附加 lint 仍有五项问题，见下文。GPUI 行为测试不等于真实窗口、Mihomo 或 TUN 验收。
- 服务端部分配置阶段完整服务测试 **160 项通过**，PATCH 模块 **15 项**、部分资源事务 **5 项**通过；Windows all-targets/all-features check、严格 clippy、crate fmt 和两轮独立审查完成。固定 Mihomo `v1.19.30` 的省略 enable、GET 默认字段和不支持局部 PATCH 的顶层字段问题已修复；不支持的字段保留完整配置事务。测试使用受控 HTTP fixture，真实 Mihomo 连接保留仍待验收。
- core 部分配置业务接入已完成阶段验证：**3 项先失败的业务测试修复后通过**，覆盖变化中的源配置不混入缓存、取消等待后持久化与缓存一致、旧备份重试保留后来保存的模式；安全 effective delta 响应与保存收据已接线，另有 **5 项服务局部事务边界测试通过**。首审发现的 **1 项 P2**（停止后未保存 PATCH 候选阻塞 accepted 重启）已通过复用 Restore 修复，第二轮最终审查通过。停止恢复 core **4 项**、service 候选清理与失败重试 **2 项**通过；该阶段完整 core **500 通过、0 失败、2 忽略**（包含 manager 四项生命周期测试）、完整服务 **182 项通过**，两 crate check 与严格 clippy 通过。后续首次 Start 和回读差异不在本结果范围内。
- Windows SCM 上游补缺的行为测试 **13 项通过**，覆盖维护期间阻止自动恢复、固定宿主退出及删除完成等待；check、严格 clippy、fmt 与首次独立审查通过。没有运行真实 SCM 安装、UAC 或系统服务删除。
- 随后 Windows SCM 注册身份批次移植上游 `registered_executable`、`check_service_registration` 和适用的 `review_security`，固定保护目录 helper 与唯一 `run` 参数、own-process/LocalSystem、无依赖及 load-order group；同一 SCM 句柄先检查可信 owner/DACL，再允许注册恢复、启动、停止或删除。拒绝普通用户的修改权限、未知 ACE、null DACL、越界配置及非固定路径；只有原生 1060 允许新建。固定保护根目录仍在时允许缺失 helper 叶子进入已有维护，根目录缺失则拒绝。原生参数解析行为先失败再修复，新增 **11 项通过**，完整 Windows service **209 通过、0 失败**；all-targets/all-features check、默认与服务端完整 CI 严格 lint、fmt 和首次独立审查通过，审查计数 **1/2**。最新 Linux GNU x64/macOS Intel service 交叉 check 通过。原生 Windows 参数解析及 SDDL 测试不等于真实 SCM 安装验收。
- Windows 服务 release 二进制通过 `cargo build --release -p zenclash-service --features server --bin zenclash-service --locked` 构建，实际执行 `--version` 返回 `zenclash-service 0.1.2`、退出码 0。未执行 `run`、安装或卸载；Inno Setup 编译器不在当前环境，尚未生成真实安装包。三平台逐场景观察和通过判据见 [原生验收入口](tun-service-native-validation.md)。Windows GUI 卸载桥接还需共享账户策略及维护 worker 的实际 owner 准入，未接入安装包；普通权限预检不能代替提权后的原子校验。
- 当前维护锁仅互斥维护 worker；服务端 `Acquire` 与业务请求只检查维护 journal，没有检查 `.maintenance.lock`。安装/修复在获锁后先暂存，再发布 journal；卸载不发布 journal。因此新请求拒绝和已有 owner 归属还未形成原子维护准入，仍是待实现项，不能以 UAC 前 health/占用观察、后台 lease 或维护锁存在作为完成证据。
- 平台只读核对还确认 Linux/macOS 注册归属缺口：当前按 unit/job 固定名称操作，注册写入及卸载删除没有核对有效执行命令是否属于本项目。macOS 非零查询误判问题已在下面独立批次修复；实际加载注册与固定 unit/plist 的归属门栓仍需补齐，受保护文件或安装元数据不能代替系统实际注册观察，没有执行系统服务命令。
- macOS 查询分类已直接移植 `classify_launchd_service_probe` 及四项适用测试并接入 start/stop/unregister。实际红 **2 通过、5 失败**，修后 Windows 分类/生产回调 **10 项通过**，普通 WSL Linux 整组 **16 项通过**，其中 **5 项**实际子进程捕获验证双管道、超限、UTF-8、超时及继承管道；另含测试子进程入口，不计作真实 launchd 行为。读取每管道 64 KiB、查询 30 秒、终止后回收最多 5 秒，未知零平台操作、bootout 失败不删 plist；Windows all-targets/all-features check、macOS Intel 交叉 check、两目标完整 CI 严格 lint、相关 fmt/diff 和独立首审 **PASS（1/2）**。没有运行真实 launchd 或修改系统网络，已加载 label 的实际归属仍待后续完成。
- P0 当前调用点补充已写入 [core 接入文档](tun-service-core-integration.md#当前生命周期调用点补充2026-10-02)：区分 ControllerBinding 的实际 owner、CoreSession 业务准入与 Manager 意图，补绑定退休、首次服务交接、动态权限和原生 quit observer。核对发现的首页 Manager 旁路已通过复用服务命令修复；首页新增五项行为、详情应用导航接线及待确认反馈的独立第二轮审查已形成上表的阶段证据。没有执行旧 Unix setuid 授权或真实服务安装。
- Linux SELinux 首批移植的独立 Rust 决策测试 **8 项通过**，bootstrap shell 语法检查通过。该批 service 在 Windows check、Linux GNU x64 和 macOS Intel 的 all-targets/all-features 交叉 check 均通过，包含 Linux 专属执行代码。本批首次独立审查未发现新增功能性 bug 或重大漏洞；真实 SELinux 安装流程待完成，见 [移植批次记录](tun-service-upstream-migration.md#5-移植批次记录)。
- Linux/macOS core 交叉 check 此前分别被 `ring` 构建缺少 `x86_64-linux-gnu-gcc` 和 `cc` 阻断。本轮使用本机已有 WSL Ubuntu GCC/AR 的临时桥接，`cargo check -p zenclash-core --all-targets --all-features --target x86_64-unknown-linux-gnu --locked` 已通过；在 Manager/UI 收据修复、启动 core 和 mode fixture 修复后复验也通过，不覆盖随后 main/UI 接线增量。macOS core 的 C 工具链阻塞仍未解除。另在 WSL Ubuntu 26.04、UID 1000 的普通用户下链接并运行 Linux ELF 服务测试，先取得 **10 项通过**：Linux 平台 3 项、Unix 公共平台 3 项，以及管理员前置拒绝、未受保护安装状态、链接清理拒绝和清理范围限制各 1 项。随后确认当前服务源码冻结且二进制晚于所有服务 Rust 源文件，独立审计整组测试的实际调用范围后，运行同一 Linux 服务库测试二进制，**176 通过、0 失败、0 忽略**。包含实际进程身份、socket 凭据和普通权限文件操作，其余协议/安装事务/调度仍使用受控夹具；不代表 root 服务安装、systemd、Polkit、SELinux 或 TUN 验收。新增开启/启动/清理结果文案后的 i18n 回归 **4 项通过**，包含双语键树与插值验证。

停止后的 Restore 只有在新鲜原生观察确认 `!running`、PID 缺失、candidate/base 匹配时才清理未保存候选。恢复响应丢失后用 Status 核实，无需 Commit(base)；清理失败保留 staged/retired。Finalizing/已保存候选不能倒退，停止结果未知时不盲重发 PATCH 或 Start。协议没有新增 `DiscardRuntimePatch`。

资源首批采用内部内存 manifest，以完成上传的字节摘要和长度为身份，同一会话内按 provider URL 与资源身份继承有界缓存；保持 Validate 后继续上传的既有行为，formal materialization 后才冻结。未发布新落盘格式，跨会话稳定 home、节点身份导入和高层缓存保存仍待完成。

资源新阶段取得 **3 项先失败再通过的行为测试**：完成上传后的同长度替换、资源增大，以及同 URL provider 新缓存未继承。摘要复核、固定句柄复制和缓存规划修复后，runtime 模块 **34 项**、服务 Stage→校验→释放的实际生产调度 **1 项**通过；首次独立审查通过。相同 URL 仅继承可安全映射的已批准资源，候选独立落盘；无法安全映射、资源改变或超预算时保留受限下载路径，不修改旧 accepted。每个 stage 共用三次重试预算；会话磁盘用量观察不是 OS 实时配额。

P4 底层维护结果分类从 **30 通过、3 失败**修复到 **36 项通过**，service all-targets/all-features check 和默认客户端严格 clippy 通过。首审发现的 Linux 已授权执行失败与取消码混同、通用 Interrupted 被当成取消，两项均已修复；实际 production bootstrap 字符串在普通 Bash 的六个场景通过，第二轮独立审查通过。这不包含真实 Polkit 授权或系统安装。超时保留实际维护任务和待确认结果，健康 Ready 不能代替未结束维护任务的确认。

P4 应用级 ServiceManager 的取消等待问题已从 **3 通过、1 失败**修复到生命周期 **4 项通过**，随后 core check 通过。正式 TUN 意图、固定安装源、授权和 CoreSession/捕获入口已接通 App/ProfileService、页面与托盘。首次服务应用的 **3 项先失败行为**（错误使用 Reload、未观察 Start 响应丢失、缺少新鲜状态仍报告成功）已在下述核心阶段修复并验证；Manager/UI 是单独批次，首审发现的收据分类问题已修复并通过第二轮最终审查。

后续首次 Start/交接核心阶段已收尾：初次运行 **3 项**、capture 交接 **3 项**、备份目标 **2 项**通过；retirement 错误覆盖已保存收据和未保存 typed error 的 **2 项实际失败行为修复后通过**，固定双语错误不带内部 worker 诊断。完整 core **514 通过、0 失败、2 忽略**、all-targets/all-features check 和完整 CI 额外严格 lint 通过，本阶段首次独立审查通过。

首次开启 UI 的 **2 项实际 GPUI 行为先失败再通过**，覆盖安装取消保留本地 A/配置、确认前真实 A→B 切换拒绝旧意图；首审前相关行为 **4 项通过**，包含 Tab/Enter/Escape 及关闭后恢复焦点、已保存收据与延迟结果边界，ProfileService 整组 **10 项通过**。首次独立审查发现 **1 项 P2**：Commit 已成功但 capture 回读或旧资源清理失败时，`saved + ReconcileNeeded` 被误作 Commit 待确认，确认按钮可清除尚未核实的故障。修复将真实 `commit_pending` 与独立 `recovery_warning` 分开传递，确认提交不清除捕获或清理警告；生产 ProfileService 回归先失败再通过。修后 core TUN **6 项**、捕获交接 **5 项**、ProfileService **12 项**、UI 相关 **4 项**通过，core/UI all-targets/all-features check、标准严格 clippy、相关 fmt 与差异检查通过。第二轮最终审查通过，本批两轮审查结束。随后完整 UI 库与 binary 已通过最新回归；CI 附加 lint 的 proxy 页面及既有 logs/profile 测试问题仍待收尾，可见实机验收尚未执行。修复与卸载目前只有底层入口和方案，完整用户流程仍未接通。

应用启动的 core 新切片已增加无 Local binary 的普通权限资源准备入口，以及保留保存收据、失败、真实 Commit 待确认和 listener 回退事实的初始化结果。**4 项实际失败行为修复后通过**；`startup_` 相关 **18 项通过**（新 10 项、旧 8 项），core all-targets/all-features check 与 CI 全部额外严格 clippy 通过。完整回归此前为 526 通过、1 失败、2 忽略，旧 mode 取消测试的 accepted TCP stream 出现 Windows `WouldBlock` 后门栓超时；仅在 fixture 接收连接后显式设置阻塞模式，保留原 3 秒超时、准入截止时间和请求断言。修后单项 **1 项通过**，完整 core **527 通过、0 失败、2 忽略**，check 与 CI 严格 lint 再次通过。启动切片独立首审通过；main/UI 接线为随后独立开发批次，不能把 core 切片证据当作应用重启或真实服务启动验收。

随后 main/UI 启动批次已将生产 main 接到私有启动路由：实际初次运行的两个行为测试均失败，分别暴露 Ready 未尝试 Service、Unknown 调用了 Local；修复后路由 **5 项通过**，ProfileService 初始化收据 **2 项通过**。core 启动相关共 **22 项通过**，包含该批新增的状态分类和普通执行文件 **4 项**；core/UI all-targets/all-features check 与标准严格 clippy 通过。Ready 鉴权或首次 Start 失败不进入旧 Local 恢复，Start 未知保留同一 session 供退出收尾；已保存收据、真实 Commit 待确认和独立故障分别传给 App/ProfileService。首审后进一步修复跨次启动与自动暂停问题，结果见下一段。没有可用 owner 的启动目前返回明确错误，可见恢复界面尚未实现；完整应用实机验收仍待完成。

main/UI 首审发现 **1 项 P1、1 项 P2**：保存后的 Commit 待确认启动被自动网络 Stop 清除 applied revision，使确认和恢复无法完成；本次隔离损坏配置后仅内存 Unknown，下次启动可误用默认配置判断 TUN=false。实际初始化 Commit 响应丢失后的 Stop/Restart、确认失败后的网络暂停事实，以及损坏层第二次启动均取得真实失败结果后修复。managed Mihomo 启动改为只读验证三层，保留坏字节供明确恢复；网络暂停先通过同 owner 确认 Finalizing，再发布暂停并释放捕获，实际 Stop 再复核，确认失败零 Stop 且捕获、phase、generation 与停止意图不变，退出仍可 Release。修后初始化 **9 项**、UI binary **11 项**、ProfileService **14 项**、i18n **4 项**、完整 Windows core **534 通过、0 失败、2 忽略**，core/UI check、标准严格 clippy、core 附加严格 lint、相关 fmt 与差异检查通过。第二轮最终审查通过，本批计数 2/2 已结束。随后完整 Windows UI 库 **294 项通过**，Linux 完整 core 及 CI lint 复验通过，见下文；UI 附加 lint 的 proxy/logs/profile 五项问题及可见恢复界面仍待完成。

Windows 和 Linux 普通权限的真实 Mihomo 自动生命周期测试分别 **2 项通过**，覆盖网络暂停/恢复、手动停止抑制恢复、shutdown 拒绝恢复，以及配置源变化后的真实 PID 更换和无效配置保留当前进程。使用官方固定 `v1.19.30`，从 [官方发布 API](https://api.github.com/repos/MetaCubeX/mihomo/releases/tags/v1.19.30) 取得对应平台产物的 SHA-256，校验后只解压到本任务临时目录。Linux 使用现有 WSL Ubuntu 26.04、UID 1000，实际构建并运行 Linux ELF 集成测试；`real_automatic.rs` 自建 `/tmp` 配置/home，controller 使用随机 loopback 端口，TUN 固定关闭。两平台各两项均已在 main/UI 首审修复后再次通过。结果不包含系统代理写入、原生高权限服务或实际 TUN。

新增普通 Local 执行文件检查在 WSL 普通用户下 **4 项通过**，包含真正设置在 `/tmp` 的 setuid/setgid 拒绝及 Linux xattr 读取错误；另用 `stat` 确认测试文件权限位实际为 4700/2700。扩展 Linux 启动相关 26 项时，继承的 HTTP 代理造成一项 loopback fixture 返回 502；仅对测试子进程移除代理变量后，整组 **24 通过、2 失败**，暴露两项 Unix 更新回滚夹具的记录同步问题。修复改为先原子发布完整 payload，再原子发布实际子进程 PID，断言在原有 5 秒界限内等待当前 PID 的记录；保留完整字节、PID 与 generation 断言，不放宽超时。修后 Linux 更新模块 **19 通过、0 失败、2 忽略**，包含新增启动回归的 `startup` 整组 **29 项通过**；Linux core check 通过后，该夹具修复的独立首审通过。

随后在同一 WSL 普通用户环境运行完整 Linux core 库：**577 通过、0 失败、2 忽略**。Windows Rust 工具链经已有 WSL GCC/AR 桥接构建 Linux ELF，测试在 Linux 原生执行；两类调用 `rustc` 的子进程 fixture 使用临时受限桥接将真实 Rust 源编译为 Linux ELF，再在 Linux 执行。测试仅使用本任务 `/tmp` home，代理隔离限于测试子进程；未安装工具链或改永久 PATH。Linux core all-targets/all-features check、标准严格 clippy 和完整 CI 附加 lint 均通过。默认忽略的两项官方网络集成测试保持忽略，没有执行原生高权限安装或 TUN。

Windows 完整 UI 库以 `cargo test -p zenclash-ui --lib --all-features --locked -- --test-threads=2` 重跑：**294 通过、0 失败、0 忽略**；binary 整组 **11 项通过**。完整 CI 附加 lint 仍有五项：proxy 的 `needless_collect`、logs 两处 `redundant_clone` 和一处 `future_not_send`、profile catalog 的 `redundant_clone`。这些问题位于当前服务启动批次之外，不能据标准 clippy 通过宣称完整 UI CI 通过，也不能把行为测试当作可见窗口验收。

provider 缓存回读的新协议、客户端、服务端及有界快照源码已写入，来源见 `UPSTREAM.md`。修前回读相关行为 **16 项通过**，完整服务 **195 项通过**；service all-targets/all-features check、默认和服务端的 CI 全部额外严格 lint、Linux GNU x64/macOS Intel service 交叉 check 及局部 fmt 通过。首审发现被拒绝 Start 提前清除预算的 **1 项 P2**，已通过 **3 项实际失败再通过的 State 测试**修复；修后回读 **19 项通过**、check/完整 CI lint 通过，第二轮最终审查通过。本轮最后增量完整服务回归 **198 通过、0 失败**，Linux GNU x64/macOS Intel service all-targets/all-features 交叉 check 通过。可靠导出要求先由高层确认内核停止；运行中 provider 文件的元数据稳定不保证写入一致性。core 普通权限保存、后续启动和失败恢复仍需接线，不能把底层分块接口当作完整缓存保留功能。

资源与包策略合并阶段完整服务回归 **179 项通过**，默认客户端及服务端严格 clippy、Linux GNU x64/macOS Intel service 交叉 check 通过；该结果早于上述授权分类根修，完整服务测试、服务端严格 clippy 及交叉 check 尚需在新差异后重跑。两个 lint 问题通过借用切片和移动测试模块修复，没有放宽规则。双语文案补齐后 i18n **4 项通过**，文案存在不代表 UI 已接入。这些结果不能代表 workspace 或三平台真实服务验收。

停止恢复阶段取得上述完整服务 **182 项**和严格 clippy 通过；随后的回读阶段已重跑 service 两目标交叉 check。按 CI 额外启用的 `future_not_send`、`missing_errors_doc` 等参数复核发现的公开接口错误说明与帧泛型线程约束缺口已补齐，没有放宽 lint；最新 service 默认及 all-features 的完整 CI lint 参数均通过。帧 **5 项行为测试**、默认 service check 与局部 fmt 通过，该小批独立首审通过；core/UI 新差异仍须另行复验。Windows CI/发布前检查已加入实际授权 bootstrap 的六场景 fixture，本机重跑 **6 场景通过**；Windows payload 暂存检查也重跑 **6 场景通过**，均不代表系统授权或原生安装包验收。

Linux 包升级/最终卸载协调已接入，Windows 普通权限拒绝、共享包策略和生成 DEB/RPM 脚本行为通过，check、严格 clippy 与首次独立审查通过；CI/发布前 Linux 检查已加入包策略测试。目标系统安装卸载尚待验证，见 [打包记录](tun-service-packaging.md#包升级与最终卸载2026-10-02)。没有执行真实系统服务卸载或修改本机网络。

文档校验补充：TUN 专项、macOS 安装与来源记录共 12 份文档，213 个本地路径无缺失；最终 11 行上游源文件摘要一致。中英文 README 的原有 `gpui-kit-migration.md` 导航各有一个缺失目标，HEAD 同样存在该链接；本批只同步 TUN 状态，未改动这项既有导航。

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

`zenclash-core` 已有共享控制器绑定、服务传输及完整配置事务。配置持久化的完成任务持有事务权限，调用方取消等待不会使保存和回滚并行。提交确认丢失时保留已保存结果，UI 提供独立确认入口。备份恢复权限隔离、后来保存的 delta 合并、唯一 owner 和首次开启的同一捕获准入交接均已有阶段验证。首次安装并开启已形成生产接线，尚不能宣称完整三平台功能交付；应用启动、修复/卸载和缓存保存等缺口见上表。调用点、事务和未完成边界见 [core 接入文档](tun-service-core-integration.md)，资源布局见 [资源与状态目录](tun-service-resource-layout.md)，部分修改的实施约束见 [部分配置事务](tun-service-partial-config.md)。

## 分阶段验证证据

下列结果按阶段留存，不按最新测试数量累计为当前 HEAD 的结论。历史行中的待办可能已在后续阶段解决，当前状态以本页首节为准。工作区仍在修改，不能把前一阶段的通过结果视为后续差异或整个 workspace 已通过。

| 验证 | 结果与证据边界 |
| --- | --- |
| 2026-10-02 Windows workspace 联合回归 | `cargo check --workspace --all-targets --all-features --locked` 与 `cargo test --workspace --all-features --locked` 均退出 0。包括首页导航尾修：core 库 534 通过/2 忽略、service 库 230 项、UI 库 299 项、UI binary 11 项和 i18n 4 项；真实内核集成默认忽略。本轮未重跑 workspace clippy，不能宣称完整 CI lint 通过；结果早于后续 Windows runtime 删除重试批次 |
| 2026-10-02 打包与授权脚本复验 | Windows helper payload 6 场景、Linux 授权 bootstrap 在普通 Git Bash 下 6 场景通过；使用独立临时目录及原生普通 fixture，不是真实 ISCC、SCM、Polkit 或管理员安装。UPSTREAM 的 9 行源文件 SHA-256 与本地上游快照一致 |
| Linux 服务库原生执行 | WSL Ubuntu 26.04、UID 1000：服务源码冻结后的 Linux ELF lib/all-features 测试二进制，`--test-threads=2`，176 项通过；直接复用本轮已编译产物，没有并行启动第二个 Cargo。原生身份/socket 与普通文件测试通过，其余包含调度夹具，不是 systemd/Polkit/TUN 验收 |
| main/UI 服务启动接线阶段 | P1/P2 红绿修复后：Windows 完整 core 534 通过/2 忽略、初始化 9 项、UI binary 11 项、ProfileService 14 项及 i18n 4 项通过；core/UI check、标准严格 clippy、core 附加 lint 和第二轮审查通过。随后 Linux 完整 core 577 通过/2 忽略、check/完整 CI lint 通过，Windows 完整 UI 库 294 项通过；UI 附加 lint 仍有五项问题 |
| Linux 新增普通执行文件检查 | WSL Ubuntu 26.04、UID 1000，`cargo test -p zenclash-core --lib --all-features --target x86_64-unknown-linux-gnu --locked service_manager::startup::tests -- --test-threads=1`：4 项通过；TMPDIR=/tmp，实际 setid 位已核对；不执行测试文件或提权 |
| Linux 启动相关原生扩展 | 两项 Unix 更新回滚夹具同步失败修复后，更新模块 19 通过/2 忽略，`startup --test-threads=1` 整组 29 项通过；固定当前子进程 PID 后等待完整记录，字节/PID/generation 断言和原 5 秒界限保持不变；check 后独立首审通过 |
| Linux 完整 core 库原生执行 | WSL Ubuntu 26.04、UID 1000，Linux ELF lib/all-features 测试产物：`--test-threads=2`，577 通过/0 失败/2 默认忽略。测试进程使用独立 `/tmp` home、loopback 代理隔离及临时真实 Linux fixture 编译桥接；没有原生高权限服务或 TUN 验收 |
| Windows 完整 UI 库 | `cargo test -p zenclash-ui --lib --all-features --locked -- --test-threads=2`：294 通过/0 失败/0 忽略。GPUI 行为测试通过，不代表可见窗口、授权弹窗或真实 TUN 验收 |
| Linux 真实 Mihomo 自动生命周期复验 | main/UI 首审修复后，`real_automatic` 两项再次通过；WSL 普通用户、官方 Mihomo v1.19.30、实际 Linux ELF 与子进程，临时 home、TUN 关闭、无系统代理写入 |
| `cargo test -p zenclash-core --test real_automatic --all-features --locked -- --ignored --test-threads=1` | Windows、官方 Mihomo v1.19.30、普通权限：2 项通过。命令前临时设置 `ZENCLASH_MIHOMO_BINARY` 指向已校验的测试产物，结束后恢复环境变量；真实进程、TUN 关闭，不代表服务模式或网卡验收 |
| `cargo check -p zenclash-service --all-targets --all-features --locked` | Windows 集成编译通过 |
| `cargo clippy -p zenclash-service --all-targets --all-features --locked -- -D warnings` | Windows 通过，无需放宽 lint |
| `cargo clippy -p zenclash-service --all-targets --locked -- -D warnings` | Windows 默认客户端 feature 通过 |
| `cargo test -p zenclash-service --all-features --locked` | 诊断修复及严格事务记录解码合并后，107 项通过；包括真实 Windows 普通用户命名管道、进程身份、文件锁定、ACL 和 Job 测试。未运行 UAC 或真实服务安装 |
| 维护诊断回退阶段 | 事务记录测试 22 项通过；包含损坏/缺失文件、无批准归档、旧 schema、重复字段、恢复与保留证据。check、严格 clippy 和相关格式检查通过；独立首审未发现新的功能性 bug 或重大漏洞 |
| 配置校验隔离阶段 | 服务 all-targets/all-features check、严格 clippy 和相关格式检查通过，服务测试 117 项全部通过。10 项新增行为测试包含正式配置及资源保持、取消清理、残留保护、读取边界、上传冻结与配置预算；本阶段独立首审未发现新的功能性 bug 或重大漏洞，尚无真实 Mihomo 校验与服务安装证据 |
| 服务部分配置阶段 | Windows 完整服务测试 160 项通过，all-targets/all-features check、严格 clippy 和 crate fmt 通过；部分资源事务 5 项与服务端 PATCH 15 项通过，两轮独立审查完成。Linux/macOS service 最新交叉 check 通过；core 业务接入和真实 Mihomo 验收待完成 |
| 唯一 owner 生命周期与捕获门栓阶段 | Windows core 483 通过、0 失败、2 忽略；owner 18 项、runtime session 20 项、capture 15 项通过。all-targets/all-features check、严格 clippy 和两轮独立审查通过。Linux/macOS core 缺少 C 交叉工具链，未验证；P4 跨后端捕获恢复仍待实现 |
| Windows SCM 上游适配阶段 | 6 项新增行为先失败，修复后模块 13 项通过；Windows service check、严格 clippy、fmt 与首次独立审查通过。适配恢复策略、维护停止和删除完成等待；真实系统服务未安装 |
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

首页统一服务入口的代码阶段已收尾，真实窗口仍待验收。当前优先确认并补齐 Managed Local 最终有效配置的 TUN 准入，再完成 Linux/macOS 有效注册归属。原子维护准入尚未实现，协议 v3 与旧服务迁移提案仍待确认，见 [主计划 §7.3](tun-service-plan.md#73-维护原子准入提案待确认尚未实现)。恢复界面、本地恢复资源物化、provider 缓存持久化目录及 Windows 多账户 GUI 卸载策略也仍待确认；没有把这些提案记为已实施。

1. 先补 Managed Local 配置 TUN 准入的行为决策和回归，再收尾 UI 完整 CI 附加 lint 的五项问题与可见实机验收。首页阶段完整 UI 库 299 项通过；其后的详情导航尾修 Home 5 项、相关 service_tun 7 项通过，binary 11 项为此前启动阶段结果；Manager/UI 收据分类批次与 main/UI 启动批次分别结束两轮审查，底层维护分类和回读也各已结束两轮审查，不重复开启同批审查。
2. 完成无 owner 时的可见恢复界面，行为选择仍待确认；验证三平台真实服务优先启动、拒绝状态和退出。main/UI P1/P2 修复、Windows/Linux 完整 core 回归、Linux check/完整 CI lint 和 Linux 真实普通生命周期复验已通过；macOS core 仍缺 C 工具链。上述结果不替代真实高权限服务或 TUN 验收。
3. 实现修复/卸载的单一捕获事务：冻结最新 accepted 配置与资源，确认服务 Stop/Release，恢复真实 Local，再授权维护；取消、未知结果、保存收据和 shutdown 分别验证。本地资源物化及 GeoData 写入范围仍是待确认方案。
4. 将底层 provider 回读接到 Stop 后、Release 前的高层工作流；以普通权限安全保存，验证完整长度和摘要，覆盖后续启动、失败恢复和调用方取消。
5. 完成跨会话稳定 home、FakeIP/节点身份、相对 TLS/SSH 映射、动态运行目录预算、旧 setuid 迁移及内核保护副本升级；新增持久化结构前说明迁移影响。
6. 收尾 Windows/macOS 维护与包卸载协调、三平台安装文档及 README 双语说明；在真实服务管理器、管理员授权、Mihomo/TUN 环境完成异常退出、升级、卸载和 release 性能验收。

本阶段没有安装系统服务、修改主机 TUN 或部署系统目录文件。macOS bootstrap 的系统工具、FIFO/符号链接拒绝和授权行为仍需 macOS 实机验收。性能和 UI 响应性尚未测量，不作资源占用或交互性能结论。
