# 三平台 TUN 服务实施记录

本记录对应 [开发计划](tun-service-plan.md)。修订日期：2026-10-04；主工作环境：Windows，Rust `x86_64-pc-windows-msvc`。行为测试使用 debug 构建，release helper 与普通 WSL Linux 执行证据分别注明。尚未完成整个计划，P0–P6 整体阶段保持未勾选；已验证子项单独记录。测试计数对应下述阶段终态；后续差异仍需重新验证。

## 开发方向调整与当前差异

### 2026-10-04 Service 内核升级范围与候选准入方案

核对 GUI、CoreSession、Manager、安装 worker 和本地上游：GUI 当前明确拒绝 Service 升级，既有 Repair 只接受 ordinary_launch.binary，已有 journal 的安装提交不能代表后续运行和捕获恢复已完成。新增 [Service 升级接线方案](tun-service-core-upgrade.md)，建议只升级受保护服务副本并复用当前配套 helper 的成对 Repair 事务，保留普通 Local binary；同时升级两个位置需要独立协调事务，不能串联两次成功当作原子升级。

方案区分候选与恢复身份，要求准备时的候选摘要贯穿原生授权与 worker 校验，明确授权等待释放业务门锁、未知结果不重发、安装已提交但运行恢复失败的收据，以及取消和退出的所有权。跨 crate attestation API 与升级范围待确认，没有修改生产代码、协议或持久格式。文档本地引用检查通过；Windows 测试包保持原样，四项目标仍未完成。

### 2026-10-04 Windows release 实机测试包

按用户要求构建 `0.2.0-test.20261004` Windows x64 release 测试包，使用当前未提交工作区，不修改固定版本或安装包流程。`cargo build --release --locked -p zenclash-ui -p zenclash-service --features zenclash-service/server --bins` 通过；只有链接器生成 import library 的提示。产物位于 `dist/ZenClash-0.2.0-test.20261004-windows-x64.zip`（42,380,155 字节），SHA-256 为 `e8955bed60aa7c5dd5c43e2669c0d66418aa9e2aaa9b7684c1b7649633477046`；另有同名 `.zip.sha256` 和展开目录。

包内包含 GUI、v3 服务 helper、Mihomo v1.19.30、官方下载并摘要校验的 geoip.metadb、默认/恢复配置、图标、许可证及测试说明。三份 PE 均验证为 AMD64，helper `--version` 通过，GUI 内嵌测试版本与图标检查通过，两份配置由包内真实 Mihomo `-t` 通过；ZIP 全部 12 个文件逐项解压流摘要与展开产物一致。`BUILD-INFO.json` 记录构建命令、提交、工作区源码摘要与工具链，`SHA256SUMS.txt` 记录包内文件摘要。未启动 GUI、安装服务、启用 TUN 或测量 release 性能，不能把打包检查称作实机验收。

没有找到 Inno Setup 编译器，因此交付解压运行 ZIP，没有生成本次安装 EXE。`start-test.cmd` 使用包内普通用户测试数据，系统服务仍共享；直接运行 GUI 使用日常用户数据。该构建不表示四项目标已完成，已知剩余项及建议测试顺序写在包内 `TESTING.md`。

### 2026-10-04 恢复后的 Local 升级继承双槽许可

新增 [真实升级专项](../../crates/zenclash-core/src/core_update/tests/real_recovery_tests.rs)，先捕获 provider、删除原源，再以原 home 的 TUN-off 双槽配置启动真实 Mihomo。Windows 两项实际红测均在升级候选预检被 SAFE_PATHS 拒绝；不是候选启动或版本拒绝。原因是 [升级 workflow](../../crates/zenclash-core/src/core_update/workflow.rs) 重新构造 validator，丢失恢复 owner 的双槽许可。

现在复用当前 process 的 validator，只用 crate-private 方法替换候选 binary；kind、原 home、恢复目录与写租约保持，普通 Local 没有恢复目录时也不新增许可。既有冻结启动 payload 继续供候选及失败恢复使用。没有新增依赖、目录、用户文案、公共 API 或持久结构。新增专项分别验证成功切换，以及版本回读拒绝后旧内核重新启动并再次读取 held provider；候选是同一正式二进制副本，错误 tag 用于触发版本拒绝，不证明不同真实版本兼容性或下载校验。

最终 Windows 真实专项 **2 通过/0 失败**，既有升级组 **8 通过/4 默认忽略**；Linux GNU core 测试构建通过（715 项），WSL Ubuntu-26.04 普通用户 zen 真实专项 **2 通过/0 失败**、既有升级组 **21 通过/4 默认忽略**。两平台真实专项都使用 Mihomo v1.19.30、空白 SAFE_PATHS 与 SKIP_SAFE_PATH_CHECK=false，关闭 TUN/DNS，临时目录与回环控制器。Windows all-targets/all-features check、严格 Clippy、workspace fmt、最终独立审查通过。未运行本批完整 core 库或管理员维护，未验证 macOS、不同正式内核版本替换和受保护 Service 副本升级。

Windows 旧注册迁移核对确认：现有 SCM 模块能验证固定 LocalSystem own-process 注册，但 `--offline-migration` 仍要求注册不存在；SERVICE_STOPPED 或 PID=0 不能独立证明没有遗留内核，因此未放行已有停止注册。普通 Local 备份恢复仍有独立路径红测，其受控重启方案待确认；四项目标仍在开发中。

### 2026-10-04 真实恢复预检失败后的 GeoData 回滚

扩展 [真实恢复专项](../../crates/zenclash-core/tests/real_local_recovery.rs)：捕获固定 `GeoIP.dat` 与 `geosite.dat` 的 held 字节后，原 home 的 GeoIP 改成不同字节、geosite 删除。进入恢复 completion 时确认两份 held 文件已激活；真实 Mihomo 拒绝无效代理组或双槽/home 之外的 provider 后，确认没有活动子进程、旧 GeoIP 字节恢复、原先不存在的 geosite 被移除。首次恢复成功后确认 held 字节保留。配置不使用 GeoIP/GeoSite 规则，因此这些断言证明文件事务，不证明真实 GeoData 格式解析。

首审发现夹具使用 `GeoSite.dat`，与生产固定小写 `geosite.dat` 不一致；Windows 的大小写行为掩盖了 Linux 捕获缺口，已修正后重新验证。最终 Windows 实际专项 **3 通过/0 失败**；Linux GNU 测试构建通过，WSL Ubuntu-26.04 普通用户 zen 实际专项 **3 通过/0 失败**，固定 Mihomo v1.19.30 摘要核验通过。两平台均显式关闭安全检查绕过，仅使用临时目录、TUN-off/DNS-off 与回环控制器。Windows check、workspace fmt 通过，第二轮独立审查通过。初次 Linux 构建的沙箱 WSL 访问拒绝已通过授权的普通构建/执行复核解决，没有重复执行旧 ELF 冒充本次结果。

没有修改生产代码、持久化格式或依赖。普通 Local 备份恢复红测仍未修复，受控重启选择仍待确认；完整 GUI 管理员维护、macOS 原生验收、跨会话缓存与旧服务迁移仍未完成。

### 2026-10-04 普通 Local 备份恢复路径红测

新增默认忽略的 [真实备份恢复回归](../../crates/zenclash-core/src/core_session/real_backup_recovery.rs)：普通 Mihomo 从托管配置启动，捕获 held provider 后实际应用不同的 candidate-node 配置，再删除原配置/provider 源并恢复。Windows Mihomo v1.19.30 实际执行失败：预检拒绝原 home 外的 `local-runtime/slot0` provider；失败后旧 PID、candidate-node、托管缓存及运行状态保留断言通过，随后预期恢复成功的断言失败。此前双槽修复仅覆盖按恢复路径启动的新 Local，不能报告普通 Local 备份恢复已完成。

本批只新增行为测试，没有实施生产修复。建议未获双槽许可的旧 Local 使用受控停止及新 owner 恢复，保留原 home、关闭 TUN；预检失败保留旧运行，启动失败恢复旧配置。已有许可的 Local 保留热重载。由于旧回归要求保持 PID，此处更换 PID 的行为已提出用户选择，尚未确认。新增测试默认忽略但显式运行仍失败，不能算作通过；core all-targets/all-features check 通过，独立审查两轮已结束。没有执行管理员服务、TUN 或完整 GUI 维护。

### 2026-10-04 真实 Mihomo 的双槽恢复路径许可

新增默认忽略的 [真实恢复专项](../../crates/zenclash-core/tests/real_local_recovery.rs)。首次执行揭示夹具的外部 binary 不在恢复租约内，负例仅检查 is_err 曾误通过；复制正式 binary 到独立 home 后再运行。实际 Windows 红测随后证实生产问题：原 home 保持不变，但双槽 provider 位于其外，Mihomo v1.19.30 在配置预检中以 SAFE_PATHS 拒绝。授权生成恢复资源不等于 Mihomo 允许读取它们，普通 Rust 子进程夹具未覆盖该正式内核规则。

[恢复准入](../../crates/zenclash-core/src/service_runtime/local_geodata.rs) 在租约、普通 binary 与 TUN-off 检查后，只为准确受控双槽 runtime.yaml 设置私有内存许可；[Process](../../crates/zenclash-core/src/process.rs) 和 [Validator](../../crates/zenclash-core/src/core_validation.rs) 给预检与启动传入两个固定槽的 SAFE_PATHS，覆盖继承的宽泛路径并移除继承的 SKIP_SAFE_PATH_CHECK。仍使用原 home，普通非恢复 owner 默认无此许可，meow 行为不变；许可根纳入 Data 协调，没有新增依赖、协议、用户设置或持久结构。

单槽初版通过恢复启动和普通重启，但新增真实轮换热重载回归再次实际失败，slot1 被原 slot0 许可拒绝；改为两个准确固定槽后通过。成功用例删除测试原始源及 provider 后消费 held-node，普通重启 PID 更换，热重载到另一槽读取 held-next 且 PID 不变；无效代理组和双槽/home 之外的 provider 均在实际预检阶段拒绝，没有活动子进程。Mihomo 并不保证损坏的 file-provider 内容使启动失败，因此失败用例改用明确无效的代理组类型，没有把非致命 provider 错误当作生产 bug。夹具同步回收进程后清理临时目录，默认忽略且不自动下载内核。

最终 Windows 正式 Mihomo v1.19.30 **3 项通过**，并在临时设置继承 SAFE_PATHS 为整个临时目录、SKIP_SAFE_PATH_CHECK=true 后复验 **3 项通过**。Windows binary SHA-256 为 `F55B3028D9160BEB9044F21B05DD7405B46524614A19642D6291492F5F985761`。同一最终 Linux GNU 构建（core 712 项、恢复 integration 3 项）在 WSL Ubuntu 26.04 普通用户下，已校验正式 Linux binary SHA-256 `3E92DF24F5E80E86B9CF9183CEB7BB575F0BD132A9DC4081DAE42E80F21076AE`；带同样宽泛继承环境的真实恢复 **3 项通过**，GeoData 恢复 **11 项**、双槽文件 **8 项**、冻结启动 **9 项**均通过，共 **31 项通过/0 失败**，没有执行 Linux 全库。

最终 Windows core 串行全库 **663 通过/0 失败/2 默认忽略**（665 项）；Core/UI all-targets/all-features check、最终 core 严格 CI Clippy、workspace fmt、文档路径和差异空白检查通过。本批两轮独立审查额度已使用；首审指出夹具租约及负例误通过，第二审通过单槽许可实现，随后轮换红测和双槽修正由真实行为测试验证，没有第三审。专项只证明真实 file-provider 消费、重启/轮换及拒绝边界；没有真实 GeoData/TLS 消费、管理员 Service、系统代理/路由改动或 macOS 验收。完整 GUI 修复/卸载、直接重启自动交接、跨会话缓存/身份、旧服务迁移与受保护升级仍未全部完成。运行方式见 [原生验收](tun-service-native-validation.md)。

### 2026-10-04 直接重启的配置语义与当前维护验证

