# 三平台 TUN 服务原生验收

本文是 [开发计划](tun-service-plan.md) P2–P6 的实机测试入口。执行步骤和通过判据供目标系统验收使用；当前实际结果见 [实施记录](tun-service-progress.md)，本页不记录为已经通过。测试使用独立账户、测试订阅与可恢复的系统环境。

## 环境与证据

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
