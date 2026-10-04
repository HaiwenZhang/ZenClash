# Windows 实机验收记录（2026-10-04）

本轮验收仍在进行，以下只记录已经取得的证据。TUN 按用户最新要求排除；没有关闭 Clash Verge 的 TUN。

## 环境与证据边界

- Windows 11 Pro，版本 10.0.26200，x64；源码基线 `9f3e31b26d2a32d88069359d4c8cc819b1140db2`。
- UI 使用当前源码的 release 构建，Mihomo v1.19.30；窗口截图尺寸 1283 × 821。
- 测试数据、配置和日志保存在被 Git 忽略的 `target/windows-acceptance`。订阅地址、密钥及配置内容不进入本记录。
- 真实订阅约 482.5 KiB，加载后为 64 个代理对象、12 个策略组、9,816 条规则。代理对象数量包含策略组及内置对象，不等同于节点数量。
- 现有 Windows 服务的普通用户健康探测返回访问拒绝。GUI 实机使用项目支持的外部控制器模式和隔离的 TUN-off 测试内核，不能据此证明服务托管启动、服务退出或 TUN 工作正常。
- 沙箱与真实桌面的进程、网络及 HKCU 视图不同。实机网络和系统代理证据取自真实用户会话，沙箱失败不直接归类为产品故障。

## 已验证

| 范围 | 结果与限制 |
| --- | --- |
| 真实订阅下载、添加、更新并应用 | 成功；源文件没有原地改写 |
| 节点页、名称筛选、切到规则页 | 成功；规则页显示 9,816 条规则 |
| 输入框聚焦、折叠表单后切页 | 修复后 release 实机通过；原版本点击侧栏不切页 |
| 主题、中英文与键盘 | 主题同步修复后 release 实机通过；Light/Dark 选中状态同步，Tab 焦点可见，Enter 可切换主题，重启读取已保存的主题和语言 |
| 真实节点批量测速 | 主选择组 47 个节点返回延迟结果；随后切换日志页成功 |
| 日志与网络诊断 | 日志流显示已连接；DIRECT 与 Mihomo 路径各 3/3 个延迟探测成功；测试配置关闭 DNS，A/AAAA 查询正确显示不可用 |
| 系统代理冲突保护 | 已有 WinINET 代理为其他应用的 127.0.0.1:7890；ZenClash 操作被拒绝，原设置保持不变。成功接管、退出恢复未验证 |
| 真实 Mihomo 集成测试 | 通过；覆盖实际控制器、配置应用、规则、监听冲突、流量/日志、重启和停止 |
| 真实普通内核资源恢复 | 3 项通过：保留资源启动、无效配置不启动子进程、拒绝目录外 provider |
| 配置变化与自动生命周期 | 2 项通过：源变化、无效变化保留当前状态、网络暂停/恢复及手动停止语义 |
| 真实升级/回滚资源恢复 | 2 项通过 |
| 普通内核备份恢复 | 失败：删除原源文件后，受管恢复目录的 provider 被 Mihomo 安全路径拒绝；当前进程和配置仍保留 |

## 修复与回归

1. 订阅/页面输入框移出渲染树后恢复有效页面焦点。延迟恢复检查页面代次和展示状态，避免旧回调覆盖新页面。
2. 模拟控制器测试改用动态端口，避免本机代理占用固定端口使预检失败、测试服务器等待永不完成。
3. 主题偏好变更同步到运行页面的内存偏好副本，修复主题已改变但设置卡片仍勾选旧选项的问题；release 实机复核通过。

最终工作区测试通过 1,314 项（不含默认忽略的真实内核测试），格式检查、全目标全特性 `cargo check` 和严格 Clippy 通过。release 构建通过。焦点修复完成两次审查，独立主题同步修复完成一次审查，没有确认的功能性 bug 或重大漏洞。

复现表单焦点路径：打开订阅页，展开在线订阅表单，聚焦名称输入框，折叠表单，再点击设置与代理组。预期点击后页面切换，随后键盘 Tab 仍能移动到可见控件。

自动化入口：

```powershell
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
$env:ZENCLASH_MIHOMO_BINARY = 'C:\path\to\mihomo.exe'
cargo test -p zenclash-core --test real_mihomo --all-features --locked -- --ignored --nocapture
cargo test -p zenclash-core --lib --all-features --locked ordinary_local_backup_recovers_held_provider_after_original_sources_are_deleted -- --ignored --nocapture
```

## 尚未形成结论

没有进行帧耗时统计或长期内存趋势采样。截图、短期进程内存采样或构建成功不能证明 UI 响应性满足产品要求。实验 meow 后端、管理员服务维护、TUN、安装包和跨平台验收不在已通过范围内。

## 大集合补测

使用相同 Windows、release 构建和窗口尺寸，独立控制器端口 19091、代理端口 17982，TUN 与 DNS 关闭。配置包含 5,000 个合成 HTTP 节点、一个显式选择组、49,999 条 DomainSuffix 规则和一条 MATCH 规则，共 2,850,197 字节。合成节点使用本机未提供代理服务的地址，仅测试数据量与 UI 行为，不用于网络可用性结论。

- 真实 Mihomo 加载成功，UI 显示 5,000 个节点、5,008 个代理对象和 50,000 条规则。
- 节点过滤 `04999` 显示唯一末尾节点，选择后 UI 与真实控制器 API 都返回 `stress-node-04999`。
- 清空过滤后显示 556 页，每页最多 9 个节点；下一页显示第 10–18 个节点。
- 规则过滤 `49998` 显示唯一末尾 DomainSuffix 规则；输入框聚焦后切到设置、再返回代理页成功，未观察到无法切页或窗口无响应。
- 每秒采样一次、每个进程共 20 次：UI 工作集 104.74–105.17 MiB，Private Bytes 146.38–146.88 MiB；内核工作集 153.84 MiB，Private Bytes 182.03 MiB。采样位于加载后设置页附近，属于短期进程统计，不区分 Rust 堆与图形资源，不证明没有长期泄漏。

可用 `./scripts/diagnostics/prepare_windows_stress.ps1` 重建无凭据配置。它只生成 `target/windows-acceptance/stress/profile.yaml`，不会启动内核或修改系统代理。实机运行前确认端口空闲，以 Mihomo 的 `-d` 指定隔离目录、`-f` 指定生成文件，再设置 `ZENCLASH_CONTROLLER=127.0.0.1:19091`、`ZENCLASH_CONFIG` 为该文件、`LOCALAPPDATA` 为隔离目录启动 release GUI。结束后正常退出 GUI，并清理自己启动的外部内核。
