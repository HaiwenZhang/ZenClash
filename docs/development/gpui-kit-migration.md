# GPUI Kit 迁移与 Windows 验收

ZenClash 的 UI 统一通过 `gpui-kit` 使用 GPUI、Component 和资源。版本由
`Cargo.lock` 固定；升级 Kit 时同时检查其配套底层库，避免应用与组件使用不同的 GPUI 类型。

| 层 | 当前依赖 |
| --- | --- |
| 应用入口 | `gpui-kit 0.7.0` |
| 组件与行为 | `gpui-component 0.7.0`、`gpui-base 0.7.0` |
| 图标资源 | `gpui-kit-assets 0.7.0` 与应用自有 SVG |
| 底层 GPUI 与桌面平台 | Kit 固定的 `gpui-pre 0.3.7` 系列 |

应用使用 `gpui_kit::application`、`gpui_kit::init` 和 `gpui_kit::open_window`。
Kit 为三个应用窗口建立 Root，管理窗口级弹层和通知。单行字段使用 `InputState` / `Input`，
多行配置和 PAC 文本使用 `TextareaState` / `Textarea`，YAML 使用 `EditorState` / `Editor`。
生产依赖启用 `tree-sitter-yaml`，`test-support` 仅在开发依赖中启用。

Mihomo 服务、持久化格式和发布流程沿用现有实现。此次迁移不提供内存改善结论。

## 自动验证

使用当前 Rust stable 工具链和对应平台构建工具，在仓库根目录运行：

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

配置输入的无窗口交互测试使用生产 `ConfigInputs`，通过 Kit 分发键盘和鼠标事件，
检查多行输入、后台刷新保留编辑和光标、配置切换及重置后恢复焦点。
YAML 编辑器测试检查生产编辑器状态的换行和撤销。已有纯逻辑测试覆盖提交回读与较晚编辑的关系。
这些测试与编译检查不能证明 Windows 的窗口系统、IME、托盘或图形显示正确。

`scripts/diagnostics` 下的两个 Rust 示例使用相同 Kit API，需按现有专项文档复制为
UI crate 的 example 后使用；编译验证不等于运行时性能验证。

2026-09-30 在 macOS 上使用 Rust 1.95.0 完成以下自动验证，未启动原生应用进行实机验收：

- workspace 的格式检查、全部 target / feature 编译检查通过。
- workspace 测试 527 项通过，6 项需要真实内核的测试按约定忽略；输入、焦点和 YAML 编辑器测试使用 GPUI 测试平台。
- Clippy 通过，包括 CI 中的并发、性能和文档附加规则。
- 两个诊断 Rust 示例在隔离源码中通过编译检查。

## Windows 实机检查

实机验证由 Windows 环境执行，以下项目在该平台确认后才算验收通过：

1. 更新工具链，设置现有 `ZENCLASH_MIHOMO_BINARY`，构建并启动 release 应用：

   ```powershell
   rustup update stable
   $env:ZENCLASH_MIHOMO_BINARY = 'C:\path\to\mihomo.exe'
   cargo build --release --locked -p zenclash-ui --bin zenclash
   .\target\release\zenclash.exe
   ```

2. 切换明暗主题、中英文及侧栏折叠；Tab、Shift+Tab、Enter 可到达并操作导航与按钮，焦点可见。
3. DNS、多行路由及 PAC 输入支持换行和中文 IME。后台刷新保留未提交内容；保存回读、切换配置和备份恢复分别符合现有行为。
4. YAML 编辑器支持中文、换行、选择、撤销和保存；非法 YAML 报错且保留编辑内容，取消后可重新打开。
5. 下拉菜单和弹层可用键盘操作，Escape 关闭后焦点合理恢复；托盘状态面板、主窗口与悬浮窗可以反复开关。
6. 通过 Ctrl+Q 和托盘退出确认受管内核停止；窗口隐藏、恢复、失焦和退出遵循现有产品行为。

记录 Windows 版本、GPU、显示缩放、构建版本和每项结果。若测量内存，单独记录 UI 进程，
注明页面、窗口尺寸、数据量、运行时长和测量工具，不将内核内存混入 UI 数值。
