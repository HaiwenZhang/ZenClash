# 三平台 TUN 服务原生验收

本文是 [开发计划](tun-service-plan.md) P2–P6 的实机测试入口。执行步骤和通过判据供目标系统验收使用；当前实际结果见 [实施记录](tun-service-progress.md)，本页不记录为已经通过。测试使用独立账户、测试订阅与可恢复的系统环境。

## 环境与证据

Linux/macOS 本轮新增维护边界的真机检查：

- Linux：固定 `/etc/systemd/system/zenclash-service.service` 与包拥有的 `/usr/lib/systemd/system/zenclash-service.service` 分别测试外来内容、符号链接、不可读及维护中被替换；拒绝后保留原文件及私有部署。应用卸载只清理 `/etc` override，包文件由包管理器移除。
- Linux：在独立可恢复环境保留已加载服务、移走磁盘 unit 后尝试修复/卸载。不能将磁盘缺失当作已停止；未知或仍加载的观察结果必须保留二进制、运行目录和恢复材料。空部署且管理器明确报告未加载/未运行/无 PID 时，首次安装仍可继续。系统实际加载的执行归属校验另行验收，磁盘模板不代替该项。
- macOS：预先禁用固定标签，再从已批准的完整 plist 开启，核对 `enable` 后 `bootstrap` 与 `RunAtLoad` 实际启动。缺失/替换 plist、授权或 Enable 失败不得继续 bootstrap；Loaded 路径维持 kickstart。记录实际 launchd disabled 状态和进程，不用测试回调证明真实系统结果。

日志流另按以下专项验证，不能用其结果代替 TUN：在已批准的 Service 会话中分别订阅 info/structured 与 warning/plain，产生 info 和 warning 事件，核对实际请求、字段形状及 warning 不收到 info；关闭后重订阅，核对原生 Mihomo PID。取消、旧 generation、旧 v2 helper 拒绝和超过 128 KiB 的帧应按回归预期失败，不能自动恢复 Local。Windows 管道忙重试与握手共用五秒期限，其他权限或 I/O 错误立即返回。

普通用户 Windows 内核管道专项可在独立临时配置下执行：设置 `ZENCLASH_MIHOMO_BINARY` 为已核验的固定正式 Mihomo 绝对路径，然后运行 `cargo test -p zenclash-service --lib --all-features --locked real_mihomo_pipe_logs_preserve_options_filter_warning_and_cancel_cleanly -- --ignored --nocapture`。该测试关闭 TUN/DNS，仅使用回环目标并清理自建子进程；它不启动受保护服务宿主，也不验证管理员安装。实际结果见 [实施记录](tun-service-progress.md)。Linux/macOS 日志与 GUI 的实机验证仍需各平台单独执行。

Unix 新增两个真实 Mihomo socket 专项，默认忽略，当前验证状态另见实施记录。均设置 `ZENCLASH_MIHOMO_BINARY` 为本平台已核验的 Mihomo 绝对路径，关闭 TUN/DNS，使用独立临时目录及回环流量：

- 普通用户执行 `cargo test -p zenclash-service --lib --all-features --locked real_nonroot_mihomo_socket_is_rejected_before_log_subscription -- --ignored --nocapture`，核对真实非 root socket 对端被生产身份校验拒绝，HTTP 在发送前失败、日志不能订阅，子进程完成回收。
- UID 0 测试环境执行 `cargo test -p zenclash-service --lib --all-features --locked real_root_mihomo_socket_logs_preserve_options_filter_warning_and_cancel_cleanly -- --ignored --nocapture`，核对真实日志等级、格式、取消及重订阅。测试不会自行提权；普通用户运行此正向用例必须返回明确权限错误，不能记录为通过。该专项仍不证明受保护服务安装或 TUN。

恢复后的普通 Local 升级专项：设置 `ZENCLASH_MIHOMO_BINARY` 为已核验的本平台普通 Mihomo，继承环境中将 `SAFE_PATHS` 清为无有效路径、`SKIP_SAFE_PATH_CHECK` 设为 false，运行 `cargo test -p zenclash-core --lib --all-features --locked core_update::tests::real_recovery_tests -- --ignored --test-threads=1`。两项默认忽略测试先删除自建 provider 源，以 TUN-off 双槽配置启动真实内核，再执行候选预检、替换、启动及版本回读；版本拒绝分支确认旧内核再次启动并能读取 held provider。候选复制自同一个正式二进制，错误 tag 仅用于触发版本拒绝，因此不证明不同版本兼容性、下载校验或受保护 Service 副本升级。测试自建临时目录、关闭 DNS、仅用回环控制器，RAII 停止测试自己的子进程后清理目录。

