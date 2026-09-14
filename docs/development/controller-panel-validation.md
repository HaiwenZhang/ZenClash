# 本地运行与菜单栏面板验收

## 实现边界

- 控制器管理和 Wi-Fi 自动切换功能已移除，包括远程页面、独立存储读写、网络监听和 macOS 定位权限声明。
- 本地 Mihomo 控制 API、配置管理和内核生命周期继续由现有模块负责。
- 已有 `controllers.json` 和 `ssid-rules.json` 不再读取，也不会主动删除；其他持久化格式无迁移。
- 菜单栏及悬浮窗显示、控制本地内核。

## 自动验证

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

回归测试检查 macOS 应用不再声明定位权限；保留状态采集取消、托盘命令及面板布局测试。网络测试需要允许监听本机端口。这些测试不替代真实窗口和内核交互验证。

## 原生手工验证（尚待验收）

1. macOS/Windows/Linux 启动应用，确认主窗口不再显示控制器入口和 Wi-Fi 规则，侧栏可正常切换本地页面；隐藏再显示窗口后代理页正常刷新。
2. macOS 切换 Wi-Fi，确认不会自动切换本地配置或请求定位权限。已有规则文件保持原样。
3. 从主窗口及托盘手动切换配置；使用无效配置验证错误反馈，当前可用配置应保留。
4. macOS/Windows 左键打开面板，右键打开菜单；Linux 从原生菜单的「打开状态面板」进入。测试模式、配置、节点选择及失败反馈；Esc 先关闭节点下拉，再关闭面板，点击外部关闭。测试重复点击、隐藏主窗口后打开、主窗口唤起和应用退出。
5. 多显示器（含左侧负坐标及不同缩放）、底部/侧边任务栏、窄屏、浅色/深色、中英文、Tab/Enter 键盘操作，检查面板边界及控件可达性。
6. 在配置切换未完成时退出，确认没有重新打开窗口或重启本地内核。真实网络及系统代理/TUN 恢复另按 [自动运行维护验证](automatic-runtime-validation.md) 执行。