核对 [GUI 直接重启](../../crates/zenclash-ui/src/pages/runtime/mihomo.rs)、[Process 冻结输入](../../crates/zenclash-core/src/process.rs) 与 [Manager 自动配置交接](../../crates/zenclash-core/src/service_manager/tun.rs)，确认直接重启使用当前生成文件，而自动配置交接重建 profile/controlled patch/overrides；Frozen profile 也会再次叠加后两层。不能简单用 ReapplyCurrent 替代原重启或重复合并已经生效的 YAML。已在 [core 接入 §5.6](tun-service-core-integration.md#56-managed-local-配置的-tun-准入与剩余缺口) 写入具体候选、授权、陈旧拒绝与恢复契约，并提出保留生成配置或改为重建的产品选择。该项待回复，尚无直接重启自动交接生产代码；已确认的普通配置自动授权保持有效。

当前 Windows 服务库完整 **319 通过/0 失败/1 默认忽略**（320 项），覆盖已有维护准入、原子锁、旧 v2 拒绝、缓存回读、停止与响应未知等行为。当前 UI 库串行全量 **314 通过/0 失败/1 默认忽略**（315 项），binary **12 项通过**，包括共享维护准备、等待者取消、陈旧收据拒绝与无 owner 退出提示。构建仅有链接器创建 import library 的 linker_messages 提示。没有执行管理员服务、真实 TUN、完整可见 GUI 操作或 macOS/Linux 原生维护；测试通过不能替代这些验收。本轮生产代码未改动，文档路径及差异空白检查通过。跨会话缓存、Linux 有效注册查询依赖及直接重启语义待选择，四项目标仍未完成。

### 2026-10-04 Local 重启拒绝保留生命周期

三项 Windows 普通子进程回归先实际失败：TUN-on 配置在停止前被拒绝，但运行、显式停止与网络暂停阶段均变为 Unknown。[CoreSession 维护](../../crates/zenclash-core/src/core_session.rs) 现在仅在 Local 原生前后观察可信且 PID、运行状态及退出原因未变化时，恢复原阶段、停止意图与网络暂停资格，不推进 generation；网络暂停回归随后实际执行恢复并启动子进程。Service 的未知结果处理未放宽，退出意图仍最终覆盖。直接 GUI 重启的 Manager 自动授权交接仍未完成。

首审发现公开进程快照会把原生查询错误降为无 PID 的停止观察，可能被误判为未变化。新增 Unix 实际子进程回归，终止并回收测试自己的 sleep 后让 Child 查询返回 ECHILD，初版确实错误保留 Stable。[Process](../../crates/zenclash-core/src/process.rs) 新增内部可返回错误的观察接口，公开 snapshot 的接口及失败回退保持不变；无法可信观察的准入后失败标记 Unknown 并推进 generation。前置观察任务异常仍在准入前返回，不把它描述为已执行维护。独立审查两轮额度已使用，最终审查未发现新的功能性 bug 或重大漏洞。

最终 Windows core 全库 **663 通过/0 失败/2 默认忽略**（665 项）；core/UI all-targets/all-features check、core 严格 CI Clippy 与 workspace fmt 通过。最终 Linux GNU ELF 共 712 项，以 WSL 普通用户执行上述生命周期回归 **3 项通过**、ECHILD 回归 **1 项通过**、已退出快照回归 **1 项通过**、冻结配置组 **9 项通过**，共 **14 项通过/0 失败**。使用普通 Rust/sleep 子进程、临时目录及 loopback，没有真实 Mihomo、管理员服务、TUN/路由修改或 macOS 原生验收。没有新增依赖、用户文案或持久化结构；跨会话缓存与 Linux 查询依赖方案仍待选择，整体四项目标保持开发中。

### 2026-10-04 旧服务迁移的有效注册前置条件核对

本轮按当前工作树核对 maintenance worker、三平台原生观察及本地上游，确认现有 offline-migration 只接受注册不存在；没有把“已有注册但停止”接入迁移。已有 v3 安装普通维护仍要求可用的可信 IPC 宿主。该结论是代码路径核对，没有执行真实管理员维护，也没有报告原生停止状态已可放行。Windows 有既存类型化 SCM 归属模块；Linux/macOS 对完整 loaded/inactive 注册及无活动内核的证明仍有缺口。上游 Linux 安装直接调用 systemctl，忽略停止错误后继续部署，不能将该顺序原样搬入已确认的 §7.3。

只读 `cargo tree -p zenclash-service --all-features --locked --target x86_64-unknown-linux-gnu --depth 1` 证明服务 crate 尚无 zbus 直接依赖；锁文件及缓存源码已有 zbus 5.19.0、zvariant 5.15.0，核对了 async-io/Tokio feature、显式地址、方法期限、队列及原始消息体接口。新增 [Linux 有效注册查询方案](tun-service-linux-registration.md)，推荐保留 systemctl 维护、用已锁定 zbus 读类型化属性，避免新增 busctl 工具与手写 libsystemd FFI；依赖、feature 合并影响与尚未测量的独立服务成本明确列出。

方案已提出用户选择，尚未修改 Cargo、生产代码、协议或用户数据。新依赖的批准不能由锁文件已有记录推断；该选择也不批准以 PID=0、无 IPC 或磁盘模板代替归属和内核退出。文档本地路径与差异空白检查通过。跨会话缓存存储方案另有待选项；四项目标保持开发中，不将这一方案记为迁移实现或原生验收。

### 2026-10-04 普通内核升级与回滚共用冻结配置

两项 Linux 行为回归先实际失败：候选配置预检后将托管源改为 `tun.enable=true`，原升级实现停止旧内核后重新打开该源，导致候选启动被 Local 准入拒绝，旧内核回滚也被同一源拒绝，最终没有运行中的内核。本批不是仅按源码推断问题。

[升级事务](../../crates/zenclash-core/src/core_update/workflow.rs) 现在在下载完成后读取并检查一次配置，候选预检、启动、激活失败恢复与旧内核回滚共用同一个 `Arc<str>`。[Process](../../crates/zenclash-core/src/process.rs) 的共享重启路径保留后台阻塞池、数据租约、取消与未就绪清理；传入冻结配置仍执行 Local TUN 检查，非 Mihomo 拒绝使用该路径。测试确认候选及回滚旧内核实际读到原始字节，变化后的源保持原样；新增普通 Rust 子进程回归确认源删除后仍能重启，TUN-on 冻结内容在验证和停止前被拒绝并保留旧 PID。独立普通重启仍读取当时源内容，无新增依赖或持久结构。

Windows 本批初版完整 core 库 **658 通过/0 失败/2 默认忽略**；最后仅补充两项行为测试及复用已有双语内部类型错误文案，最终构建（662 项）的新增测试 **2 通过/660 未选中**，未重复报告为最终全库执行。最终 core/UI all-targets/all-features check、core 严格 CI Clippy、workspace fmt 与差异空白检查通过。独立审查 **2/2** 均未发现功能性 bug 或重大漏洞。

最终 Linux ELF（708 项）以 WSL 普通用户运行完整 `core_update::tests`，**21 通过/0 失败/2 默认忽略**；冻结配置组 **9 通过/0 失败**。测试使用普通 shell/Rust 子进程、临时目录及 loopback，没有管理员服务、真实 Mihomo 升级或 TUN/路由改动，没有 macOS 原生验收。Service 受保护内核升级、旧服务包注册迁移、直接重启自动交接、跨会话缓存及节点身份仍未完成；缓存新结构方案仍待用户确认，整体四项目标保持开发中。

### 2026-10-04 跨会话缓存方案与内核更新路径核对

新增 [跨会话缓存实施方案](tun-service-cache-persistence.md)，具体列明 `provider-cache/{slot0,slot1}`、版本 1 pointer/manifest、provider 来源摘要和批准资源映射、原子发布与预算、取消收尾、失败策略、迁移影响及行为验收。方案建议可丢弃缓存落盘失败保留旧代并警告，继续正常退出；Stop/Release 与恢复必需资源失败保持严格语义。项目规约要求新增持久结构与产品行为先确认，已提出用户确认；尚未新增缓存存储生产代码、协议或用户数据。节点身份不混入缓存 slot，其稳定目录、导入清单和卸载保留仍需独立方案。

重新核对 [CoreSession](../../crates/zenclash-core/src/core_session.rs)：正常 shutdown 当前直接 Release，内存 active/candidate 随之清空，尚无跨会话保存。[内核更新](../../crates/zenclash-core/src/core_update/workflow.rs) 候选预检调用 `validate_file`，之后 Stop/激活，再走普通 restart 的配置读取；预检到重启之间的源变化窗口尚未冻结，当前只按源码识别，没有新增故障红测证据。`install_release` 仍要求实际 Local process，未接受保护 Service 内核升级。[维护 worker](../../crates/zenclash-service/src/installer.rs) 已有显式 offline-migration 分支，但这不能证明旧包拥有注册迁移与 GUI 升级链路已完成。

普通 WSL 用户使用上一轮启动冻结批次构建的 Linux ELF（704 项）补跑 `core_update::tests`，**19 通过/0 失败/2 默认忽略**。core 运行实现自该 ELF 构建后未改动；随后启动提示批次的 i18n 文案不在该 ELF 中，不把此证据写为最新全库构建。测试使用临时目录 shell 子进程和 loopback，覆盖既有普通内核替换、预检拒绝、精确配置回滚、取消及退出竞争；没有管理员服务、真实 Mihomo 或节点身份验证。新增提案的 6 个本地链接及差异空白检查通过。本批是具体方案与调用链/既有行为核对，跨会话缓存与身份、旧服务迁移及受保护升级仍未实现完成，整体目标保持开发中。

### 2026-10-04 无 owner 启动保持退出与错误提示

用户明确选择“维持退出，仅改进错误提示”。本批没有新增恢复窗口或离线主界面，仍沿既有 tracing/stderr 返回启动错误并退出。[启动分类](../../crates/zenclash-ui/src/startup.rs) 使用专用双语文案，区分已保存 TUN 但服务缺失、占用、配置不可确认、服务停止、需修复、维护中、未授权、不兼容、安装归属未知与状态未知，分别说明下一步处理方式。旧“启动已暂停”措辞已移除，未知连接错误不推断为不兼容，也不包含原始 IPC/控制器信息。服务 Missing 且确认 TUN-off 的正常 Local 路由保持不变。计划中的恢复窗口待选项已改为用户确认的退出策略；服务 owner 仍存在时的修复/卸载与恢复继续按原计划开发。

Windows UI binary 全部 **12 项通过**（扩展拒绝分类及新增 unknown 私密错误回归），i18n **4 项通过**；UI all-targets/all-features check、严格 Clippy、workspace fmt 通过。独立只读首审通过，本批 **1/2**；没有必要重复审查。测试验证启动路由及输出内容，未启动真实 GUI 或原生服务，没有 macOS/Linux 原生启动提示验收。本批没有修改退出返回码、原生消息框或日志输出渠道；Windows `windows_subsystem` 下 stderr 不保证在桌面上可见，不能把这些测试报告为可见错误弹窗验收。没有新增依赖、配置或持久结构。

同步 [开发计划](tun-service-plan.md)、[core 接入](tun-service-core-integration.md)、[资源布局](tun-service-resource-layout.md) 与双语 README，修正仍称双槽恢复“待确认/未物化”的过时当前状态。四份专项文档的 **213 个本地链接有效**，差异空白检查通过。跨会话 provider 映射与持久节点身份格式仍需明确；已批准双槽目录不能作为这些新结构的批准。旧服务迁移、受保护内核升级、Windows 多账户卸载策略及完整管理员维护链路仍未完成，整体四项目标保持开发中。

### 2026-10-04 Local/Mihomo 启动配置冻结

[Process](../../crates/zenclash-core/src/process.rs) 现在把一次有界读取后的配置同时用于最终 TUN 判定、重启预检和内核启动。通过 [文件输入](../../crates/zenclash-core/src/process/input.rs) 将冻结内容作为 stdin 交给 Mihomo `-f -`，保留原 home；Unix 立即 unlink，Windows 使用 delete-on-close，避免管道写入阻塞和无人管理的后台写任务。启动及验证移除 `CLASH_CONFIG_STRING`，防止该环境变量覆盖已检查内容；meow 继续使用原文件参数。未新增依赖或用户持久化格式。依据固定 [Mihomo v1.19.30 main.go](https://raw.githubusercontent.com/MetaCubeX/mihomo/v1.19.30/main.go)，没有改动固定内核版本与发布流程。

两个普通真实子进程回归先失败：启动读取到检查后的 TUN-on 源；重启丢失已预检候选。初版清除启动环境但遗漏验证环境，环境回归再次失败；修正后六项 Windows 回归通过，覆盖源替换、环境覆盖、取消保留原 PID、启动失败清理及大输入无管道阻塞。首审发现 Unix FIFO 打开可能永久阻塞；改为 `O_NONBLOCK` 后核对同一 handle 的普通文件类型，Windows 另检查磁盘类型。新增 Unix FIFO 拒绝、旧 PID 保留及清理回归；该新增回归没有修改前红测记录。第二轮审查通过，本批达到 **2/2**。

最终 Windows Core 串行全库 **658 通过/0 失败/2 默认忽略**（660 项），Core/UI all-targets/all-features check、Core 完整 CI 严格 Clippy、UI 标准严格 Clippy、workspace fmt 及差异空白检查通过。实际 Mihomo v1.19.30 普通生命周期测试 `real_automatic` **2 项通过**，覆盖修改源后重启、无效源保留原进程、网络暂停与崩溃恢复区分、手动停止及退出；配置 TUN-off、controller 为 loopback，不是 mock，也不代表后台服务、系统授权或真实 TUN 验收。最终 Linux GNU ELF 共 704 项；普通 WSL 用户执行启动冻结 7 项、双槽恢复 8 项、GeoData 恢复 11 项及 Manager 备份 4 项，共 **30 项通过**。未执行 Linux 全库或本批 Linux 真实 Mihomo。初次双槽及备份筛选分别为零项，定位真实模块后已补跑；不把零项记作通过。

双语 README 同步补充启动快照行为；两份专项文档的 **153 个本地链接有效**。README 全量链接检查发现已有 `docs/development/gpui-kit-migration.md` 缺失，两种语言均指向同一缺失目标，本批未修改该无关链接。最终 Windows UI 串行全库 **314 通过/0 失败/1 默认忽略**（315 项）；测试链接器输出创建 import library 的 `linker_messages` 提示，构建与测试成功。未改变 UI 布局，没有新的实机 UI 验收。

本批关闭 Local/Mihomo 检查到启动读取之间的源替换窗口。GUI 丢失 Service owner 的恢复、跨会话缓存/身份、旧服务迁移和受保护内核升级仍未完成；三平台原生服务与 macOS 验收仍待执行，整体四项目标保持开发中。

### 2026-10-04 备份激活前授权与 held 资源回滚

[备份工作流](../../crates/zenclash-ui/src/pages/runtime/settings/backup/workflow.rs) 现在在备份准入及数据激活之前，经 [ProfileService](../../crates/zenclash-ui/src/profile_service.rs) 调用 [Manager 准备](../../crates/zenclash-core/src/service_manager/backup.rs)。仅实际 Local/Mihomo 冻结归档最终配置、旧运行快照及双方资源；需要 TUN 的候选先完成既有系统授权，等待期间不保留 Data lease、capture gate 或 transition。激活前核对绑定、版本、旧缓存及源身份，激活后重新比较归档候选。消费禁用再次授权，并把备份已有写权限传给 CaptureBackend，避免持有 exclusive lease 后重新排队。

首审发现旧资源只在内部 Service 准备中持有，外层事务缺少 bundle；Local→Service 成功后若设置读取失败，旧快照无法在 Service 中恢复。已把 held snapshot 经仅内存的 PreparedBackupRestore 带入备份事务。实际 Local 回滚复用 §5.5 双槽 TUN-off 文件及资源；Service 沿既有 held bundle 恢复。旧 provider/TLS 删除回归先失败后通过。第二审发现热重载不更新启动路径，连续恢复会误判活动槽并覆盖旧资源；HTTP400 拒绝回归确实先失败。改由会话内存保存已接受槽，成功后更新；未知 PUT 保留待确认文件，只允许同一 held 候选复用重试，避免覆盖任一可能活动槽。没有新增依赖、归档版本或用户持久化结构；已有订阅及资源源文件不改写。

最终 Windows Core 串行全库 **652 通过/0 失败/2 默认忽略**（654 项），UI 串行全库 **314 通过/0 失败/1 默认忽略**（315 项）。五项新增回归全部通过：授权等待时正常设置 writer 可完成且原 PID/缓存不变；旧缓存变化在数据替换前拒绝；激活候选变化在 reload 前拒绝；旧 provider/TLS 删除后的直接恢复；Manager 准备、激活、Local 应用、外层回滚、连续拒绝及未知结果重试。最后一项复验 HTTP400 不改变原槽，未知响应期间其他 bundle 在派发前拒绝，删除原始资源后同一候选仍可通过 `retry_backup_restore` 恢复，PID 不变。授权等待使用替代授权回调；进程是普通测试子进程，controller 是 loopback responder，不能视为真实 Mihomo 或原生授权验收。

Core 完整 CI 严格 Clippy、UI `-D warnings` Clippy、workspace all-targets/all-features check、workspace fmt、差异空白检查及两份文档的 148 个本地链接检查通过。最终 Linux GNU ELF 共 697 项，普通 WSL 用户执行新备份/资源恢复 5 项、双槽文件 8 项和 GeoData 11 项，共 **24 项通过**；仅隔离测试进程的代理环境，复用现有编译桥，没有安装工具链。两次独立审查额度已使用（2/2），最终审查发现的槽轮换问题修复后做行为验证，没有第三审。原生授权、管理员服务、真实 Mihomo/TUN 和 macOS 未在本批验收。启动文件冻结、GUI 丢失服务 owner 的恢复、跨会话缓存/身份、旧服务迁移和受保护内核升级仍未完成，四项目标保持开发中。

### 2026-10-04 完整 YAML 合并语义与自动 Service 准备

核对备份激活前授权所需的候选准备时，发现自动交接仍用未展开 YAML 的 TUN 字段，而上一批底层已有检查：merge 候选会被错误送入普通路径并拒绝。现在 [service_tun](../../crates/zenclash-core/src/core_session/service_tun.rs) 与 [tun_admission](../../crates/zenclash-core/src/tun_admission.rs) 共用最终 YAML 判定；[profile](../../crates/zenclash-core/src/profile.rs) 的基础源及每个 ordered override 在合并层之前展开默认映射，继承值按已有层顺序覆盖；[Service bundle](../../crates/zenclash-core/src/service_runtime.rs) 和 provider YAML 在资源重写前展开，持有真正生效的 provider/TLS。源文件保持不变，没有新增依赖或持久化格式。备份事务顺序没有在本批修改，授权持锁仍待解决。

最初新增三项行为回归实际 **0 通过/3 失败**（`merged_` 筛选另含两项既有通过），分别为 Service 候选判定、覆写继承值优先级和资源持有。初版展开后的 Windows Core **645 通过/0 失败/2 默认忽略**。首审发现库单次 `apply_merge()` 会把下一层 `<<` 复制到已处理的映射，链式默认值仍漏展开。链式扩展实际 **3 通过/2 失败**：候选准备因中间多次展开而通过，不代表共用判定无漏判；ordered override 和 bundle 失败。修正为统一后序展开，先处理子映射/序列，再消费当前 merge，保留显式键与 merge 序列先者优先，设置 128 深度上限。补充显式关闭 TUN、合并序列、无效 merge、深度和底层 loopback 链式回归。第二轮审查通过，已达 **2/2**。

最终 Windows Core 串行全库 **647 通过/0 失败/2 默认忽略**（649 项），随后严格 lint 发现新增测试不必要的路径复制，改为借用并通过 Core 完整 CI 严格 Clippy，最终 Windows 合并专项 **7 项通过**；本批没有第三次审查。最终 workspace all-targets/all-features check、workspace fmt、差异空白检查以及两份专项文档的 143 个本地链接检查通过。最终 Linux GNU ELF 共 692 项，在普通 WSL 用户下执行合并专项 8 项、Local TUN 4 项、External/meow 各 1 项、双槽恢复 8 项及 GeoData 恢复 11 项，共 **33 项通过**；包含最后测试借用修正，仅隔离测试进程代理环境，没有安装工具链。本批不修改 UI 布局，未重跑 UI 全库，也没有管理员服务、系统授权、真实 Mihomo/TUN 或 macOS 验收。备份激活前授权、启动文件冻结、GUI 丢失服务 owner 的恢复、跨会话缓存/身份、旧服务迁移及受保护内核升级仍未完成，整体四项目标保持开发中。

### 2026-10-04 普通 Local 执行边界的 TUN 检查

新增 [tun_admission](../../crates/zenclash-core/src/tun_admission.rs)，仅约束实际 Local/Mihomo。客户端完整 payload 在普通验证和派发前检查，PATCH 在派发前检查；普通 [Process](../../crates/zenclash-core/src/process.rs) 首次启动及重启读取有界配置，重启在预检前后检查，未经交接的 TUN 开启返回 `ServiceRequired`，不归为发送结果未知，拒绝重启保持原 PID。首次拒绝不创建 home。GUI 已接的 Manager 自动授权流程继续负责交接，底层检查不执行系统授权，也不替代它。External、Service、meow 排除；验证未激活配置不受此执行政策限制。新增错误文案已同步中英文，未新增依赖或持久化格式。

六项新增行为测试中，最初 Local PATCH、完整重载和重启三项实际红测 **0 通过/3 失败**。首审发现 JSON 大小写字段与 YAML merge 两项 P1；扩展 loopback 回归后实际 **2 通过/2 失败**。修正为展开 YAML merge 后检查最终 TUN，并逐一检查 JSON 所有大小写匹配成员，任何启用请求都拒绝。回归覆盖根级/嵌套 merge、大小写及同名变体并存。第二轮只读审查通过，达到 **2/2**。随后严格 lint 发现新增测试未处理读取字节数，补上非零断言并复验，没有第三次审查。

Windows Core 串行全库 **642 通过/0 失败/2 默认忽略**（644 项），在最后读取字节数断言修正之前执行。修正后的 Windows TUN 专项 **27 项通过**，Core 完整 CI 严格 Clippy 通过。Core/UI all-targets/all-features check、workspace fmt 和差异空白检查通过。最终 Linux GNU ELF 共 687 项；普通 WSL 用户执行 Local 准入 4 项、External/meow 各 1 项、双槽恢复 8 项和 GeoData 恢复 11 项，共 **25 项通过**，包含最后断言修正。测试仅隔离进程代理环境，没有安装工具链、服务或开启真实 TUN。本轮不修改 UI 布局，也未重跑 UI 全库。

本批只是执行边界兜底。普通启动仍读取可变路径，最终检查到内核读取之间的窗口没有冻结；备份先激活恢复数据、再进入可能授权的配置流程，exclusive lease 内等待授权仍待调整。GUI 丢失服务 owner 的恢复、跨会话缓存/身份、旧服务迁移及受保护内核升级也未完成。三平台原生服务、系统授权、真实 Mihomo/TUN 与 macOS 均未在本批验收，四项目标保持开发中。

### 2026-10-04 目录自动授权与服务试运行后的比较提交

[ProfileService](../../crates/zenclash-ui/src/profile_service.rs) 的导入、订阅下载/更新、激活和编辑复用 [ProfileApplication::apply_with_service](../../crates/zenclash-core/src/profiles/application.rs)。实际 Local/Mihomo 的最终有效候选开启 TUN 时，授权前仅持有候选与资源；[Manager](../../crates/zenclash-core/src/service_manager/tun.rs) 在健康查询前核对原候选 binding/generation，再使用既有授权和捕获交接。非 TUN、inactive、External/Service/meow 沿原目录事务。普通 `apply_prepared` 拒绝需要 Service 的候选，GUI 则进入自动授权流程，不能从普通应用绕过。

目录是同一个 Core 交接事务中的 one-shot participant：profiles root 纳入共同 Data lease，停止旧 Local 前比较目录与受管源；Service trial 和 TUN 回读成功后再次比较提交，使用 held 基础源生成 staging 和托管源。完整目录应用只核对 controlled 层、不改写它。目录失败经原缓存回滚及 held Local 恢复；目录成功之后才标记缓存 durable、发布 committed profile/override 身份及确认 Service revision。Commit 未确认保留目录收据。UI owned 完成任务分别发布目录结果与同版本捕获警告，页面不再等待也不丢失已准入业务完成。

首审发现候选版本可能晚于授权检查以及同版本警告被拒绝两项，已修复。二审发现把其他事务推进的全局版本误当本次失败证据，已改为 Core 在实际停止尝试前记录 runtime attempted，并在捕获门仍持有时记录本次结束版本；无运行尝试的失败为 Rejected，旧失败使用自己的版本，不覆盖新运行结果。本批审查达到 **2/2**，最后分类修正没有第三次审查。新增 Core 六项与 UI 两项回归，新增测试没有实际修改前红测记录。修正前目录专项 25 项、Core 全库 **634 通过/0 失败/2 默认忽略**（636 项）；最后分类修正后 Windows 并行 Core 全库 **635 通过/1 失败/2 默认忽略**（638 项），失败为既有 `geodata_recovery_restart_does_not_deadlock_behind_waiting_restore` 的 200ms 就绪窗口；单独复跑 1 项通过。最终 Core 串行全库 **636 通过/0 失败/2 默认忽略**（638 项）；最终 Windows UI 串行全库 **314 通过/0 失败/1 默认忽略**（315 项）；最新 Linux GNU ELF 在普通 WSL 用户下执行目录事务 28 项、Manager 陈旧候选拒绝 1 项、配置准备 6 项、既有 Service→Local 恢复 4 项及初始失败恢复 2 项，共 **41 项通过**；仅隔离测试进程代理环境，不安装工具链。不据串行或单独通过宣称并行稳定性已解决。Core/UI all-targets/all-features check、Core 完整 CI 严格 Clippy 及 UI 标准严格 Clippy 已通过；UI Clippy 在最终 Core 分类修正前运行，最终 UI 全库构建/测试已复验。最终 workspace all-targets/all-features check、workspace fmt、差异空白检查及两份专项文档的 135 个本地链接有效。

这是生产接线及分层行为证据，尚无真实 ServiceClient 的目录 Start→保存失败→Local 恢复与 GUI 系统授权全链路验收，不宣称目录 TUN 功能完整交付。未安装服务或开启 TUN，macOS 原生未执行；其他直接配置/备份/重启入口、跨会话缓存身份、旧服务迁移与内核升级仍待完成。整体四项目标保持开发中。

### 2026-10-04 普通恢复的写租约范围校验

继续落实已确认的 §5.5 双槽方案。[双槽生成](../../crates/zenclash-core/src/service_runtime/local_runtime.rs) 与 [GeoData 激活](../../crates/zenclash-core/src/service_runtime/local_geodata.rs) 的内部 admitted 入口现在除核对同一 store mutation gate 外，还必须确认传入写租约覆盖 store root；GeoData 还要求覆盖原 home。拒绝发生在目录生成、文件替换及 completion 调用之前，返回错误而非触发授权构造的 panic。合法调用继续传递原租约与锁，不重新排队；未新增依赖、持久化格式或原生维护行为。

两项新增行为测试在修复前实际 **0 通过/2 失败**：不覆盖 store 的租约仍生成双槽文件，只覆盖 store 的租约在普通 home 授权处 panic。修复后验证错误返回、不生成恢复目录、原 GeoData 字节保持及 completion 不调用。Windows Core 全库 **630 通过/0 失败/2 默认忽略**（632 项）；Core/UI all-targets/all-features check、Core 完整 CI 严格 Clippy 通过。一次独立审查 PASS（1/2）。最新 Linux GNU ELF 在普通 WSL 用户下执行双槽 8 项、GeoData 11 项、既有 Service→Local 恢复 4 项及初始交接失败恢复 2 项，共 **25 项通过**；仅隔离测试进程代理环境，未安装工具链。workspace fmt、差异空白检查及两份专项文档的 133 个本地链接有效。本轮未改 UI 业务调用，不重复 UI 全库。

该修复只补齐普通恢复写权限边界，不能作为真实管理员服务、Mihomo/TUN 或 macOS 原生验收。目录自动授权与 Service trial 后的目录提交、其他配置入口 TUN 准入、跨会话缓存身份、旧服务迁移与内核升级仍未完成，四项目标保持开发中。

### 2026-10-04 目录 Local/Mihomo 候选的最终层与资源冻结

[目录准备](../../crates/zenclash-core/src/profiles/application.rs) 对实际 Local/Mihomo 的运行变更调用 [Core 共同配置准备](../../crates/zenclash-core/src/core_session/service_tun.rs)，以已持有的基础 YAML 与最终目录目标身份准备，不读取尚未提交的目标文件。[受控配置准备](../../crates/zenclash-core/src/controlled_config.rs) 支持 File/Frozen 基础源；Frozen 仅用于完整应用，不接受 PATCH。候选合并 controlled 与 ordered overrides、规范化及监听约束后持有最终 payload、受控层字节和确切旧运行缓存。最终开启 TUN 时同时持有 previous/next bundle，保留实际 DNS 等配置及最终 profile/override 身份；缺少旧快照仍按共同 TUN 准备规则拒绝，不能虚构可恢复资源。非 Local/Mihomo 或 inactive 操作保持原路径。

`PreparedProfileChange::requires_service` 仅报告准备事实，不授予权限。普通目录消费通过内部 Prepared source，在 store mutation gate 下核对 controlled bytes 与确切缓存再使用最终 payload，不能重新读 deleted/changed override；原源比较提交、generation、失败回滚和退出所有权仍沿用同一事务。**目录 Manager 授权与 Service trial 尚未接入，目前该标志不会让目录操作自动进入服务**；进入普通目录消费时转换为最终 source，不能据此宣称 TUN 准入完成。下一阶段必须在此转换之前把 Service 候选交给同一个目录提交所有者，确认授权及服务试运行后再保存目录，不允许先保存运行配置再调用普通目录 apply。

红测将未提交目录目标传给原 File 接口，确实因路径不存在失败；改为 held YAML 后完成最终 TUN/DNS 判断，删除 ordered override/provider/TLS 源仍能从已持有 next bundle 生成恢复资源，且不创建原目录目标、不修改旧缓存或 generation。另两项真实普通 Local 子进程测试验证 source TUN-on 被 override 关闭后，删除源及 override 仍接受同一最终 off/direct payload（目录只保存原基础源）；controlled 层 off→on 后拒绝、保留新 controlled 内容，不提交目录、不创建运行缓存且 generation/PID 不变。核对还发现共同准备在 Core 持有 expected_cache 后再次读取缓存来生成 previous_payload；已改为直接使用同一份持有快照。第四项测试让磁盘缓存改变后仍保留提供的 previous snapshot，不写磁盘缓存，消费比较拒绝变化；无这项修改的红测记录。首审及最终二审 PASS（2/2）。首轮 Windows Core 全库 **627 通过/0 失败/2 默认忽略**（629 项），Windows UI 串行全库 **312 通过/0 失败/1 默认忽略**（313 项）通过。UI 全库在最后的缓存读取收敛之前运行，最终 Core 回归与 Core/UI check 另行复验；未以串行通过宣称固定端口并行竞争已解决。最终 Windows Core 全库 **628 通过/0 失败/2 默认忽略**（630 项），Core/UI all-targets/all-features check 与 Core 完整 CI 严格 Clippy 通过。最新 Linux GNU ELF 的目录事务 23 项、候选准备 6 项、旧缓存一致性 1 项、不可读缓存拒绝 1 项及精确回滚 1 项，共 **32 项通过**；普通 WSL 用户仅在测试进程内隔离代理环境。workspace fmt、差异空白检查及两份专项文档的 131 个本地链接有效。最后的缓存快照修正由上述 Core 全库与 Linux 相关测试验证，未重复 UI 全库。

本批不新增依赖或持久化格式，不启动管理员服务或 TUN。冻结候选的测试与普通进程证据不能代替 GUI 授权/Service 全链路，macOS 原生未执行。四项目标继续进行中。

### 2026-10-04 目录候选的准备与消费准入

[ProfileApplication](../../crates/zenclash-core/src/profiles/application.rs) 新增 `prepare_change` → `PreparedProfileChange` → `apply_prepared`，现有 `apply` 也复用同一流程。准备阶段处理导入、下载、激活和编辑，持有有界源字节与完整目录/源比较信息；返回前在后台移除 generated staging 文件、释放子写租约，外层 Data lease/transition/mutation 不跨等待保留。未应用运行配置，也不保存目录。原导入源删除后仍能消费已持有内容，不重建或覆盖原源；受管激活/编辑源变化则必须拒绝，不能覆盖新版本。

消费核对 profiles/controlled root、同一 CoreSession 身份以及实际运行变更的 binding/generation，等待新全范围写租约后再次复核；后台核对原目录与受管源，再重新生成 staging。进入 [Core 阶段事务](../../crates/zenclash-core/src/core_session.rs) 的 transition 后再次复核预期运行版本，防止排队期间的新操作被旧候选覆盖。运行接受后的目录比较提交、精确缓存回滚、owned persistence completion 与 unknown finalization 保持既有规则。准备错误使用 `ProfilePreparationError`，转换后保留原 Rejected 与 last-known-good；不把包含成功状态的大结果放入 Err。两处源校验复用同一比较函数，commit 保持 staging → catalog/source → 写入顺序。没有新增依赖或持久化格式。

新增四项行为覆盖：准备候选仍存活时可取得真实 Data restore 排他租约且 staging 为空；导入源删除后应用 held 字节；受管源变化在 HTTP 请求前拒绝；同版本不同会话拒绝；新运行事务接受后旧候选拒绝。等待准入测试使用同一测试的复合可观察行为，四项测试没有修改前红测记录。首审及最终二审均 PASS（2/2）。最终修正后 Windows Core 全库 **624 通过/0 失败/2 默认忽略**（626 项）；Windows UI 串行全库 **312 通过/0 失败/1 默认忽略**（313 项）。Core/UI all-targets/all-features check 和 Core 完整 CI 严格 Clippy 通过；UI 串行通过不证明先前固定端口的并行竞争已经解决。最新 Linux GNU ELF 的目录候选/目录事务 21 项、不可读缓存拒绝 1 项和精确 payload 回滚 1 项，共 **23 项通过**；普通 WSL 用户仅在测试进程内隔离代理环境。workspace fmt、差异空白检查与两份专项文档的 127 个本地链接检查通过。

此为目录授权等待前的候选所有权前置，**尚未接入目录自动授权或 Local→Service 交接**。候选仅冻结基础源；最终 controlled/ordered overrides、资源 bundle 和运行缓存须在授权前通过共同 Core 准备接口冻结。下一阶段必须将已准备目录候选带入 Service trial，同一业务完成任务在运行接受后比较提交目录；提交失败确认 Stop/Release 后恢复 held Local；目录已保存但 Commit 未确认时保留 durable source/runtime receipt，不能回滚已保存数据。当前一次性 Manager 配置应用不能直接替代该目录提交边界。真实管理员服务、Mihomo/TUN 与 macOS 原生未执行。

### 2026-10-04 初始服务交接失败的持有资源恢复

[Core 初始交接回滚](../../crates/zenclash-core/src/core_session/service_tun.rs) 不再重启原源文件路径，而是向 [普通恢复组合](../../crates/zenclash-core/src/service_runtime_session/local_recovery.rs) 传入授权前持有的 previous bundle。即使原配置、file provider 或 TLS 源已经删除，仍使用已确认的 §5.5 双槽生成配置与资源；恢复配置关闭 TUN，保留原 Mihomo home，启动后保存确切恢复缓存。新 Local owner 在启动前发布到既有 binding，正常退出可追踪并停止。服务 Release 未确认、旧缓存回滚失败或应用退出时，不启动另一个 Local 内核；GeoData 激活前必须确认服务已经停止并释放。已有 Service→Local 维护恢复复用同一组合，保存失败或子进程停止未知仍保留活跃 owner 与资源并报告未恢复，禁止继续维护。

红测删除原配置后执行旧的 restart，确实因配置文件不存在而失败。新增行为测试使用真实普通测试子进程和 mock Service 传输：失败试运行没有 accepted snapshot，持有 provider/TLS/GeoData 仍可恢复；核对 Release 先于发布、TUN 关闭、原 home、确切缓存、原源不被重建以及 shutdown 停止新进程。另一项核对未知 Release 不发布、不启动、不替换原 GeoData。首审发现生产外层 store gate 与双槽/GeoData 再获取同一锁导致自锁，已改为连续传递 owned guard 与 lease，并校验同一 store；成功回归明确持有外层 gate 且限制完成时间。最终第二次独立审查 PASS（2/2）。Windows Core 全库 **620 通过/0 失败/2 默认忽略**（622 项）；Core/UI all-targets/all-features check、Core 完整 CI 严格 Clippy、workspace fmt 和差异空白检查通过。最新 Linux GNU ELF 的初始交接恢复 2 项、既有普通恢复 4 项、双槽文件 7 项和 GeoData 10 项，共 **23 项通过**；使用普通 WSL 用户，仅在测试进程内隔离代理环境，未安装工具链。两份专项文档的 124 个本地链接有效。本轮未修改 UI 业务调用，未重复 UI 全库。

本批没有新增依赖、持久化格式或原生安装行为。mock Service 及普通进程测试不能证明管理员服务/真实 Mihomo/TUN 的全链路恢复；macOS 原生未验证。目录自动授权交接、其他配置/启动入口的 TUN 准入、跨会话缓存身份、旧服务迁移和内核升级仍属于未完成工作。

### 2026-10-04 目录事务热重载与重启的最终 payload 接续

[目录运行阶段](../../crates/zenclash-core/src/core_session.rs) 的完整重载现复用 [配置事务](../../crates/zenclash-core/src/controlled_config.rs) 的单一候选管线：先核对确切旧运行缓存，再合并基础源、controlled 与 ordered overrides，完成规范化及监听约束；整个热重载与 HTTP 未知失败后的重启保留同一 payload、mutation gate 和写权限。重启不再重新读取这三层文件，仅实际 Local owner 且启动路径等于托管缓存路径时允许回退；Service/External 不能通过这个分支重启。旧的无重启阶段接口仍保留语义，验证及不支持重载的阶段保持原流程。

红测使用普通测试子进程与回环 HTTP，在原 TUN-off/direct 请求丢失响应前把三层源改为 TUN-on/global，旧实现确实重启并保存了变化后的配置；修复后检查启动缓存等于初次 PUT 的最终 YAML，PID 变化且结果为 Restarted。首轮完整回归还发现不可读缓存的错误优先级变化，已恢复“先检查旧缓存，再合并候选”，以既有拒绝测试复验。独立首审未发现新的功能性 bug 或重大漏洞（1/2）。最终 Windows Core 全库 **618 通过/0 失败/2 默认忽略**（620 项）；最新 Linux GNU ELF 的候选接续 5 项、目录事务 17 项、不可读缓存拒绝 1 项、确切缓存回滚 1 项共 **24 项通过**，仅在测试进程内隔离代理环境。Core 完整 CI 严格 Clippy、最终 Core/UI all-targets/all-features check、workspace fmt、差异空白检查通过；两份专项文档的 121 个本地链接有效。未修改 UI 调用，本轮未重复 UI 全库；尚无真实 Mihomo 或管理员 TUN 验收。

这是目录最终候选一致性的阶段修复；**目录自动授权及 Local→Service 交接仍未接入**。直接 Core/client 配置、备份、启动/更新、源码自动变更等其他入口尚未统一 TUN 准入；真实管理员服务、Mihomo/TUN 与 macOS 原生验收仍待完成。本批不新增依赖、持久化格式或原生维护行为。

### 2026-10-04 托管目录事务的基础源快照

[ProfileApplication](../../crates/zenclash-core/src/profiles/application.rs) 的导入、激活、编辑和远程更新候选现在持有 UTF-8 基础源内容（单份不超过既有 16 MiB 限制）。[Core 阶段事务](../../crates/zenclash-core/src/core_session.rs) 与 [配置合并](../../crates/zenclash-core/src/controlled_config.rs) 消费同一份持有内容，不能改用被外部写入的临时候选。目录提交核对 staging 内容是否仍与持有内容相同，使用持有字节写入托管源；原目录和源版本比较、失败回滚和完成任务所有权保持既有流程。快照随事务结束释放，不新增持久化格式或依赖。新增错误文案已同步中文和英文。

红测复现旧实现把改写后的 `tun.enable=true`/CHANGED 候选持久化。修复后的行为测试检查真实回环 HTTP：应用原 HELD/TUN-off payload、拒绝已被改写的 staging、恢复确切旧运行 payload 与缓存、不新增目录记录，并清理临时文件。独立首审未发现新的功能 bug 或重大漏洞（1/2）；Core/UI all-targets/all-features check 与 Core 完整 CI 严格 Clippy 通过。最终 Windows Core 全库 **617 通过/0 失败/2 默认忽略**（619 项）；最新 Linux GNU ELF 的目录事务 17 项、配置合并 3 项、确切缓存回滚 1 项共 **21 项通过**，测试进程显式隔离代理环境。最终 Core/UI all-targets/all-features check、workspace fmt 和差异空白检查通过；两份专项文档的 119 个本地链接有效。本轮未修改 UI 业务调用，因此未重复运行 UI 全库；当前编译检查不能替代实机交互验收。

这一步为目录事务的授权前候选准备提供基础源快照，**尚未完成目录事务自动授权及 Local→Service 交接**。controlled 与 ordered overrides 仍在各阶段合并；热重载失败后的重启可能重新读取它们，后续必须冻结最终有效 payload 并协调目录提交与交接失败恢复。三平台管理员服务、真实 Mihomo/TUN 和 macOS 实机未执行，普通回环测试不能代替这些验收。

### 2026-10-04 普通配置候选冻结与重启边界

共享 GUI 配置入口的 `try_apply_service_config` 现在区分 Local 与 Service 接受结果。最终 TUN 关闭的 Local/Mihomo 配置也保留检查时的完整候选，不再返回后由普通路径重新读取源文件。实际应用在同一会话身份、binding/generation、controlled patch 和运行缓存复核后，使用候选字节；源文件变为 TUN 开启或 override 被删除不会改变本次应用内容。无运行缓存的首次普通应用仍可成功；完整应用不新增 controlled patch 文件。External/Service/meow 仍使用原路径。

热重载遇到传输失败时，只有实际 owner 的启动配置路径等于托管运行缓存路径才允许回退重启；其余启动路径保留当前子进程并报告失败，避免重启从变化的订阅源加载未检查配置。PATCH 的持久化失败恢复确切旧缓存及旧运行 payload，不重新合并源文件。owned 完成任务持有事务与写权限，成功后发布完整 committed 配置身份及 generation。

回归先复现非 TUN 候选被丢弃（1 项失败），修复后新增行为覆盖源配置 off→on、override 删除、无缓存首次应用、运行缓存变更拒绝、托管缓存重启与可变源路径重启拒绝、PATCH 保存失败后的 HTTP payload/缓存恢复，以及 Manager 普通应用不查询健康或触发授权。独立首审未发现新的功能性 bug 或重大漏洞（1/2）；Core/UI all-targets/all-features check、Core 完整 CI 严格 Clippy 和 UI 标准严格 Clippy 通过。Windows Core 全库 616 通过/0 失败/2 默认忽略（618 项，最终代码已复验）；UI 首次并行全库 311 通过/1 失败/1 忽略，失败为既有固定 7891 端口候选探测；串行全库复核 312 通过/0 失败/1 默认忽略（313 项），不能由此宣称并行稳定性已解决。Linux 最新 GNU ELF 的候选准备 4 项、Manager 分流 3 项、保存失败恢复 1 项共 8 项通过；首次重启测试受环境代理影响返回 HTTP 502，在仅移除测试子进程代理变量并明确回环 NO_PROXY 后通过，未修改用户代理设置。普通子进程复用既有受限编译桥，未安装 Linux 工具链。两份专项文档的 116 个本地链接有效。

此修改仅补齐上述共享入口的源文件竞争；ProfileApplication 目录事务、直接 Core/client、备份/重启/更新的统一 TUN 准入、跨会话持久化和旧服务迁移仍未完成。行为测试使用普通测试子进程和回环 HTTP，不代表真实 Mihomo 或管理员服务/TUN 验收。

### 2026-10-04 共享配置操作的最终 TUN 候选交接

[ProfileService](../../crates/zenclash-ui/src/profile_service.rs) 的完整 override 重载、重应用及 [运行页手动 PATCH](../../crates/zenclash-ui/src/pages/runtime/lifecycle.rs) 接入 `try_apply_service_config`。实际 Local/Mihomo 的最终有效配置开启 TUN 时，冻结源配置、controlled patch、有序 overrides 及确切旧运行缓存对应资源，再使用既有授权/捕获/交接事务应用完整候选。自动候选保持实际 DNS 与其他配置，不能用固定 TUN+DNS delta 代替；成功后保存候选 controlled patch、精确启动缓存和新 committed profile/override 身份，完整接受按既有语义取代旧 pending backup。保存收据、待确认状态及捕获警告在 owned 业务完成任务内发布，页面取消等待不丢失。

非 TUN 候选返回普通路径，不要求旧缓存；External/Service/meow 不被 Local 自动分支替换。候选在授权前核对与 capture 使用同一 store root，消费再次核对；核心错误保持类型，无 committed profile 的重应用仍为空操作。首审发现存储错配、普通路径缓存要求和错误类型丢失三项，已修复；二审发现首次完整应用仍依赖旧 profile，已改为候选身份。本批审查达到 2/2，最后修正不进行第三次审查。新增两项 core 子进程/候选测试和一项 UI 业务测试通过，没有实际红测记录；最终 Windows core 全库 **612 通过/0 失败/2 默认忽略**（614 项）、UI 全库 **312 通过/0 失败/1 默认忽略**（313 项）通过；core/UI all-targets/all-features check、core 完整 CI 严格 Clippy、UI 标准严格 Clippy、workspace fmt 与差异空白检查通过。最新 Linux GNU ELF 的配置候选/存储身份 2 项、Manager 8 项与普通恢复 4 项，共 14 项通过；普通子进程复用已有 target 下限定输入编译桥，没有安装 Linux 工具链。两份更新专项文档的 116 个本地链接有效。最后的首次应用修正经 Windows 全库及 Linux 相关测试复验，没有第三次独立审查。

本批只覆盖上述共享业务入口。托管订阅的 ProfileApplication 目录事务、直接 Core/client 调用、备份/重启/内核更新，以及普通路径检查后源文件从 TUN-off 变为 on 的竞争仍未统一准入。冻结资源后真实授权/Service Start/失败恢复未串联验收，不能以候选测试证明 GUI 自动交接全链路完成；真实管理员服务、Mihomo/TUN 和 macOS 实机未执行。

### 2026-10-04 Local TUN 交接的授权前候选冻结

[ServiceManager](../../crates/zenclash-core/src/service_manager/tun.rs) 的 Local TUN 命令现在先通过现有 capture/transition/store 准入，冻结受控更新、确切运行缓存及当前配置资源，再查询服务健康和发起系统授权。授权期间不持有这些锁或 Data lease。授权后的既有 [交接入口](../../crates/zenclash-core/src/core_session/service_tun.rs) 消费同一候选，核对会话 Arc 身份、binding/generation、受控 patch 和精确缓存；文件被外部修改时在 Stage/Local Stop 前拒绝，不能重新合并源配置。

首审发现 pending backup 尚可能在授权后首次读取源资源；已将其资源及 delta 也纳入候选，并复核 pending generation/store_root。两个新增普通子进程行为测试覆盖当前/provider/TLS 与备份资源冻结、预检后源文件删除仍可恢复、缺失备份拒绝、锁释放、缓存变更拒绝，以及无效候选不会进入服务健康查询或授权；原 PID/generation 保持。没有实际红测记录。独立二审 PASS（2/2）；随后为严格 Clippy 合并绑定和配置版本参数，未进行第三次审查。

本批不新增持久化格式、依赖或原生维护行为。候选资源准备是完整配置/PATCH 自动服务准入的前置事务，**尚未接入任意有效 TUN 配置的自动授权及交接**；不能以此宣称本地 TUN 配置准入已完成。最终 Windows core 全库 **610 通过/0 失败/2 默认忽略**（612 项）；core/UI all-targets/all-features check、core 完整 CI 严格 Clippy、workspace fmt 和差异空白检查通过。最新 Linux GNU ELF 的候选准备 1 项、无效候选 1 项和 Manager 8 项共 10 项通过；子进程测试复用已有 target 下限定输入编译桥，未安装 Linux 工具链。两份更新专项文档的 113 个本地链接检查通过。真实管理员服务、Mihomo/TUN 及 macOS 实机未执行。

### 2026-10-04 临时 Local 的 HTTP provider 缓存接续

[双槽资源模块](../../crates/zenclash-core/src/service_runtime/local_runtime.rs) 新增有界缓存导出，由 [服务交接事务](../../crates/zenclash-core/src/core_session/service_tun.rs) 在恢复收据仍有效且 Local Stop 确认后调用。仅读取当前固定槽内的 HTTP proxy/rule provider 缓存，检查链接、普通文件及单个/总字节预算；缺失缓存移除旧字节，空缓存仍作为真实文件保留。TLS、file provider 和 GeoData 使用 held bundle，不读取订阅、override、原始 TLS 或原 Mihomo home。proxy 缓存内已持有资源的槽内绝对路径转换回服务相对资源路径。

现有候选先完成预检，Stop 后导出最新缓存并重新 Stage/Validate，再发布和 Start；导出或候选准备失败回滚配置暂存，并经既有确认 Release 后重启 Local 的路径处理。尚未得到真实 ServiceClient 全链路失败恢复证据，不能将静态接线等同实机验收。

新增两项真实文件行为测试覆盖新增缓存、资源字节保留、原 TLS 删除、缺失缓存、非法目录与槽外路径拒绝；没有实际红测记录。扩展既有模拟 Service/普通 Local 子进程恢复测试，验证 Stop 后新缓存上传至新 owner、下一槽物化后字节保持。二审发现提前 Stop 削弱原退出回收断言，已补重新启动及 PID 存在断言后再 shutdown；本批审查达到 2/2，不进行第三次审查。最终 Windows core 全库 **608 通过/0 失败/2 默认忽略**（610 项），core/UI all-targets/all-features check、core 完整 CI 严格 Clippy、workspace fmt 和 tracked 差异空白检查通过。最新 Linux GNU ELF 的双槽/缓存模块 7 项及普通子进程恢复 4 项通过（共 11 个独立用例）；后者复用已有 target 下限定输入编译桥，未安装 Linux 工具链。两份更新专项文档的 110 个本地链接检查通过。最后的退出断言修正经 Windows 全库和 Linux 恢复专项复验，没有第三次独立审查。本地 TUN 配置统一准入、身份持久化、旧服务迁移/升级及三平台真实管理员维护验收仍未完成。

### 2026-10-04 维护代理偏好的事务保存与 GUI 回读

Service→Local 的 owned 维护准备在捕获门内调用 [暂停入口](../../crates/zenclash-core/src/traffic_capture.rs)，复用现有 `SystemProxySession::set_enabled(false, 0)` 事务保存关闭意图并释放自有代理。原路径只 `release_owned`，保留 enabled=true，可能在维护期间被后台 reconcile 重新开启。新增生产入口回归实际先 **0 通过/1 失败**（保存的 enabled 仍 true），修复后五项专项全部通过。

测试使用真实偏好文件、现有 ProductionCaptureBackend/SystemProxySession 和模拟 native 后端，PAC 测试运行真实普通 loopback listener。覆盖关闭后重新打开及 reconcile 不重启、PAC 释放、外部替换保护、原生失败保留偏好/所有权、保存失败恢复旧 live PAC，以及 Off/未知状态不写。未知代理观察在 Service Stop/Release 前拒绝；保存失败阻止继续维护准备。正常退出的 release_owned 未改，仍保留下一次启动偏好。修复有效恢复通过既有 set_enabled 保存开启；Service 准备完成后的卸载及取消卸载继续关闭。

[GUI 维护任务](../../crates/zenclash-ui/src/pages/runtime/tun/maintenance.rs) 在准备失败或维护完成后均后台回读 AppPreferencesStore，只用 SystemProxy 范围同步页面与应用，保留语言等其他字段，原维护错误优先显示。扩展真实 Kit 窗口的旧 owner 拒绝用例：磁盘关闭/UI 旧缓存开启，完成后 UI 同步关闭，语言 En、原 PID/generation 保持；专项通过。未执行系统授权或修改原生系统代理，不能作为真实 Service/admin 串联证据。

最新 core/UI all-targets/all-features check 通过，独立二审 **PASS（2/2）**。Windows core 全库 **606 通过/0 失败/2 默认忽略**、UI 全库 **311 通过/0 失败/1 默认忽略**；core 完整 CI 严格 Clippy、UI 标准严格 Clippy 和 workspace fmt 通过。最新 Linux GNU 测试 ELF 在普通 WSL 用户下执行 suspension（5 项）、maintenance_proxy_（12 项）及 Manager 恢复（2 项）筛选，全部通过；前 5 项包含在 12 项中，不能相加，后两个筛选共 14 个独立用例。本批 tracked/untracked 差异空白检查通过；五份更新文档有 179 个有效本地链接，双语 README 既有缺失的 gpui-kit-migration.md 仍单独记录。尚未完成临时 Local 新缓存导出、无 Service owner 的可见恢复、本地 TUN 配置统一准入、旧服务迁移/升级及三平台实机验收。

### 2026-10-04 修复后的捕获恢复与意图冲突保护

[Manager](../../crates/zenclash-core/src/service_manager/maintenance.rs) 新增 `restore_prepared_capture`，由 [ProfileService](../../crates/zenclash-ui/src/profile_service.rs) 的同一 owned 维护完成任务调用。修复成功或已确认失败/取消后，新鲜健康为 Ready、原恢复收据仍有效且未退出时，使用 held bundle 恢复原 TUN；请求不携带授权许可，健康变化时不能再次安装或启动。原生结果未知不恢复，卸载继续 Local。此前 owned 且 active 的系统代理经现有持久化 owner 单独恢复，保持 Advanced 组合，并使用当前 listener；外部代理、未知观察、不可用内核和零端口禁止写入。

TUN 保存收据、pending commit 和后续代理警告分别送回共享业务状态；代理恢复失败不丢失已保存配置。新用户捕获选择在 CoreSession 共享的内存版本中记录，不新增用户数据格式。普通 Apply、显式服务 TUN 推进意图，自动恢复不推进。Service→Local 准备在等待捕获门前冻结版本，取得门后核对；自动 TUN 与代理恢复在捕获门内核对，观察后、代理写入前复核。代理写入使用恢复结果的 generation，不能采纳之后的新配置版本。

首审发现重新读取当前 generation 与缺少捕获意图版本两项 P1，已修复。二审发现排队后的 owned task 才读取意图版本会接纳新选择，已将冻结移到等待门之前，并补精确控制第一次 Pending 和新 Apply 排队的回归。按规约本批审查已到 **2/2**，最后修正没有第三次独立审查；Windows 全库通过覆盖最后修正。新增 11 项行为回归，没有真实红测记录。

Windows core 全库 **601 通过/0 失败/2 默认忽略**、UI 全库 **311 通过/0 失败/1 默认忽略**；core 完整 CI 严格 Clippy、UI 标准严格 Clippy 通过。测试使用 Core 版本/锁与模拟原生代理，没有写系统代理，也未执行管理员维护；不能作为真实 Service→GUI→native→捕获恢复的验收。最新 Linux GNU 测试 ELF 在普通 WSL 用户下执行维护捕获（8 项）、排队意图（1 项）及 Manager 恢复（2 项），共 11 项全部通过。workspace fmt、tracked 差异和本批 untracked 源文件空白检查通过；五份更新文档有 177 个有效本地链接，双语 README 既有的缺失 `gpui-kit-migration.md` 链接仍单独记录，未扩展本批范围。

修复确认文案与双语 README 已同步接线事实。维护期间及卸载后的系统代理偏好保存尚未补齐；普通 `release_owned` 保留 enabled 偏好，不能把恢复时 `set_enabled` 的持久化视为整个维护偏好流程完成。临时 Local 新缓存导出、失去 Service owner 的可见恢复、旧服务迁移与升级、本地最终 TUN 配置准入和三平台实机验收继续开发。

### 2026-10-04 直接 Service 的普通恢复身份准备

直接 Service 启动没有先前 Local 描述时，TUN 页在展示修复/卸载确认框之前，后台调用 `ServiceManager::discover_maintenance_request`。Manager 从当前 Service owner 的普通 source home 和 accepted profile 路径生成恢复描述，复用正常内核选择顺序；不读取源 YAML、不补 GeoData、不执行内核、不停止捕获、不发起授权。打包普通内核缺少缓存时可先写入既有普通 home 缓存；配置与资源恢复继续使用已确认的双槽方案。

身份准备使用既有命令状态及串行准入，等待前后复核运行时身份/版本。GUI 只接收原窗口、当前页面的结果；已有 Local 描述则不进行文件发现。公开 Service descriptor 仍不暴露受保护 executable/home，不能用于普通恢复。

首审发现两个 P1：外层 home 共享租约与安装内部的新共享租约可能在排他恢复排队时形成死锁；Service descriptor 的 home 始终为空会使直接启动路径被拒绝。修复后安装严格借用现有授权，普通 home 取自当前 owner。新增三个回归覆盖损坏/缺失源 YAML、环境覆盖优先级与身份保留、排队恢复期间真实缓存安装，以及已有 Local 描述的无文件读取分支；没有真实红测记录。独立二审 **PASS（2/2）**。

Windows core 全库 **590 通过/0 失败/2 默认忽略**，UI 全库 **311 通过/0 失败/1 默认忽略**；core/UI all-targets/all-features check、core 完整 CI 严格 Clippy 和 UI 标准严格 Clippy 通过。最新 Linux GNU 测试 ELF 在普通 WSL 用户下执行 discovery（8 项）、resources（8 项）及 maintenance_discovery（1 项），全部通过，包含 setid 拒绝与排队恢复安装。workspace fmt、tracked 差异和本批新源文件空白检查通过。五份更新文档中的 175 个本地链接有效；双语 README 既有的 `gpui-kit-migration.md` 链接指向仓库缺失文件，本批未扩大范围处理。

分层测试没有构造真实绑定的 ServiceClient，不能证明直接 Service→GUI→管理员维护全链路已验收。没有执行系统授权、服务安装/卸载、真实 Mihomo/TUN 或 macOS 原生操作；维护后自动捕获恢复与偏好保存仍未完成。

### 2026-10-04 TUN 页修复/卸载确认与业务提交

新增 [maintenance.rs](../../crates/zenclash-ui/src/pages/runtime/tun/maintenance.rs)，在 TUN 页服务状态区显示修复/卸载按钮，采用标准 Kit 按钮与确认框。确认之前只保存同一 owner 的维护意图；确认后后台调用共享准备及原生提交，页面使用 WeakEntity 接受反馈。按钮按对应 operation 显示等待，服务命令或页面修改期间禁止重复提交；窄窗口允许动作行换行。卸载确认使用 Danger，修复确认使用 Primary，中文/英文命令明确标注打开确认框。

维护成功后重新查询并发布健康，避免卸载成功仍显示旧 Ready。显式重新开启 TUN 时，匹配当前 Local generation 的恢复收据转入既有 `request_enable_tun_after_recovery`，复用持有资源；身份、binding、ready 继续由 core 校验。当前修复说明仅承诺维护后继续 Local、可显式重新开启，并没有把尚未实现的自动捕获恢复写为已完成。双语 README、接入文档与计划同步更新。

新增 GPUI 组件窗口行为测试覆盖两个按钮的 Tab/Enter/Escape、鼠标取消、焦点恢复、PID/config/generation 保持，以及确认框打开后替换 owner 的拒绝。两项均通过；没有实际红测证据。Windows 完整 UI 库 **311 通过/0 失败/1 默认忽略**；最终确认按钮样式调整后，两项专项再次通过。core ServiceManager 模块 **15 项通过**，core/UI all-targets/all-features check、标准严格 Clippy `-D warnings`、workspace fmt 和本批 tracked/untracked 空白检查通过，独立二审 **PASS（2/2）**。

本批没有执行系统授权、服务安装/卸载、真实 Mihomo/TUN 或原生窗口实机操作。GPUI fixture 使用真实组件与普通测试子进程，不能证明管理员维护及真实网络恢复。direct-Service 启动没有普通启动身份时仍被拒绝；原生维护后的自动捕获恢复、偏好保存、Linux/macOS UI 与三平台验收未完成，整体目标继续保持。

后续实机复验（当前未执行）：先从普通 Local 进入 Service/TUN，再通过修复或卸载确认框进入维护；分别确认或取消系统授权，核对唯一 Local PID、TUN 关闭、配置保存及服务健康。操作期间切换页面与正常退出，确认不丢失收据、不遗留内核；修复后显式重新开启 TUN，核对资源接续。另在明暗主题、最小窗口、中文/英文及放大文字下复验按钮换行、焦点与错误消息。直接 Service 启动场景应在身份准备完成后单独验收。

### 2026-10-04 GUI 业务层维护收据与取消生命周期

[ProfileService](../../crates/zenclash-ui/src/profile_service.rs) 接入 `request_service_maintenance`、`prepare_service_maintenance` 与 `maintain_prepared_service`。共享业务命令使用非阻塞准入与 owned completion，准备结果在任务内写入共享状态；页面取消等待不丢失收据。原生提交只接受最新 Arc 准备身份和显式 consent，运行时身份/版本/就绪继续由 Manager 校验。共享状态最多保留最新准备结果及最后一次 Service→Local 资源准备；后来的 Local 命令不丢弃上一恢复资源，native 成功/失败不把准备结果视为已恢复捕获。

首审发现成功 Local 恢复未清理旧 Service 提交确认，可能导致 Local 无法确认旧 token、TUN 控件一直禁用。修后仅在 ready 且当前 generation 匹配时接受恢复版本、清理旧记录，失败及旧结果不覆盖新版状态。新增状态回归直接通过，没有真实红测证据；二审 **PASS（2/2）**。

新增五项行为测试覆盖页面 owner 丢弃后收据保留、缺 consent 与被替换收据在 native 前拒绝、业务命令重叠拒绝、等待者取消后后台保存，以及旧确认清理/新版保护。测试使用无源文件的 stopped Local 和共享业务状态，没有执行服务原生维护；恢复成功清理测试是状态层证据，不代表完整 Service→ProfileService 串联已验证。Windows 完整 UI 库 **309 通过/0 失败/1 默认忽略**，UI all-targets/all-features check、标准 Clippy `-D warnings`、workspace fmt check 和本批差异空白检查通过；接入文档 82 个本地链接通过。

本批没有改动可见界面。GUI 修复/卸载确认框、资源收据用于重新进入 Service、维护后自动恢复与偏好保存仍待接线；Linux/macOS UI、真实管理员维护、Mihomo/TUN 和窗口实机验收未验证。

### 2026-10-04 Local 恢复配置的持久化与失败准入

Local 恢复确认就绪后，将双槽生成的确切 YAML 保存为运行缓存，并比较提交受控 `tun.enable=false`；成功后同步待恢复备份 delta。保存使用已持有的资源和普通绝对路径，不重读订阅/provider/TLS 源。原成功恢复测试新增持久化断言，实际先 **0 通过/1 失败**，修复后通过。

持久化完成归属后台恢复事务，取消前台等待不放弃保存。受控层遇到外部修改时回滚暂存缓存并保留外部 patch；保存失败保留已经发布的 Local owner 和激活 GeoData，返回失败准备结果，阻止原生维护以及新的维护请求。新增普通子进程回归覆盖保存失败后 owner 可退出、资源仍可用；另有真实文件事务回归覆盖比较冲突和取消等待。

独立首审发现过期恢复句柄可通过普通租约 fallback 重新取得写权限。过期保存回归实际 **0 通过/1 失败**；改为严格借用权限、过期拒绝，并把保存 worker 绑定到自身持有的借用 lease。补充排他恢复已排队时仍可保存的有界回归，二审 **PASS（2/2）**。Windows core 全库 **587 通过/0 失败/2 默认忽略**，all-targets/all-features check、完整 CI 严格 Clippy 和 workspace fmt check 通过。

最新 Linux GNU 测试 ELF 在普通 WSL 用户下执行 `service_local_recovery`（4 项）、`local_recovery_save`（3 项）、`failed_local_recovery_blocks`（1 项）和 `local_geodata`（10 项）筛选，均通过；筛选包含重叠测试，不能相加作为独立用例数。两份更新文档的 101 个本地链接与 tracked 差异空白检查通过。

本批补齐维护前的受控 TUN-off 与启动缓存保存，不代表 GUI 修复/卸载、系统代理偏好保存或维护后自动恢复完成。没有执行真实管理员维护、Mihomo/TUN 或 macOS 原生验收。

### 2026-10-04 Manager 修复/卸载准备与原生提交入口

新增 `ServiceMaintenanceRequest` 与 `ServiceMaintenancePreparation`。`request_maintenance` 只接受 Repair/Uninstall，绑定共享 capture gate 身份、binding/generation 与普通启动描述，不执行 I/O 或授权；直接 Service 启动缺少普通描述时要求显式 fallback。`prepare_maintenance` 将 Service 交给既有捕获恢复入口，保留此前捕获观察、Local readiness 与持有资源，并将请求更新到恢复后的版本；已有 Local 保持原 owner，不重读源配置、不启动进程。准备结果不是维护成功收据，`ready()` 为 false 时禁止原生提交。

`maintain_prepared` 要求同会话同代的准备结果及 `with_authorization` 确认，后台核对健康与固定 helper；Repair 的普通内核源检查后再核对原意图，复用原生维护的 pin/hash、授权、排他维护锁和 pending observer。授权期间不持 capture/transition 锁，前台取消不丢弃已准入 command completion。调用方持有准备结果以便失败或取消后接续资源；实际 native 成功仅表示安装维护成功，不自动回到 Service 或保存捕获偏好。Unknown、Unauthorized、不兼容和待维护日志等健康状态暂不进入此入口，旧服务迁移与维护日志恢复须另行接线。

新增五项测试覆盖 Local Repair/Uninstall 准备保持、数值版本相同的其他会话、准备前配置变化、非法操作，以及原生提交前的 consent/foreign/stale 拒绝；没有执行真实原生维护。Windows core 全库 **583 通过/0 失败/2 默认忽略**，all-targets/all-features check、完整 CI 严格 Clippy、workspace fmt 和本批 tracked/untracked 差异空白检查通过。独立首审 **PASS（1/2）**。新增行为直接通过，没有红测证据。最新 Linux GNU 测试 ELF 在普通 WSL 执行 ServiceManager 模块 **17 项全部通过**，未安装新工具或服务。可复验 `cargo test -p zenclash-core --lib --all-features --locked service_manager`；只复验本批时筛选 `service_maintenance_`。

Service 分支的 Manager 整体行为、原生失败后的自动恢复、卸载后的捕获偏好保存和 GUI 仍待实现/验证，不能把 API 存在或 Local 拒绝测试当作完整维护工作流验收。没有真实管理员服务、Mihomo/TUN 或 macOS 原生验收。

### 2026-10-04 捕获层与 Local 恢复的共同准入

新增 `TrafficCaptureSession::recover_service_to_local`，核对同 CoreSession 的 capture gate、请求 binding/generation 和 Service owner 后，在一个 owned completion 中保存捕获观察、释放 ZenClash 自己的原生系统代理，再把持有的 gate 交给 Core runtime 恢复。排队阶段取消不产生原生写入；准入后取消调用者不丢弃 completion，退出仍等待捕获锁。返回 `ServiceLocalRecoveryOutcome`，提供此前观察与可用于资源接续的 runtime outcome；不授予维护权限，也不保存捕获偏好。释放失败不得进入 Core 恢复，未知 runtime 不自动重启或恢复代理。

首审发现配置应用不取 capture gate，恢复等待 write lease/transition/binding mutation 时可能接受过期 generation。transition 等待回归实际 **0 通过/1 失败**；修后原 generation 传入 Core，并在后台计算、write lease、transition 和 binding mutation 等待后核验，拒绝过期请求进入 Stop/Export/Release。二审 **PASS（2/2）**。另外三项测试覆盖 capture 排队取消、排队期间版本变化和另一会话在 capture 准入前被拒绝；它们没有执行原生 Service 或真实网卡操作。

Windows core 全库 **578 通过/0 失败/2 默认忽略**，最新 all-targets/all-features check 与完整 CI 严格 Clippy 通过。最新 Linux GNU 测试 ELF 在普通 WSL Linux 执行恢复准入/收据 **7 项**、runtime 恢复组合 **3 项**均通过；复用现有 target 临时编译桥，没有管理员维护操作。可复验 `cargo test -p zenclash-core --lib --all-features --locked service_recovery_` 与筛选 `service_local_recovery`。本批差异空白检查通过（CRLF 按 Windows 行尾处理）。此批是捕获释放到 runtime 恢复的组合入口，尚无真实 ServiceClient→原生代理→Local 成功恢复的整体验收；原 runtime 组合测试仍使用模拟 transport 和真实普通子进程。Manager Repair/Uninstall、管理员授权和取消恢复、GUI 调用、捕获偏好保存仍未接入，不能把入口存在当作维护流程完成。

### 2026-10-04 恢复资源向新 Service 会话接续

`CoreLocalRecoveryOutcome` 保留关闭 TUN 的不可变 bundle，绑定原 CoreSession 的身份、generation 与 binding generation；不持有已退休进程或事务门栓。`ServiceManager::request_enable_tun_after_recovery` 只接受同会话、同代、就绪的 Local 恢复结果，继续经过原授权、健康检查与 capture 交接。过期结果、数值版本相同的其他会话、子进程停止未确认的结果均拒绝。调用方释放 outcome/request 后可释放这份资源；没有新增持久化格式。

显式恢复交接使用持有的 YAML 和资源准备 TUN 更新，不重新读取订阅、override 或运行缓存；普通配置准备路径保持原行为。服务仍需新会话与新 revision。现有恢复测试增加删除源 YAML、文件 provider 和 TLS 证书后的普通 Local 启动，再向新模拟 Service 会话提交持有资源，并验证另一恢复槽内嵌套证书字节。新增测试覆盖恢复收据身份/版本边界和损坏运行缓存下的持有配置准备。模拟 Service transport 与真实普通 Rust 子进程不代表真实管理员服务或 Mihomo。

Windows core 全库 **574 通过/0 失败/2 默认忽略**；全目标/全 feature check、完整 CI 严格 Clippy、workspace fmt 与 tracked/untracked 差异空白检查通过（CRLF 按 Windows 行尾处理）。限定本批独立首审 **PASS（1/2）**，未发现功能性缺陷。本批新增测试直接通过，没有行为测试先失败的证据。Linux 最新源码通过现有 WSL 普通编译桥构建，恢复组合 **3 项**、恢复收据 **3 项**、持有配置准备 **1 项**全部通过。首次沙箱构建无法启动 WSL（`E_ACCESSDENIED`），通过审批以普通 WSL 用户重试后成功；不是行为测试失败。

可复验命令：`cargo test -p zenclash-core --lib --all-features --locked service_local_recovery`、同命令筛选 `service_recovery_receipt` 和 `service_tun_recovery_preparation`。本轮 Linux 使用最新 Linux GNU 测试 ELF、target 下限定输入的普通子进程编译桥执行这些专项，未运行整个 Linux core 测试库或新增运行时工具。

尚未接入 GUI Repair/Uninstall 的捕获冻结、维护授权与取消恢复，也未接入所有 Local 有效配置启用 TUN 的自动授权流程。这份 bundle 对应停止旧 Service 时的快照，临时 Local 运行期间新增的 HTTP 缓存尚未导出；跨会话缓存与身份持久化、旧服务迁移和内核升级仍未完成。没有真实 Mihomo/TUN、管理员维护或 macOS 原生验收。

### 2026-10-03 Core 服务退出与普通 Local 恢复组合事务

新增 `CoreSession::recover_service_to_local`，在 capture publication、写入租约、transition 与 binding mutation 准入下完成 runtime 恢复。优先使用保留的普通启动身份；直接服务启动可由调用方提供普通身份 fallback，必须为 Mihomo 且 home 一致，不能从 helper 推导 binary。先检查普通可执行文件，再确认服务 Stop/导出最新缓存、生成关闭 TUN 的双槽配置；普通 home GeoData 激活后在持有租约下执行本地预检。Release 成功确认后才发布停止的新 Local owner，并授权启动和等待 controller 就绪。取消外层等待不会取消已准入 Core completion。

未知 Release 不进入 publication 或 Start。启动/就绪失败后再次确认普通子进程停止，才允许 GeoData 回滚；停止不确认时保留已激活资源与已发布 owner，通过 `CoreLocalRecoveryOutcome.failure` 阻止继续维护。`LocalGeoDataRecovery` 的预检与启动共享既有租约授权，避免排在待恢复排他租约之后重新申请共享租约。

新增三项组合行为测试使用模拟 service transport、真实普通 Rust 子进程与 HTTP 就绪夹具，覆盖源配置删除后成功恢复、Release 未确认时零 Local publication，以及就绪失败后保留停止 owner、恢复旧 GeoData；没有将模拟 transport 计作真实服务。首审发现网络暂停标记在恢复成功后未清除，回归实际 **0 通过/1 失败**；修后 ready 且非 shutdown 时清除，验证后续操作与监督器重新准入。二审 **PASS（2/2）**。Windows core 全库 **570 通过/0 失败/2 默认忽略**，全目标/全 feature check、完整 CI 严格 lint、workspace fmt 与差异检查通过。普通 WSL Linux 恢复组合 **3 项**、GeoData 授权/回滚 **10 项**、网络暂停状态 **1 项**均通过；复用 target 下临时限定输入编译桥运行普通 Rust 子进程，没有安装运行时工具或管理员服务。

在已有 Rust 开发工具链中可复验：`cargo test -p zenclash-core --lib --all-features --locked service_local_recovery`、同命令筛选 `local_geodata` 和 `suspended_session_successful_local_recovery`。WSL 本轮使用 Windows 编译的 Linux GNU 测试 ELF 执行专项，未将交叉编译结果当作 native macOS 证据。

此批只组合 runtime 层事务，调用方仍需冻结捕获意图并释放原生捕获。GUI Repair/Uninstall、捕获偏好保存、授权取消后的修复恢复与结果未知处理尚未接线；恢复 Local 后再次进入 Service 的持有资源接续也需完成，不能重新依赖已删除的源。未验证真实 Mihomo/TUN、管理员服务、macOS 原生或强制制造子进程停止失败。

### 2026-10-03 原 Local 启动描述随服务 owner 保留

Local→Service 的 TUN 交接现在核对原 home 一致性，并用 `RuntimeSession::from_local` 保留原 `MihomoLaunchConfig` 值；常规同 home 的服务绑定切换也传递该描述。仅保存路径、内核种类与普通 controller 启动信息，不永久保留旧进程 Arc，不读取或采用 protected helper binary。服务直接启动仍无 Local 描述；源 YAML 删除、服务 Release 清理运行状态后，原启动描述保持。原 YAML 路径只是身份，后续恢复仍必须物化 accepted bundle 的字节、重新核对普通可执行文件。

`CoreSession::local_recovery_launch` 通过借用当前 watch 读取已准备内存，返回当前 Local 描述或 Service 持有的描述；External 返回 None。行为测试覆盖源删除后元数据保留、Release 后保留、直接 Service 不构造 Local 描述，以及 getter 不增加进程 Arc 计数和切换 External 后清除恢复描述。限定生产改动与两项会话测试的独立首审 PASS（1/2），随后追加 getter 行为测试；Windows core 全库 **566 通过/0 失败/2 默认忽略**，Windows/普通 WSL Linux 会话模块各 **38 项通过**，Linux getter 专项 **1 项通过**。Windows 全目标/全 feature check、完整 CI 严格 lint、workspace fmt 与差异检查通过；Linux 最新源码重新编译及专项执行通过，未验证 macOS 原生或管理员服务。此批尚未组合完整 Stop/导出/物化/Release/发布/启动事务，也未接 GUI 修复/卸载。

### 2026-10-03 恢复前创建并发布停止的 Local owner

新增 `MihomoProcess::prepare_stopped`，只构造内存中的停止 owner，不读文件、不启动子进程、不授予启动权限。维护恢复可先把该 owner 发布到共享绑定，再通过既有 `LocalGeoDataRecovery` 与持有租约进行普通启动；就绪失败或外层等待取消时，所有者仍可由会话退出路径触达。该入口不做配置或可执行文件校验，授权启动路径仍必须执行原有校验。

两个新增行为测试使用真实普通 Rust 子进程与 HTTP 就绪夹具，验证发布前无新 PID、启动成功后 session shutdown 回收新 PID，以及无效可执行路径启动失败后保持新 owner 和回滚旧 GeoData。限定本批独立首审 PASS（1/2），Windows 全目标/全 feature check 与完整 CI 严格 lint 通过；Windows core 全库 **563 通过/0 失败/2 默认忽略**，Windows/普通 WSL Linux 恢复模块各 **10 项通过**，workspace fmt 与差异检查通过。Linux 复用上一批临时限定输入编译桥，未执行管理员服务或真实 Mihomo/TUN；macOS 原生未验证。本批尚未新增 GUI 维护调用，原 Local 启动描述保留、服务 Stop/Release 与新 Local owner 的整体接线仍待实施。

### 2026-10-03 普通用户 GeoData 激活与失败恢复

按用户确认的 §5.5 新增 `ServiceRuntimeBundle::with_local_geodata`：从持有资源对原 home 的固定 GeoData 名称执行普通权限原子替换。MMDB 沿用现有名称选择，避免旧别名遮蔽新资源；所有目标与旧字节先检查，单文件 128 MiB、备份总量 256 MiB，读取时受剩余预算约束。正常错误恢复原字节或原先不存在的状态；外层等待取消后，已准入 completion 继续持有租约与 mutation 门栓直到完成。调用前必须确认使用该 home 的内核已停止；启动后返回错误前也必须确认子进程已停止，回滚失败时禁止继续本地启动。

首审发现旧回调在排队的恢复排他租约后重新取得共享租约，会导致恢复启动死锁；实际回归先失败，修后传入 `LocalGeoDataRecovery`，使用已有租约授权启动，过期授权或开启 TUN 的配置均不得启动子进程。二审 PASS（2/2）。测试使用真实普通 Rust 子进程及 HTTP 就绪夹具，不是真实 Mihomo 或高权限服务。二审后补充剩余备份预算读取边界及回归，未进行第三次审查。

最终 Windows core 全库 **561 通过/0 失败/2 默认忽略**，全目标/全 feature check、完整 CI 严格 lint、workspace fmt 与差异检查通过。普通 WSL Linux 本模块 **8 项通过**。此前 Linux 七项测试首次执行为 4 通过/3 失败，原因是普通 WSL 无 rustc；使用仓库 target 下临时限定输入的编译桥后复验通过，没有忽略失败用例或安装工具链。Linux 严格 lint 已在补充读取预算边界之前通过，最终 Linux 源码重新编译及八项执行通过；没有 macOS 原生或真实 Mihomo 验收。

此批提供 GeoData 事务与受限本地启动入口，尚未接入 GUI 修复/卸载、服务 Stop/Release、新 Local owner 发布或跨会话身份持久化，不能计作维护流程完成。

### 2026-10-03 普通用户双槽恢复资源物化

用户已确认 §5.5 的 `ControlledConfigStore.root()/local-runtime/{slot0,slot1}`、关闭恢复配置 TUN、保留原 home，以及后续固定 GeoData 原子激活与回滚范围。新增 `ServiceRuntimeBundle::materialize_local_runtime`，从持有的不可变 YAML/资源生成配置、文件/HTTP provider、TLS 与 planet 资源，不再读取订阅或 TLS 源；MRS 字节保持不变，proxy provider 内部 TLS 引用改写为该槽绝对路径。缺失 HTTP 缓存获得该槽内的可写路径，主恢复 YAML 强制 `tun.enable=false`；普通权限 worker 中准备 pending 槽后替换非活动槽。已准入 completion 持有普通写入租约与 store mutation，外层取消不丢弃后台准备。调用方必须传递真实活动槽路径，不能据此丢弃运行中槽；暂无 GUI 调用方。

资源沿用 256 个持有文件、单文件 128 MiB、总计 256 MiB 与主 YAML 4 MiB 的预算，改写后的 provider 重新核对单文件与总预算。固定两槽，暂存 pending/previous 仅替换非活动槽；静态目录链接、Windows reparse/junction、非普通文件及越界活动路径拒绝。Unix 新槽为 0700，文件复用原子写入的 0600 路径。节点数计入最多 256 个随后生成的 HTTP 缓存；这些静态检查不证明同权限恶意进程同时换目录的原生竞争防护。

首审发现本地后来生成的缺失缓存超过旧槽节点上限，第三次轮换会误拒绝；实际专项 **0 通过/1 失败**，联合节点上限修复后通过。Windows 修后专项 **5 项通过**，包含实际普通权限 junction、源删除后持有 TLS/provider 恢复、失败保留活动槽与 HTTP 缓存填充后的轮换；core 全库 **553 通过/0 失败/2 默认忽略**，check、完整 CI 严格 lint、workspace fmt 与差异检查通过；二审 **PASS（2/2）**。普通 WSL Linux 修后专项 **5 项通过**，包含真实符号链接拒绝。未证明 macOS 原生、真实 Mihomo 本地恢复、管理员服务或 TUN。

本批仅实现普通权限恢复文件生成。GeoData 字节目前保存在槽内，尚未对原 home 固定名称执行激活/回滚；Local 启动描述、原生服务 Stop/Release→新 Local owner→维护授权、GUI 操作，以及跨会话缓存/节点身份仍未完成。四项总体目标保持开发中。

### 2026-10-03 四项优先开发与完整缓存读取前置

用户将当前优先范围调整为 GUI 修复/卸载与恢复、本地 TUN 配置准入、缓存与身份持久化、旧服务迁移与内核升级；其余工作暂缓。用户已明确选择：配置操作进入授权与服务交接，成功后再应用，不以只拒绝并提示作为最终实现。该配置接线尚未完成。

首批新增 `ServiceClient::read_complete_provider_cache`，复用现有 Begin/Read/Finish 协议，持同一 IPC 通道锁到结束，避免同客户端 Start/Release 插入分页。逐页核对偏移、256 KiB 上限、总长度、结束标记和 SHA-256；proxy 上限 4 MiB、rule 上限 128 MiB，缺失与空缓存分别返回 None/空字节。成功 Finish 后才返回完整内容；传输失败与超时不重发，取消或失败的服务快照由现有服务期限和 owner 清理回收。没有新增依赖、协议字段、用户文件或持久化格式。

Windows 完整服务回归 **319 通过/0 失败/1 默认忽略**，普通 WSL Linux **278 通过/0 失败/2 默认忽略**；缓存专项 **7 项通过**。Windows/Linux GNU x64/macOS ARM64 服务全目标、全 feature 完整 CI 严格 lint 通过。首审发现等待 channel 锁不在十五秒期限内，新队列回归实际 **0 通过/1 失败**，修复后将锁等待纳入同一期限，证明排队超时不发送 Begin；二审 **PASS（2/2）**。macOS 仅交叉编译，未原生执行。

该读取批次只完成客户端前置，不能据此勾选四项完成。后续 CoreSession 停止与内存导出进展单独记录如下，普通用户原子保存、缓存恢复、GUI 维护和身份持久化仍待接线。

### 2026-10-03 CoreSession 停止与内存缓存导出

新增 `CoreSession::export_service_runtime`：沿用写入租约、会话 transition 与 binding mutation 门栓，已准入完成任务保留事务所有权，取消外层等待不会中断停止/导出。会话确认原生 Stop 的状态、accepted revision 且无候选，再调用完整缓存读取；整个导出读取阶段限制十五秒，发布前再次核对停止状态与 revision。全部成功才发布新的不可变资源 bundle；失败保留旧 accepted bundle 与显式停止意图，不自动重启或 Release。TLS、文件 provider、GeoData 和原 YAML 保持，缺失缓存删除旧 seed，空缓存保留空资源。未新增持久化格式、依赖或用户文件写入。

配置版本在最后一次缓存读取后变化的回归实际先失败，再经发布前新鲜状态核对修复。首审发现逐个替换新旧缓存时中间预算误拒绝：原缓存合计 256 MiB、最终仅两字节的专项实际 **0 通过/1 失败**，修后先在候选副本移除全部待导出的旧 HTTP seed，再按保留资源与新缓存核对预算。修后 Windows 资源与会话专项 **49 项通过**、core 全库 **548 通过/0 失败/2 默认忽略**，全目标/全 feature check、完整 CI 严格 lint、workspace fmt 与差异检查通过；二审 **PASS（2/2）**。普通 WSL Linux 修后资源与会话专项 **49 项通过**。真实高权限服务、Mihomo/TUN、macOS 原生和 GUI 接线未验证。

本批是维护恢复所需的内存导出入口，尚未接 GUI、普通用户原子落盘、本地资源物化、跨会话缓存/节点身份以及旧服务升级迁移。配置自动授权与服务交接仍按用户确认方向实施。

### 2026-10-03 原生跨进程维护锁退出回归

补齐 §7.3 的真实进程边界：当前测试程序启动独立普通权限子进程，分别持共享锁与排他锁；就绪后验证另一进程的维护排他锁被拒绝，共享模式仍允许另一 owner、排他模式拒绝另一 owner。强制终止并回收子进程后，验证可以在相同原生文件身份上重新取得维护锁。父测试失败时也回收其子进程。仅新增测试，没有改变生产协议、依赖、配置或服务维护行为。

最终 Windows 维护锁整组 **7 通过/0 失败**，普通 WSL Ubuntu 26.04 用户 zen 整组 **9 通过/0 失败**；macOS ARM64 all-targets/all-features check、Windows service 全部 CI 严格 lint、全仓格式检查及差异检查通过。独立首审发现退出后立即断言锁释放可能误判，按 [LockFileEx 官方退出语义](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-lockfileex) 修为五秒内仅重试 WouldBlock，其他错误立即失败；第二轮审查 **PASS（2/2）**。此测试不证明管理员路径保护、原生管理器维护或 TUN；macOS 仅编译未执行。可重复命令见 [原生验收入口](tun-service-native-validation.md)。已有生产逻辑在新增用例下通过，没有制造行为红；最初测试编译的借用错误已修复，不计作行为失败。

### 2026-10-03 Linux release 与 macOS ARM64 编译补验

上一目标轮完成 v3 核心接线及阶段验证，本轮继续核对实际剩余项，没有将停止但仍注册的服务修复缺口解释为功能完成。Linux §7.4 的固定 busctl 运行时要求已再次提出确认；此前“参考上游”未批准此新增要求，上游核对仍只有 systemctl 维护，没有可直接复制的完整归属查询。

当前 v3 Linux x64 GNU release helper 已构建，并以普通 WSL 用户 zen 验证 x86-64 ELF64 PIE 与 `--version` 退出 0；本地符号需求最高 GLIBC_2.39，正式旧发行版兼容、DEB/RPM 和原生安装未验证，摘要与构建边界见 [打包记录](tun-service-packaging.md#2026-10-03-v3-本地-release-helper-验证)。补充既有 macOS ARM64 目标标准库后，service all-targets/all-features check 与全部 CI 附加严格 lint 退出 0；没有链接 Apple SDK 或执行 macOS 原生查询。本轮没有修改 Rust 源码、Cargo 依赖或发布流程。

### 2026-10-03 原子维护准入核心接线阶段验证

用户明确确认按主计划 §7.3 实施，不新增 Rust 依赖或用户配置格式。共享原生锁、实际 State 会话准入与协议版本的三个行为用例均实际失败后开始修复；编译失败未计作行为失败。当前协议 v3 的核心接线通过本批阶段验证，整个三平台计划仍未完成。

- 服务 owner 持有稳定文件的原生共享锁；幂等 Acquire、Stop、断连保留锁，完整停止与清理成功后 Release 才释放。维护持排他锁时允许原生 Hello，新 Acquire 返回 MaintenancePending；业务目录恢复延迟到首次共享准入。
- 维护在恢复、暂存及 journal 发布之前核对固定宿主、同一原生 IPC 的 PID/出生身份及真实 v3 Hello。活动宿主与管理器 PID 关联在 Stop 前和新宿主提交前再次核对，停止后确认原进程退出；未知保留恢复材料。
- Linux 复用固定 systemctl 和有界输出核对 MainPID；macOS 新固定 root 只读 `--query-host-pid` 使用系统框架 typed PID 与五秒查询子进程。没有增加 busctl 要求。macOS 新 CF 对象边界测试已通过交叉编译，但没有本机运行，SDK 链接和真实字段仍待验收。
- 新安装元数据仍为 schema 1，写入协议 3；旧 1/2 可只读保留，普通握手拒绝在 Acquire 前。仅显式 Repair 接受一次 `--offline-migration`，要求管理器明确 absent 且 endpoint quiet，GUI 不自动传递。管理员须在此之前结束旧应用、受控停止/注销并确认旧宿主退出；该 marker 没有事前旧 PID 收据，不能单独证明旧进程已退出。包拥有 unit 的 loaded/inactive 迁移与完整 GUI 流程仍未完成。legacy journal 恢复旧字节不会重新注册或启动旧 helper。

本批最终 Windows 完整 service **311 通过/0 失败/1 默认忽略**，普通 WSL Linux **270 通过/0 失败/2 默认忽略**。service Windows/Linux GNU x64/macOS Intel all-targets/all-features check 与全部 CI 附加严格 lint 通过；workspace all-targets/all-features check、格式和差异检查通过。首次 P1/P2 修复后第二轮最终审查 **PASS（2/2）**，未进行第三轮审查。Windows 普通用户真实 Mihomo 日志夹具抽取后复验 **1 项通过**；Unix 负向真实专项见下文。未执行管理员安装、原生 manager 维护、macOS framework 链接、Unix root 正向日志或 TUN。

中间验证：Windows 完整 service **308 通过/0 失败/1 忽略**，普通 WSL Linux **266 通过/0 失败/2 忽略**；Windows/Linux GNU x64/macOS Intel 的 service all-targets/all-features check 通过。首审发现 P1：业务授权名单阻止原生 root 维护身份进入 Hello，实际政策回归先失败，修复后完整库通过；该普通 runner 的政策夹具不证明真实管理员 token。首审另发现 P2：启动新宿主后、提交 journal 前少了管理器 PID 核验，实际 post-ready 门禁回归 **0 通过/1 失败**后开始修复；因此以上是中间结果，不是本批终态。

新增 Unix 真实日志夹具默认忽略。普通 WSL Ubuntu 26.04、用户 zen，Mihomo v1.19.30 Linux amd64（SHA-256 `3E92DF24F5E80E86B9CF9183CEB7BB575F0BD132A9DC4081DAE42E80F21076AE`）的真实非 root socket 拒绝专项 **1 通过/0 失败**：生产原生身份校验拒绝 HTTP 发送与日志订阅，并回收自身子进程。TUN/DNS 关闭，仅回环目标；不证明受保护服务或 TUN。UID 0 正向日志专项及 macOS 原生运行未执行，命令与权限要求见 [原生验收](tun-service-native-validation.md)。

### 2026-10-03 Linux/macOS 维护批次

用户指定优先开发 Linux/macOS、真机环境随后提供，并要求参考本地上游。继续使用现有 systemctl/launchctl；没有引入 busctl、Rust 依赖、持久化格式或协议版本变化。macOS Enable 来源已补入 [UPSTREAM.md](../../crates/zenclash-service/UPSTREAM.md)，Linux 磁盘模板门禁和缺失查询属于项目适配，不冒充上游复制。

- macOS：完整已批准 plist、明确未加载时按上游先 Enable 再 Bootstrap；失败短路，每次副作用前重验文件。Start 缺 plist 时零 Enable/Bootstrap/Kickstart；已加载且磁盘有效仍走 Kickstart。首轮行为 **10 通过/4 失败**，修后 portable macOS 注册行为 **16 项通过**（包含最终完整服务库）。真实 disabled 状态恢复与 loaded job 归属仍待 macOS。
- Linux：新增固定 `/etc`、包 `/usr/lib` unit 的保护、16 KiB 完整模板、固定句柄身份及 effect 前重验；应用仅删除 `/etc` override。早期门禁在 root/journal 变更前，四维护方法调用同一实际调度。新调度旧 exists 基线 **3 通过/4 失败**，不是运行真实 systemd 的失败证明。
- 首审指出既有 P1：两处磁盘 unit 缺失时跳过 Stop 会使后续卸载、部署和恢复继续删改文件。新增回归 **11 通过/4 失败**；修后生产路径先观察已有固定 systemctl 的 LoadState、ActiveState、MainPID，唯成功、无 stderr、三个唯一字段为 not-found/inactive/0 才继续。错误阻断部署删除/替换并保留恢复材料。共享 64 KiB 输出预算，30 秒查询期限，超时终止并最多 5 秒回收自身子进程；普通 Unix fixture 另覆盖输出预算、完整 UTF-8 与真实子进程退出。
- 最终 Windows 完整 service **283 通过/0 失败/1 默认忽略**；普通 WSL Ubuntu 26.04、用户 zen 的 Linux ELF 完整 service **239 通过/0 失败/0 忽略**，包括 **21 项 Linux 注册/查询行为**及 Unix 子进程测试。使用已核对临时桥接，不写项目工具链配置。Windows/Linux GNU x64/macOS Intel service all-targets/all-features check 与 CI 全部附加严格 lint 通过；workspace all-targets/all-features check 通过。首审 P1 修复后第二轮最终 **PASS（2/2）**，没有第三轮审查。
- 没有运行真实 systemctl 维护、管理员安装、launchd 或 TUN；macOS 结果是 portable 行为和交叉编译，不能宣称原生执行。有效 loaded unit/job 的执行归属、原子维护准入、缓存持久化和迁移仍未完成；[真机验收入口](tun-service-native-validation.md) 增加本批步骤。

### 2026-10-03 日志流批次证据

- 实际行为红：Service 内核请求固定 debug/缺失格式，128 KiB 预算未生效；core 参数丢失、非法 query 未在连接前拒绝、迟到错误连接新控制器。修复后相关回归与完整库通过。
- 新协议操作的两项日志选项均为必填、拒绝未知字段；core 拒绝未知、重复及非法 query。旧 v2 helper 关闭新操作连接时保留控制 owner，不伪装 typed Incompatible；不重试旧操作、plain 或 Local。
- Windows 原生管道忙五项测试通过，覆盖实例重建、五秒期限、取消零后台连接、其他错误零等待，以及打开与握手共用期限。首次夹具错误曾导致失败及挂起，已终止该轮并修正实例生命周期，再全部通过；没有放宽生产期限。
- 真实 `real_mihomo_pipe_logs_preserve_options_filter_warning_and_cancel_cleanly`：沙箱内曾失败，Mihomo 已监听但后续 Accept 返回 AccessDenied；获准在沙箱外以同一普通用户执行后，首先发现 231 管道忙，修复后 **1 通过/0 失败**。Mihomo v1.19.30，Windows amd64，二进制 SHA-256 `F55B3028D9160BEB9044F21B05DD7405B46524614A19642D6291492F5F985761`。临时配置关闭 TUN/DNS，仅回环通信；覆盖 structured/plain、warning 过滤、同句柄 PID、关闭及重订阅。权限错误的具体沙箱成因尚未由 OS 审计证明，不据此要求管理员。
- 最终 Windows service 完整库 **263 通过/0 失败/1 默认忽略**；上述真实专项单独显式运行通过。core 完整库 **540 通过/0 失败/2 默认忽略**。workspace all-targets/all-features check、core/service CI 全部附加严格 lint、`cargo fmt --all -- --check` 与差异检查退出 0；Linux GNU x64/macOS Intel service 交叉 check 退出 0。独立首次与第二轮最终审查均 PASS，第二轮含管道重试及真实专项夹具；没有第三轮审查。
- 未运行管理员安装、SCM/launchd/systemd 服务启用或 TUN；未证明 Linux/macOS 原生日志运行、受保护 `Kernel::start`、GUI 日志实机表现或整个 workspace 测试/lint。本批不新增依赖和持久化结构，协议保持 v2；§7.3 维护锁能力仍未实现。

采用“直接移植上游有用代码，再按 ZenClash 当前实现适配”的方式，见 [上游代码移植计划](tun-service-upstream-migration.md)。Linux SELinux、Windows SCM、资源差异规划及同会话缓存继承已有适配、行为测试与来源记录；provider 底层回读已通过两轮审查，高层保存与恢复、跨会话稳定目录和节点身份导入仍未完成。源码落地、阶段检查通过和三平台验收分别记录，不合并为功能完成。

| 范围 | 最新阶段结果 | 尚缺的交付条件 |
| --- | --- | --- |
| v3 原子维护核心接线 | 原生共享/排他锁、State 与维护 worker 已接线；Windows 311、普通 Linux 270 项通过，三服务目标 check/CI lint、workspace check 与修后最终审查通过 | macOS 原生框架/管理器、三平台真实维护与 TUN；完整 loaded/inactive 归属及包拥有旧服务迁移未完成，offline marker 不能替代管理员旧宿主退出证明 |
| Linux 固定 unit 与缺失停机证据 | Windows Linux 注册行为 15 项、普通 Linux 21 项通过；完整库 283/239 项，三目标 check/CI lint 与修后最终审查通过 | 磁盘有 unit 时系统实际加载的执行归属、systemd/Polkit 原生维护验收；不能用磁盘模板替代有效归属 |
| macOS Enable 恢复 | 适配上游 Enable→Bootstrap；缺/变更 plist 或 Enable 失败不继续。注册行为 16 项、macOS 服务交叉 check/CI lint 与最终审查通过 | 真实 disabled 恢复、launchd/授权与 loaded job 归属验收 |
| Service 日志流参数 | `SubscribeLogs` 贯通具名等级/格式；core 540 通过/2 忽略，service 263 通过/1 忽略；check、core/service 完整 CI 附加 lint、两轮审查通过。Windows 普通用户真实 Mihomo 日志专项 1 项通过；Linux/macOS service 交叉 check 通过 | 已完成本批适配；三平台高权限服务、受保护内核启动与 TUN 原生验收仍待执行 |
| 当前 Windows release helper | 2026-10-03 最终源码重新构建退出 0，版本入口返回 `zenclash-service 0.2.0`；文件大小及 SHA-256 见 [打包记录](tun-service-packaging.md#windows-产物) | 当前环境无 Inno Setup 编译器；不代表安装包、UAC、SCM 或 TUN 验收 |
| macOS helper 版本非空白门禁 | 新脚本批次实际先失败再通过：完整受控打包 fixture、bash 语法和差异检查通过，独立首审 PASS（1/2）；拒绝时不签名、不替换已有暂存内容 | 使用工具替身，不代表真实 Mach-O、codesign 或 Gatekeeper 验收 |
| Managed Local 配置 TUN 准入 | 最终 effective `tun.enable=true` 的统一准入仍未接线，未执行 TUN | 用户已确认进入授权与服务交接后应用；覆盖配置、override、备份、更新与自动恢复，见 [core §5.6](tun-service-core-integration.md#56-managed-local-配置的-tun-准入缺口) |
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
| 重启后直接使用服务 | main/UI 生产接线的首审 P1/P2 已修复并通过第二轮审查；Windows 完整 core 534、Linux 完整 core 577 项通过，各 2 项忽略；初始化 9 项、ProfileService 14 项及 i18n 4 项通过，core/UI check、标准严格 clippy、core 附加 lint 和 Linux core 完整 CI lint 通过 | 无 owner 已确认维持退出并完成双语提示；三平台真实服务启动仍待验收 |
| 资源持久性与发布 | 三平台 payload 暂存、Linux 包协调已有阶段证据 | 跨会话状态、旧 setuid、内核副本升级、Windows/macOS 维护协调及三平台真实验收 |

已收尾阶段与正在开发的差异如下；下文历史结果仍只对应各自阶段：

- CoreSession 唯一 owner 生命周期阶段完整 core 回归 **483 通过、0 失败、2 忽略**，owner 生命周期 **18 项**、ServiceRuntimeSession **20 项**、捕获门栓 **15 项**通过；Windows all-targets/all-features check、严格 clippy 和两轮独立审查完成。已确认停止意图、停止后维护失败的 generation、授权取消和捕获任务取消均有实际失败再通过的证据。该阶段只保证捕获收尾后发布新 owner；后续首次开启交接的证据见下文，其他切换与维护不在该阶段范围内。
- UI 的实际 A→B 来源切换、停止后的页面状态和跨 generation 延迟回调回归已通过，该阶段两轮独立审查结束。此前完整 UI 为 **252 通过、1 失败**，失败涉及 profile 列表投影；最新 Windows 完整 UI 库已重跑为 **294 通过、0 失败、0 忽略**，binary **11 项通过**，此前失败不是当前结果。check 与标准严格 clippy 通过；完整 CI 附加 lint 仍有五项问题，见下文。GPUI 行为测试不等于真实窗口、Mihomo 或 TUN 验收。
- 服务端部分配置阶段完整服务测试 **160 项通过**，PATCH 模块 **15 项**、部分资源事务 **5 项**通过；Windows all-targets/all-features check、严格 clippy、crate fmt 和两轮独立审查完成。固定 Mihomo `v1.19.30` 的省略 enable、GET 默认字段和不支持局部 PATCH 的顶层字段问题已修复；不支持的字段保留完整配置事务。测试使用受控 HTTP fixture，真实 Mihomo 连接保留仍待验收。
- core 部分配置业务接入已完成阶段验证：**3 项先失败的业务测试修复后通过**，覆盖变化中的源配置不混入缓存、取消等待后持久化与缓存一致、旧备份重试保留后来保存的模式；安全 effective delta 响应与保存收据已接线，另有 **5 项服务局部事务边界测试通过**。首审发现的 **1 项 P2**（停止后未保存 PATCH 候选阻塞 accepted 重启）已通过复用 Restore 修复，第二轮最终审查通过。停止恢复 core **4 项**、service 候选清理与失败重试 **2 项**通过；该阶段完整 core **500 通过、0 失败、2 忽略**（包含 manager 四项生命周期测试）、完整服务 **182 项通过**，两 crate check 与严格 clippy 通过。后续首次 Start 和回读差异不在本结果范围内。
- Windows SCM 上游补缺的行为测试 **13 项通过**，覆盖维护期间阻止自动恢复、固定宿主退出及删除完成等待；check、严格 clippy、fmt 与首次独立审查通过。没有运行真实 SCM 安装、UAC 或系统服务删除。
- 随后 Windows SCM 注册身份批次移植上游 `registered_executable`、`check_service_registration` 和适用的 `review_security`，固定保护目录 helper 与唯一 `run` 参数、own-process/LocalSystem、无依赖及 load-order group；同一 SCM 句柄先检查可信 owner/DACL，再允许注册恢复、启动、停止或删除。拒绝普通用户的修改权限、未知 ACE、null DACL、越界配置及非固定路径；只有原生 1060 允许新建。固定保护根目录仍在时允许缺失 helper 叶子进入已有维护，根目录缺失则拒绝。原生参数解析行为先失败再修复，新增 **11 项通过**，完整 Windows service **209 通过、0 失败**；all-targets/all-features check、默认与服务端完整 CI 严格 lint、fmt 和首次独立审查通过，审查计数 **1/2**。最新 Linux GNU x64/macOS Intel service 交叉 check 通过。原生 Windows 参数解析及 SDDL 测试不等于真实 SCM 安装验收。
- Windows 服务 release 二进制通过 `cargo build --release -p zenclash-service --features server --bin zenclash-service --locked` 构建，实际执行 `--version` 返回 `zenclash-service 0.1.2`、退出码 0。未执行 `run`、安装或卸载；Inno Setup 编译器不在当前环境，尚未生成真实安装包。三平台逐场景观察和通过判据见 [原生验收入口](tun-service-native-validation.md)。Windows GUI 卸载桥接还需共享账户策略及维护 worker 的实际 owner 准入，未接入安装包；普通权限预检不能代替提权后的原子校验。
- v3 开发前，维护锁仅互斥维护 worker；服务端 `Acquire` 与业务请求只检查维护 journal，没有检查 `.maintenance.lock`。安装/修复在获锁后先暂存，再发布 journal；卸载不发布 journal。该基线存在原子维护准入竞争窗口；本次 v3 核心接线已补充共享 owner 锁及原生宿主证明，阶段证据见本页开头，不能以 UAC 前 health/占用观察或后台 lease 代替。
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

首页统一服务入口的代码阶段已收尾，真实窗口仍待验收。用户当前要求先开发 GUI 修复/卸载与恢复、本地 TUN 配置准入、缓存与身份持久化、旧服务迁移与内核升级，其他工作暂缓；Local 配置已确认进入授权及服务交接后应用。原子维护 v3 核心接线已通过阶段验证，完整旧服务迁移仍需收尾，见 [主计划 §7.3](tun-service-plan.md#73-维护原子准入2026-10-03-已确认开发中)。完整缓存读取前置已有本轮证据；本地恢复资源物化、用户缓存保存和共享服务卸载仍需具体接线与迁移影响记录，没有把提案记为已实施。

1. 按已确认的自动授权及服务交接行为补 Managed Local 配置 TUN 准入和回归。UI 完整 CI 附加 lint 的五项问题与可见实机验收暂后置；首页阶段完整 UI 库 299 项通过，其后的详情导航尾修 Home 5 项、相关 service_tun 7 项通过，binary 11 项为此前启动阶段结果。Manager/UI 收据分类批次与 main/UI 启动批次分别结束两轮审查，底层维护分类和回读也各已结束两轮审查，不重复开启同批审查。
2. 按用户确认维持无 owner 启动失败后退出，不开发恢复窗口；双语错误提示已实现，继续验证三平台真实服务优先启动、拒绝状态和退出。main/UI P1/P2 修复、Windows/Linux 完整 core 回归、Linux check/完整 CI lint 和 Linux 真实普通生命周期复验已通过；macOS core 仍缺 C 工具链。上述结果不替代真实高权限服务或 TUN 验收。
3. 完成修复/卸载的单一捕获事务：冻结最新 accepted 配置与资源，确认服务 Stop/Release，恢复真实 Local，再授权维护；取消、未知结果、保存收据和 shutdown 分别验证。本地双槽物化及 GeoData 写入范围已确认且有阶段实现，完整原生串联仍需验收；普通 Local 备份恢复路径缺口见本次红测。
4. 将底层 provider 回读接到 Stop 后、Release 前的高层工作流；以普通权限安全保存，验证完整长度和摘要，覆盖后续启动、失败恢复和调用方取消。
5. 完成跨会话稳定 home、FakeIP/节点身份、相对 TLS/SSH 映射、动态运行目录预算、旧 setuid 迁移及内核保护副本升级；新增持久化结构前说明迁移影响。
6. 收尾 Windows/macOS 维护与包卸载协调、三平台安装文档及 README 双语说明；在真实服务管理器、管理员授权、Mihomo/TUN 环境完成异常退出、升级、卸载和 release 性能验收。

本阶段没有安装系统服务、修改主机 TUN 或部署系统目录文件。macOS bootstrap 的系统工具、FIFO/符号链接拒绝和授权行为仍需 macOS 实机验收。性能和 UI 响应性尚未测量，不作资源占用或交互性能结论。