普通用户真实恢复资源专项：设置 `ZENCLASH_MIHOMO_BINARY` 为已核验的固定正式 Mihomo 绝对路径，运行 `cargo test -p zenclash-core --test real_local_recovery --all-features --locked -- --ignored --test-threads=1`。测试复制内核到独立临时 home，准备后删除测试自己的配置/provider 源，物化 TUN-off 双槽，核对真实内核选择 held provider、普通重启更换 PID 和轮换槽热重载保持 PID 且读取新 provider；无效代理组与 home/双槽之外的 provider 必须在预检阶段被拒绝，并确认无活动子进程。测试不启动高权限 Service、不写系统代理/路由，不代表真实 GeoData/TLS、修复/卸载系统授权或完整 GUI 验收。成功/失败及 panic 都由夹具回收自己的子进程后清理临时目录。默认忽略，未提供正式内核时不自动下载。实际结果见实施记录。

§7.3 的 v3 原子维护核心接线已通过阶段验证，完整迁移与原生维护仍待验收：真机需覆盖普通用户安装名单下提权 worker 的 Hello、owner 与维护的两种竞争顺序、Stop 后 owner 未 Release、退出清理失败、旧 v2 拒绝自动升级，以及维护过程中启动新 helper。维护持排他锁时新 helper 应可完成原生身份与 Hello 验证，新 Acquire 必须返回 MaintenancePending；业务目录恢复延迟到首次成功取得共享锁之后。旧服务受控迁移需先结束旧应用、管理员核实固定注册归属并停止/注销宿主、确认实际退出，随后显式 Repair；未知归属不得删除恢复材料。

普通用户原生跨进程锁回归：`cargo test -p zenclash-service --all-features --locked maintenance_process_exit_releases_native_lock -- --nocapture`。测试由当前测试程序启动真实子进程，覆盖共享/排他冲突及强制终止后的原锁文件恢复；只对退出后短暂的 WouldBlock 限时重试，不替代管理员路径保护、服务注册或真实 TUN 验收。Windows 与普通 WSL Linux 的结果见实施记录，macOS 需本机运行。

macOS 活动宿主关联还需单独验收新固定只读 helper 参数 `--query-host-pid`：只接受 root 权限、system domain 与固定标签，输出唯一正整数 PID 和 LF，stderr 或未知结果均拒绝。记录 SDK、系统版本及 Apple 框架实际字段类型，核对 PID 与同一次原生连接的出生身份、UID、执行路径一致。查询子进程设置五秒期限，失败停止并回收自身查询进程；缺失/错误类型/浮点/零值/溢出不允许 Stop 或安装提交。交叉 check 不验证系统框架链接或真实字段，完整 inactive job 与已加载参数归属仍待开发与验收。

每次验收记录仓库提交或工作区差异、系统版本与架构、普通用户及管理员身份、安装包来源及摘要、GUI/helper/Mihomo 版本。应用使用 release 构建；debug、WSL 普通子进程测试和交叉 check 的结果单独记录。Windows 与 macOS 授权必须在可见桌面执行，Linux 同时记录 systemd、Polkit 授权代理及 SELinux 状态。

每个场景保存操作时间、期望与实际结果、GUI/服务/Mihomo PID、服务注册状态、TUN 网卡及路由的前后差异，以及失败后的恢复结果。记录采用测试数据；不附完整订阅、控制器密钥或配置内容。服务“运行”与 TUN“生效”分别判定。

以下为只读观察命令。服务尚未安装时的“不存在”输出属于基线，不能解释为功能失败；访问拒绝或观察失败则记录为未知。

| 平台 | 固定服务身份 | 管理员保护目录 |
| --- | --- | --- |
| Windows | `ZenClashService`，own-process、LocalSystem | 系统 ProgramData 下的 `ZenClashService` |
| macOS | `system/org.zenclash.service` | `/Library/PrivilegedHelperTools/org.zenclash.service` |
| Linux | `zenclash-service.service` | `/var/lib/zenclash-service` |

Windows PowerShell：

```powershell
Get-CimInstance Win32_Service -Filter "Name='ZenClashService'" |
    Select-Object Name,State,ProcessId,StartName,PathName
Get-Process zenclash,zenclash-service,mihomo -ErrorAction SilentlyContinue |
    Select-Object Id,ProcessName,Path
Get-NetAdapter | Select-Object Name,InterfaceDescription,Status,ifIndex
Get-NetRoute | Select-Object InterfaceIndex,DestinationPrefix,NextHop,RouteMetric
```

macOS：

```sh
launchctl print system/org.zenclash.service
ps -axo pid,ppid,uid,comm
ifconfig
netstat -rn
scutil --dns
```

Linux：

```sh
systemctl show zenclash-service.service -p ActiveState -p SubState -p MainPID -p User -p ExecStart -p ControlGroup
ps -eo pid,ppid,uid,comm
ip -brief link
ip route show table all
ip -6 route show table all
ip rule show
ip -6 rule show
```

Linux 的 DNS 观察使用目标发行版实际采用的解析器；使用 systemd-resolved 时另记录 `resolvectl status`。不得仅凭配置中的 DNS 或 TUN 字段判定系统实际生效。

