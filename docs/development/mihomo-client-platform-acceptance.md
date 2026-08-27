# Mihomo 客户端真实平台验收协议

> 更新日期：2026-08-27
>
> 对应门槛：[`mihomo-client-development-plan.md` 第 6.3 节](mihomo-client-development-plan.md#63-平台门槛)
>
> 行为映射：[`mihomo-client-behavior-acceptance.md`](mihomo-client-behavior-acceptance.md)

本文给出无法由 adapter 或 mock 代替的真实操作系统验收步骤。每次发布候选都应在目标系统
上按同一顺序执行，并保存去敏后的记录。涉及系统代理、TUN 或管理员授权的步骤必须由测试者
在隔离环境中显式确认；自动化不得代替用户接受系统权限提示。

## 1. 记录与安全边界

每次执行至少记录以下信息：

- OS 名称、版本、架构和桌面环境；
- ZenClash 版本、构建提交、包名及签名/包校验结果；
- Mihomo `-v` 输出中的版本与架构，不记录 controller secret；
- 操作前、操作后和应用退出后的原生 readback；
- UI 显示的 L1–L4 状态、失败恢复文案和关键进程 PID；
- Pass、Fail 或 Blocked，以及失败时的去敏日志。

使用专用测试用户和可恢复的网络环境。开始前关闭其他 VPN/代理客户端，并让 System Proxy
处于关闭状态；如果必须保留现有代理，先导出原生状态，测试结束后由测试者手工恢复。不要把
订阅 URL、Authorization、controller secret、公网 IP 或完整用户目录写入验收记录。

测试 Profile 必须提供一个可工作的 Mixed 或 HTTP 监听器。路径探测使用项目内置动作，记录
“经 Mihomo”与 DIRECT 的路由标签和成功/失败即可，不保存公网响应正文。

## 2. 通用场景

### A. System Proxy 启用、回读和释放

1. 保存原生基线，确认 HTTP、HTTPS 和 PAC 均未启用。
2. 启动发布包，等待首页显示 L1 内核与 L2 Controller 就绪。
3. 从首页选择“使用系统代理”，或进入“设置 → 代理接管 → 系统代理”后启用。
4. 刷新页面并执行本平台的原生 readback。HTTP 与 HTTPS 必须同时指向 ZenClash 当前
   Mixed/HTTP 端口，PAC 模式则必须只启用应用显示的本地 PAC URL。
5. UI 必须分别显示 intent、actual 和 ownership，L3 只有原生状态与所有权同时成立时才为
   Active。
6. 选择关闭接管。原生 HTTP、HTTPS 和 PAC 必须关闭，ownership 必须清除。
7. 再次启用 System Proxy 后正常退出应用。只有仍由 ZenClash 拥有的原生状态应被关闭，
   Mihomo 子进程必须同步退出。

### B. ownership 被外部覆盖

1. 按场景 A 启用并确认 ZenClash owns 当前 System Proxy。
2. 使用操作系统设置把代理改为测试专用的不同回环端点，例如 `127.0.0.2:9`；不要使用真实
   第三方地址。保持新值启用。
3. 在 ZenClash 刷新接管状态。UI 必须显示“系统代理已被其他应用覆盖”或等价 Lost 状态，
   不能继续显示 Owned。
4. 正常退出 ZenClash，再做原生 readback。外部回环端点必须保持不变，证明退出未覆盖第三方
   新值。
5. 测试者通过操作系统设置关闭该测试代理，并确认恢复到第 1 节记录的基线。

### C. TUN 权限拒绝、授权、重启和释放

1. 记录受管 Mihomo 的权限、当前 PID、TUN 配置、虚拟网卡与 `1.1.1.1` 路由。
2. 显式选择“使用 TUN”或“安装 / 修复 TUN 权限”，在系统授权窗口选择拒绝。
3. ZenClash 必须退出 pending，显示可恢复错误；TUN 配置、设备、路由和 System Proxy 均不得
   因拒绝而发生部分写入。
4. macOS/Linux 再次执行该动作并批准一次性授权。授权必须只作用于 canonical 受管 core，
   授权前后 SHA-256 必须一致；内核 PID 必须更换，并重新通过 Controller readiness。
5. 启用 TUN 后，页面必须分别显示 permission、device、route 与原生证据。只有三项都 Active
   时 L3 才能为 Active；缺设备名、设备或 route 时必须是 Unknown/Inactive，不能伪报成功。
6. 执行“运行路径探测”，确认 L4 的目标步骤明确标记为经 Mihomo。
7. 选择关闭接管并退出应用。TUN 配置、设备和相关路由必须释放，Mihomo 子进程必须退出；
   Unix core 的一次性权限可保留，直到 core 被更新或安装包替换。

Windows 当前没有具备调用方 ACL 的按需 helper。普通用户进程执行本场景时，预期结果是权限
动作明确显示 Unsupported、不会出现整 GUI 提权、不会写入 TUN 配置或创建网卡。只有安全
helper 实现后，Windows 才执行上述“授权成功并重启”分支；不得用“以管理员身份运行整个
ZenClash”冒充通过。

### D. core 崩溃、恢复和应用退出

1. 记录 ZenClash PID 和它的直属 Mihomo 子进程 PID。
2. 只终止该直属子进程，不按名称批量终止系统中的其他 Mihomo 进程。
3. 观察有界恢复：UI 必须显示 Recovering/Recovered、尝试次数与安全退出原因，新子进程 PID
   必须不同，Controller 重新就绪；若接管原先由 ZenClash 拥有，则恢复后重新 reconcile。
4. 正常退出应用，确认应用与新子进程均已退出，且场景 B 的外部代理不会被改写。

### E. 路径明确经 Mihomo

1. 分别在 Off、System Proxy 和受支持的 TUN 状态运行网络诊断/路径探测。
2. 报告必须把 Mihomo 与 DIRECT 标为不同 route；Mihomo 步骤使用当前 Mixed/HTTP listener，
   不能仅根据 traffic WebSocket 活跃推断接管成功。
3. 断开目标网络后重试。L4 必须显示 Failed/Stale，并保留最后成功时间；L1–L3 的成功分片
   不应被清空。

### F. 安装包升级

1. 准备同一渠道的相邻两个签名版本 `old` 与 `new`，验证包身份后安装 `old`。
2. 在 `old` 中创建不含真实凭据的测试 Profile，改变一项普通偏好并记录应用数据摘要；关闭
   接管后正常退出。
3. 使用平台标准覆盖升级方式安装 `new`，不得预先删除应用数据目录。
4. 启动 `new`，确认 Profile、偏好和 last-known-good 仍在，内核及 Controller 就绪，应用没有
   静默启用 System Proxy/TUN。
5. 确认新应用版本、bundled Mihomo 版本、桌面入口和卸载记录均来自 `new`。Unix 新 core 应
   重新核对 TUN 权限，不得沿用旧文件的所有权结论。
6. 正常退出并执行卸载；是否保留用户数据必须符合当前发布约定，并记录实际结果。

## 3. 平台原生 readback

下列命令只读取状态，外部覆盖与授权仍通过系统 UI/ZenClash 的显式动作执行。

### macOS

先用 `route -n get default` 和 `networksetup -listnetworkserviceorder` 确认活动接口对应的服务名，
并将它传给以下命令：

```bash
/usr/sbin/networksetup -getwebproxy "<service>"
/usr/sbin/networksetup -getsecurewebproxy "<service>"
/usr/sbin/networksetup -getautoproxyurl "<service>"
/usr/sbin/networksetup -getproxybypassdomains "<service>"
/sbin/ifconfig "<utun-device>"
/sbin/route -n get 1.1.1.1
ps -Ao pid=,ppid=,comm=
codesign --verify --deep --strict /Applications/ZenClash.app
```

升级方式：退出应用，挂载新 DMG 并用 Finder 覆盖 `/Applications/ZenClash.app`，再执行签名
验证和场景 F。记录测试前后的应用版本；未签名的本地开发包只能用于开发验收，不能替代发布
签名包。

### Windows

在非管理员 PowerShell 中执行：

```powershell
reg.exe query "HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings" /v ProxyEnable
reg.exe query "HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings" /v ProxyServer
reg.exe query "HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings" /v ProxyOverride
reg.exe query "HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings" /v AutoConfigURL
Get-NetAdapter -Name "<mihomo-device>" -IncludeHidden
Find-NetRoute -RemoteIPAddress "1.1.1.1"
Get-CimInstance Win32_Process | Where-Object { $_.Name -in @("zenclash.exe", "mihomo.exe") } |
  Select-Object ProcessId, ParentProcessId, Name
Get-AuthenticodeSignature "${env:LOCALAPPDATA}\Programs\ZenClash\zenclash.exe"
```

升级方式：关闭应用后运行 `ZenClash-<new>-windows-x64-setup.exe` 覆盖相同 AppId 的旧版本，
再执行场景 F。安装器声明 `PrivilegesRequired=lowest`；出现整 GUI 管理员要求即判 Fail。

### Linux（GNOME）

System Proxy 生产 adapter 以 GNOME `gsettings` 为边界；非 GNOME 桌面应明确报告 Unsupported，
不能以环境无效值伪装通过。

```bash
gsettings get org.gnome.system.proxy mode
gsettings get org.gnome.system.proxy.http host
gsettings get org.gnome.system.proxy.http port
gsettings get org.gnome.system.proxy.https host
gsettings get org.gnome.system.proxy.https port
gsettings get org.gnome.system.proxy ignore-hosts
gsettings get org.gnome.system.proxy autoconfig-url
ip link show dev "<mihomo-device>"
ip -4 route get 1.1.1.1
ps -eo pid=,ppid=,comm=
```

Debian/Ubuntu 使用 `dpkg -i ZenClash-<new>-Ubuntu-22.04+-<arch>.deb` 覆盖旧包；RPM 系使用
`rpm -Uvh ZenClash-<new>-linux-x86_64.rpm`。升级后用 `dpkg -s zenclash` 或 `rpm -q zenclash`
确认版本，再执行场景 F。授权依赖桌面 Polkit 的 `pkexec`，缺少代理或用户拒绝时必须安全失败。

## 4. 当前执行状态

| 场景 | macOS 真实实包 | Windows | Linux |
|---|---|---|---|
| System Proxy enable/readback/release | 待用户授权执行 | 待目标机 | 待目标机 |
| ownership 被外部覆盖 | 待用户授权执行 | 待目标机 | 待目标机 |
| TUN 权限拒绝/授权/重启 | 拒绝路径通过；授权路径待用户授权 | 待目标机；当前应验证安全 Unsupported | 待目标机 |
| core 崩溃与应用退出 | 通过：直属 core 被终止后以新 PID 恢复，UI 显示尝试次数和退出原因；退出同步回收 | 待目标机 | 待目标机 |
| 路径探测明确经 Mihomo | 真实 Mihomo harness 通过；系统接管组合待执行 | 待目标机 | 待目标机 |
| 安装包升级回归 | 待两个发布签名版本 | 待两个发布签名版本与目标机 | 待两个发布包与目标机 |