## 行为矩阵

从未安装服务、普通用户 GUI、测试配置关闭 TUN 的基线开始。每个场景结束后检查当前实际 owner、内核与捕获事实，再进入下一项。

| 场景与操作 | 通过判据 |
| --- | --- |
| 首次开启 TUN，确认安装并完成系统授权 | 只请求一次授权；固定受保护副本及注册有效；仅一个受管 Mihomo；TUN/DNS/路由回读及测试流量均符合请求 |
| 首次授权取消、授权失败或服务未就绪 | 准确反馈；不显示 TUN 已开；此前捕获方式与有效配置保留；再次显式操作可以重试 |
| 关闭后再开启 TUN | 关闭后网络恢复；健康且内核摘要已批准时再次开启不重复提权；真实网卡及路由变化与 UI 一致 |
| 保存 TUN 开启后正常退出，再普通权限启动 | 旧内核同步停止；新启动直接使用健康服务，没有先启动本地内核；同一个业务 owner 负责配置和退出 |
| 服务缺失、占用、不兼容或配置层损坏时启动 | 不按默认值启动本地 TUN、不依赖旧 setuid；占用不 Stop/Release 他人；错误及恢复入口可见 |
| 快速开关、安装中切页、窗口隐藏到托盘 | 无并行安装、双内核或旧意图覆盖；页面切换/隐藏不丢失已提交工作；隐藏不等于结束应用会话 |
| 配置拒绝、Start/Commit 响应丢失、保存失败 | 已保存收据准确；未知结果通过同 owner 回读确认；未确认前不重复 Start、不启动第二个内核；可重试恢复 |
| 从首页开启，维护结果未知或配置已保存但 Commit 待确认 | 首页显示准确的待确认提示，不能仅禁用开关或显示已开启；键盘可访问详情并到达既有确认入口。切页再返回仍显示共享事实；重复点击不产生第二次维护或丢失保存收据 |
| Commit 待确认时网络暂停、休眠恢复 | 暂停前确认 Finalizing；确认失败保持捕获、运行和停止意图；手动 Stop 及 shutdown 抑制自动拉起 |
| 正常菜单退出、托盘退出及应用重启 | 等待捕获释放与真实 child 结束；退出期间禁止迟到任务拉起；只在收尾成功后执行重启 |
| 强制结束 GUI、强制结束服务、系统重启 | 有界回收旧会话及内核；TUN/路由无遗留；服务可恢复待命，不能自行恢复无 owner 的代理 |
| 修复、卸载及取消授权 | 完成同一捕获事务的 Stop/Release 与普通内核恢复；取消和未知结果遵循计划，用户配置保持；此高层流程尚待实现 |
| 内核副本升级、服务升级及中途失败 | 新副本经授权及摘要校验；失败恢复上一批准产物与配置；未知维护记录保留并准确反馈，不报告升级成功 |
| Unix 旧 setuid 内核迁移 | 只处理已证明属于 ZenClash 的内核；停止状态下迁移；失败不静默重授 setuid；无属主证明时保留待处理项 |

网络验收覆盖测试配置声明的 IPv4、IPv6、DNS，以及系统代理与 TUN 互切。关闭或退出后与基线比较网卡、路由、Linux 策略路由规则和测试流量，不能用 Mihomo 进程退出代替网络恢复证据。

## 安装包与权限边界

使用 [打包记录](tun-service-packaging.md) 的正式产物进行全新安装、覆盖升级和最终卸载；受控打包工具替身不计为发布包验收。Linux 分别执行支持发行版的 DEB/RPM 路径，验证升级不注销已批准服务、最终卸载失败时包文件保留，并在 SELinux enforcing 环境验收。Windows 保持 GUI 普通用户安装，macOS 按发布包实际签名方式验证 helper、完整 bundle 及授权结果。

Windows GUI 卸载协调和 macOS 维护入口仍待接入；Windows 多账户共享服务卸载策略待确认。验收需另外覆盖同名外部服务、注册执行路径/账户异常、源文件替换、符号链接/重解析点、未授权用户与并发 owner，证明拒绝发生在原生修改之前。维护及卸载只能清理固定私有产物。

## 结果与完成条件

每个平台单独给出通过、失败、未执行和无法观察的场景，不合并推断其他系统。错误分支须记录是否保留安装文件、业务保存结果、运行 owner 与恢复入口。缺少环境或界面接线的项保持未执行。

只有上述行为、计划中的权限/资源边界及打包升级卸载均取得目标平台证据后，才更新主计划阶段标记。资源占用与响应性另按 [主计划验证要求](tun-service-plan.md#9-验证矩阵与证据) 使用 release 测量，记录窗口尺寸、数据规模、采样时段和统计口径，分别报告 GUI、服务与 Mihomo；本页的进程和网卡观察不构成性能结论。
