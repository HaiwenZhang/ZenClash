# `crates/zenclash-ui` 代码与测试组织重新设计方案

- **文档日期**：2026-11-22
- **适用范围**：`crates/zenclash-ui`（源码物理分层、状态所有权演进、白盒单元测试、GPUI 无头 UI 集成测试、统一测试夹具与 CI 静态守护）
- **关联文档**：
  - [architecture-review-2026-11-21.md](architecture-review-2026-11-21.md)
  - [software-design-v2-2026-11-21.md](software-design-v2-2026-11-21.md)
  - [zenclash-core-layout-redesign-2026-11-22.md](zenclash-core-layout-redesign-2026-11-22.md)
- **规范基准**：全仓统一 Rust 2024 无 `mod.rs` 风格（`<name>.rs` + `<name>/`）与 GPUI Kit Coding Guides（按能力边界垂直组织、依赖严格向下、纯逻辑单测与无头窗口 UI 集成测试分层）。

---

## 一、现状诊断（基于代码盘点的 6 大结构痛点）

当前 `crates/zenclash-ui/src/` 共 **214 个 `.rs` 文件**（生产文件 155 个、测试文件 59 个，总计约 6.3 万行，含 **508 个测试**：`#[test]` 231、`#[tokio::test]` 49、`#[gpui_kit::test]` 228），核心混乱来自以下 6 个方面：

1. **`pages/runtime/` 成为“巨型上帝宿主”（God-Host Sprawl）**
   - `pages/` 独占全 crate **85% 代码量**（163 个文件 / 54,001 行），其中 `pages/runtime/` 顶层直接平铺 **45 个项**，且有多达 **50 个生产文件分散编写 `impl RuntimePage`**。
   - **页面模型严重不对称**：全应用 15 个页面中，只有 `ProxiesPage` 是顶层 `pages/proxies/` 下的独立 `Entity<ProxiesPage>`；`ConnectionsView`、`LogsView`、`RulesView` 虽已是独立 `Entity` 却被埋在 `pages/runtime/` 下再经 `page_integration.rs` 桥接；其余 11 个页面直接写成 `impl RuntimePage` 方法，与宿主共享 30+ 字段。
   - 跨页基础设施（`busy.rs`、`common.rs`、`config_inputs.rs`、`feedback.rs`、`lifecycle.rs`、`page_observation.rs`、`page_task.rs`、`view.rs`）与业务页面（`home`、`profiles`、`network`、`settings`、`tun` 等）全部平铺在同一层，无法区分“框架壳”与“具体页面”。

2. **顶层散落胶水模块、同名双托盘目录与空目录残留**
   - `src/` 顶层裸露 3 个 25–183 行的内部胶水模块（`preference_previews.rs` 33L、`runtime_read_source.rs` 25L、`proxy_selection.rs` 183L），以及仅服务于 `main.rs` 的 `startup_failure.rs`（277L）。
   - 托盘代码分裂在两个同名目录：`src/tray/`（5 个文件，纯 OS 托盘图标/菜单驱动）与 `src/app/tray/`（11 个文件，托盘命令/刷新/观测协调），极易混淆职责。
   - 残留废弃空目录 `src/app/controllers/wifi/`。

3. **巨型单文件与内联测试严重混编**
   - Top 10 生产文件普遍在 **800–2,483 行**，其中 `pages/runtime/home.rs` 高达 **2,483 行**、`lifecycle.rs` **1,545 行**、`config_inputs.rs` **1,275 行**、`proxies/view.rs` **1,263 行**、`network.rs` **1,185 行**、`connections/dashboard.rs` **1,176 行**、`rules/dashboard.rs` **1,095 行**、`profiles/view/catalog.rs` **1,055 行**。
   - 共有 **21 个生产文件**内联了超过 80 行的 `#[cfg(test)] mod tests { ... }`（合计超过 **4,200 行**，部分文件内联测试占比达 50%–60%）。

4. **缺失 `tests/` 目录，508 个测试全部压在 `--lib` 单一测试目标内**
   - `crates/zenclash-ui/tests/` 目前**完全不存在**。
   - 228 个重型 `#[gpui_kit::test]` 无头窗口 UI 集成测试（分布在 61 个文件、超过 1.5 万行代码）与 280 个轻量单元测试全部编译链接进 `zenclash-ui --lib`，导致修改任意一行颜色常量或纯函数都会触发全量无头窗口测试重编。
   - 测试文件命名极其杂乱，同时存在 6 种风格：`tests.rs`、`ui_tests.rs`、`host_ui_tests.rs`、`observation_tests.rs`、`keyboard_tests.rs`、`page_integration/tests.rs`。

5. **测试夹具（Test Support）分散在 5 处且向生产类型渗透**
   - `app/test_support.rs`（31L）、`app/tray/test_support.rs`（158L）、`pages/proxies/test_support.rs`（316L）、`pages/runtime/test_support/`（4 文件 683L）、`pages/runtime/resources/test_support.rs`（131L）各自为政，合计 **1,319 行**。
   - 生产状态中存在测试侵入字段（如 `HomeUiState::design_validation`），导致难以编写纯粹的外部黑盒 UI 集成测试。

6. **CI 静态守护尚未覆盖 `zenclash-ui` 完整分层与 Feature 水平隔离**
   - 现有 `architecture_guards.rs` 仅守护了 `components`/`actions` 不依赖 `pages`/`app` 以及顶层无双向环，尚未约束 `zenclash-ui` 完整 5 层 DAG、Feature 间水平隔离以及禁止空目录残留。

---

## 二、目标物理目录架构（`src/` 严格 5 层单向目录树）

对齐 `zenclash-core` 的 5 层严格单向 DAG 设计与 GPUI Kit Coding Guides（*Organize large applications by capability*），将 `crates/zenclash-ui/src/` 收敛为 **5 个物理业务层 + 1 个统一测试夹具层 + 2 个 crate 入口文件**，全仓严格遵循 **Rust 2024 无 `mod.rs` 风格**（`<name>.rs` + `<name>/`）。

### 2.1 顶层视图（16+ 散落项 → 8 个清晰顶层入口）

```text
crates/zenclash-ui/
├── Cargo.toml
├── build.rs
├── build_version.rs
├── src/
│   ├── main.rs                           # 极薄二进制入口（Tracing、SafeMode、Tokio + Bootstrap + GPUI run）
│   ├── lib.rs                            # 极薄 crate 根：5 层物理声明 + 兼容重导出
│   ├── foundation.rs + foundation/       # Layer 0：主题令牌、内嵌资产、全局 Action、路由与共享状态原语（0 内部层依赖）
│   ├── components.rs + components/       # Layer 1：纯可复用 GPUI 组件与通用页面构建原语（仅依赖 foundation）
│   ├── runtime_host.rs + runtime_host/   # Layer 2：页面运行时宿主基础设施（仅依赖 foundation, components）
│   ├── features.rs + features/           # Layer 3：按业务能力垂直切分的独立页面 Feature（仅依赖 L0–L2；Feature 间水平隔离！）
│   ├── shell.rs + shell/                 # Layer 4：应用外壳 ZenClashApp、窗口生命周期、后台协调器与原生托盘（仅依赖 L0–L3）
│   └── test_support.rs + test_support/   # 统一 UI 测试夹具（#[cfg(any(test, feature = "test-support"))]）
└── tests/                                # 黑盒 UI 集成测试（按依赖深度聚合为 3 个独立测试二进制）
    ├── ui_components/                    # 【Tier 1】默认特征即可跑：纯组件与主题无头交互测试（毫秒级，零 Core/Tokio）
    ├── ui_features/                      # 【Tier 2】feature = "test-support"：单页面 Feature 无头 UI 集成测试
    └── ui_shell/                         # 【Tier 3】feature = "test-support"：整壳跨页端到端流测试（App + Tray + Service TUN）
```

### 2.2 五层内部完整映射表（已落地结构）

```text
src/
├── main.rs                               # 极薄二进制入口（调用 shell::platform::startup_failure）
├── lib.rs                                # 声明 5 层 + test_support；保留兼容导出（actions/assets/design -> foundation；app/tray -> shell）
│
├── foundation.rs + foundation/           # Layer 0（仅依赖 gpui-kit / zenclash-core / zenclash-app / zenclash-i18n）
│   ├── design.rs + design/tests.rs       # ZenClash 语义色板、Surface 令牌、WCAG AA 对比度与主题切换
│   ├── assets.rs                         # 内嵌 SVG 与品牌图标资源
│   ├── actions.rs                        # 全局 GPUI actions!、Page 枚举与导航元数据
│   └── primitives.rs + primitives/       # 收拢原顶层 3 个胶水文件
│       ├── preference_previews.rs        # 菜单偏好未决预览值对象
│       ├── runtime_read_source.rs        # CoreSession 读观测身份锚定
│       └── proxy_selection.rs + proxy_selection/tests.rs # Home 与 Proxies 共享的代理选择状态机
│
├── components.rs + components/           # Layer 1（仅依赖 foundation；严禁依赖 runtime_host / features / shell）
│   ├── surface.rs                        # 统一表面原语（Card / Section / Banner / Inset）
│   ├── sidebar.rs + sidebar/tests.rs     # 全局可折叠导航侧栏
│   ├── feedback.rs                       # FeedbackNotifications（通知去重与语义等级）
│   ├── mint_switch.rs                    # 主题切换开关控件
│   ├── wave_percentage.rs + wave_percentage/tests.rs # 订阅用量波浪百分比指示器
│   ├── workspace.rs                      # 通用页面工作区骨架
│   ├── page_primitives.rs + page_primitives/zoom_tests.rs # 抽离自原 common.rs（setting_card / info_row / metric / empty_state / format_*）
│   └── form_primitives.rs + form_primitives/{choice.rs, tests.rs} # 通用表单原语与选项控件（抽离自原 settings/forms.rs）
│
├── runtime_host.rs + runtime_host/       # Layer 2（仅依赖 foundation, components）
│   ├── services.rs                       # RuntimePageServices + RuntimeMonitors + ShellPresentationState 契约定义
│   ├── busy.rs                           # 变更互斥与忙碌状态机
│   ├── task_token.rs                     # 页面异步任务代际令牌
│   ├── lifecycle_schedule.rs             # 页级轮询与刷新调度节律
│   ├── config_inputs.rs + config_inputs/ # Wave 2 拆分（1,275L → codec / core / fields / read_identity / read_state / sections + tests）
│   └── observation.rs + observation/     # 统一页级快照拉取、generation 防陈旧与取消
│       ├── observation_time.rs
│       ├── read.rs
│       ├── snapshot_data.rs
│       ├── tests.rs
│       └── readers.rs + readers/{connections,home,mihomo,network,profiles,resources,rules,settings,system_proxy,tun}.rs
│
├── features.rs + features/               # Layer 3（仅依赖 L0–L2；兄弟 Feature 严禁互相引用内部实现！）
│   ├── host.rs + host/                   # 页面路由宿主 RuntimePage（仅 host 允许聚合各兄弟 Feature）
│   │   ├── view.rs / feedback.rs / form_helpers.rs / config_inputs.rs
│   │   ├── lifecycle.rs + lifecycle/{config_apply.rs, live_loop.rs, page_loading.rs, tests.rs, ui_tests.rs} # Wave 2 拆分 1,545L
│   │   └── ui_tests.rs + ui_tests/{service_tun.rs, service_maintenance.rs, tray_receipts.rs, design_validation.rs}
│   ├── home.rs + home/                   # 仪表盘首页（Wave 2 拆分 2,483L → cards / controls / dashboard / menu_size / proxy_selection / selector / status / traffic / transitions / validation）
│   ├── proxies.rs + proxies/             # 独立 Entity<ProxiesPage>（Wave 2 拆分 1,263L view.rs → view.rs + view/{inspector, proxy_card}.rs；含 actions / catalog_runtime / measurement / node_grid / presentation / selection）
│   ├── profiles.rs + profiles/           # 配置库（Wave 2 拆分 1,055L view/catalog.rs → catalog.rs + catalog/{card, presentation}.rs；含 actions / library / state / workflow / yaml_editor / view/{editor, forms}.rs）
│   ├── overrides.rs + overrides/         # YAML 覆写（diff_view / model / store_actions）
│   ├── connections.rs + connections/     # 独立 Entity<ConnectionsView>（Wave 2 拆分 1,176L dashboard.rs → dashboard.rs + dashboard/{detail, summary}.rs；含 page_integration / projection / timeline）
│   ├── rules.rs + rules/                 # 独立 Entity<RulesView>（Wave 2 拆分 1,095L dashboard.rs → dashboard.rs + dashboard/{details, table}.rs；含 page_integration / projection）
│   ├── traffic.rs + traffic/             # 流量分析（actions / model / query_presentation / view）
│   ├── logs.rs + logs/                   # 独立 Entity<LogsView>（dashboard / dialogs / export / page_integration / projection）
│   ├── network.rs + network/             # 网络诊断（Wave 2 拆分 1,185L network.rs → network.rs + {actions, ai, diagnostic_query, dialogs, latency_view, model, probes_view}.rs）
│   ├── resources.rs + resources/         # 规则集、GeoData 与 Provider 操作（action_context / builtin_actions / provider_actions / ruleset / state）
│   ├── capture.rs + capture/             # 归口系统代理与 TUN 两大捕获路径
│   │   ├── system_proxy.rs + system_proxy/{actions.rs, view.rs, ui_tests.rs}
│   │   └── tun.rs + tun/{maintenance.rs, service.rs}
│   ├── kernel.rs + kernel/               # 归口内核运行态、DNS、嗅探器配置
│   │   ├── mihomo.rs + mihomo/{maintenance.rs, release_list.rs, ui_tests.rs}
│   │   ├── dns.rs
│   │   └── sniffer.rs
│   └── settings.rs + settings/           # 偏好设置、内核管理、备份恢复、应用更新与法务（app_update / backup / core_management / legal / preferences）
│
├── shell.rs + shell/                     # Layer 4（仅依赖 L0–L3；应用外壳组装层，严禁写具体页面业务逻辑）
│   ├── app.rs                            # ZenClashApp 根实体：仅组合窗口、Sidebar、Feature 与 Tray
│   ├── bootstrap.rs                      # init / create_main_window_* / reopen_main_window
│   ├── view.rs + view/tests.rs           # 主窗口 Chrome、标题栏、Sidebar 与内容区装配
│   ├── window.rs + window/tests.rs       # 窗口尺寸、隐藏托盘、关闭拦截
│   ├── actions.rs                        # 全局快捷键与 Action 分发
│   ├── navigation.rs                     # 页面切换协调
│   ├── controllers.rs + controllers/     # 应用级后台协调器（已彻底删除废弃空目录 wifi/）
│   │   ├── automatic.rs
│   │   ├── outbound_mode.rs
│   │   ├── preferences.rs
│   │   ├── menu_preferences.rs + menu_preferences/tests.rs
│   │   ├── profile_updates.rs
│   │   └── system_proxy.rs + system_proxy/tests.rs
│   ├── tray.rs + tray/                   # 统一收拢原顶层 src/tray/ 与 src/app/tray/
│   │   ├── native.rs + native/{icon.rs, menu.rs, command_bindings, macos.rs, tests.rs} # 纯 OS 托盘驱动
│   │   ├── controller.rs / queue.rs
│   │   ├── commands.rs / proxy_commands / directories
│   │   └── refresh / runtime_observation
│   └── platform.rs + platform/           # 平台目录操作 + startup_failure.rs + startup_failure/tests.rs
│
└── test_support.rs + test_support/       # 统一测试夹具（#[cfg(any(test, feature = "test-support"))]）
    ├── controller.rs                     # Mock Mihomo Controller
    ├── owned_cores.rs                    # 受管内核生命周期夹具
    ├── runtime_host.rs                   # RuntimePage 测试宿主夹具
    ├── service_workflow.rs               # 提权服务工作流夹具
    ├── proxies.rs                        # ProxiesPage 目录与测速夹具
    ├── resources.rs                      # Resources/GeoData 操作夹具
    └── shell_tray.rs                     # Shell 与 Tray 集成夹具
```

### 2.3 渐进式消除 `RuntimePage` 上帝对象（两阶段策略）

直接一次性重写 50 个 `impl RuntimePage` 文件风险过高；采用安全可控的两阶段演进：
1. **阶段 A（目录归口与大文件拆分）**：保持 `RuntimePage` 作为 `features/host.rs` 中的兼容宿主，先将所有页面物理迁移至 `features/<feature>/`，把 `home.rs`（2,483L）、`lifecycle.rs`（1,545L）、`config_inputs.rs`（1,275L）、`proxies/view.rs`（1,263L）、`network.rs`（1,185L）、`connections/dashboard.rs`（1,176L）、`rules/dashboard.rs`（1,095L）、`profiles/view/catalog.rs`（1,055L）拆分为 300–550 行职责单一的子模块，并用强类型 `RuntimeHostContext` 封装宿主共享服务。
2. **阶段 B（独立 `Entity<FeatureView>` 化）**：参照已成熟的 `ProxiesPage`、`ConnectionsView`、`LogsView`、`RulesView` 模式，优先将最重的 4 个页面逐个提升为独立 GPUI `Entity`：
   - `HomeView`（独立订阅 `TrafficMonitor` 与 `OperationalStatus`，避免首页动画重绘波及全局 `RuntimePage`）
   - `ProfilesView`（独立持有 `ProfileLibrary` 与表单 `InputState`，通过 `EventEmitter<ProfileEvent>` 向外壳发事件）
   - `NetworkView`（独立持有探测/诊断/AI 网络状态与对话框）
   - `SettingsView`（独立持有受控配置表单、核心管理、备份与偏好状态）
   - 最终 `RuntimePage` 退化为纯薄路由容器（仅持有共享服务句柄 + 各独立 `Entity` 页面）。

---

## 三、单元测试组织规范（White-Box Unit Tests）

严格遵循 GPUI Kit Testing Guide 的分层原则：**“纯逻辑用普通 `#[test]` / `#[tokio::test]`，需要实体/无头窗口交互的归入 UI 集成测试”**。

### 3.1 统一拆分与归类规则
1. **体量门槛**：
   - `<= 80` 行纯函数单元测试：允许保留在生产文件底部 `#[cfg(test)] mod tests { ... }`。
   - `> 80` 行单元测试：**强制拆离为独立同级文件** `tests.rs`（生产文件末尾仅保留 `#[cfg(test)] mod tests;`），严禁使用 `mod.rs`。
   - 单模块测试超过 `~500` 行时，按主题进一步拆为 `<module>/tests.rs` + `<module>/tests/<topic>.rs`。
2. **命名统一**：
   - 彻底消除当前 6 种混乱命名（`tests.rs`、`ui_tests.rs`、`host_ui_tests.rs`、`observation_tests.rs`、`keyboard_tests.rs`、`page_integration/tests.rs`）：
     - `src/**` 内只允许存在**白盒单元测试**，统一命名为 `tests.rs` 或 `tests/<topic>.rs`（如 `tests/keyboard.rs`、`tests/observation.rs`、`tests/projection.rs`）。
     - 所有基于 `#[gpui_kit::test]` + `TestWindowExt` 驱动真实视图与点击/键入的 **UI 集成测试**，统一迁出至 `crates/zenclash-ui/tests/`！
3. **测试边界红线**：
   - `src/**` 白盒单元测试原则上只使用 `#[test]` / `#[tokio::test]`（以及极少数必须访问私有状态的纯 `TestAppContext` 状态机单元测试，**不创建完整无头窗口**）。
   - 禁止在单元测试模块中使用 `use gpui_kit::*;`（避免 GPUI 的 `test` 宏遮蔽标准库 `#[test]`，遵循 `gpui-kit/references/gpui/test.md`）。

### 3.2 Wave 2 超大生产文件拆分与内联测试剥离落地清单

| 原生产文件 | 拆分前行数 | 内联测试行数（占比） | 已落地拆离与子模块拆分目标 |
| --- | ---: | ---: | --- |
| `pages/runtime/home.rs` | 2,483 | 648 行（26%） | `features/home/tests.rs` + 拆分为 `home.rs`、`cards.rs`、`controls.rs`、`dashboard.rs`、`status.rs`、`transitions.rs`、`validation.rs` |
| `pages/runtime/home/selector.rs` | 911 | 300 行（32%） | `features/home/selector/tests.rs` |
| `app/view.rs` | 795 | 299 行（37%） | `shell/view/tests.rs` |
| `pages/runtime/config_inputs.rs` | 1,275 | 245 行（19%） | `runtime_host/config_inputs/tests.rs` + 拆分为 `codec.rs`、`core.rs`、`fields.rs`、`read_identity.rs`、`read_state.rs`、`sections.rs` |
| `pages/runtime/rules/projection.rs` | 378 | 227 行（60%） | `features/rules/projection/tests.rs` |
| `pages/runtime/connections.rs` | 870 | 201 行（23%） | `features/connections/tests.rs`（同步拆分 `connections/dashboard.rs` → `dashboard/{detail, summary}.rs`） |
| `components/sidebar.rs` | 537 | 198 行（36%） | 逻辑单测 -> `components/sidebar/tests.rs`；无头交互 -> `tests/ui_components/` |
| `pages/runtime/home/traffic.rs` | 362 | 174 行（48%） | `features/home/traffic/tests.rs` |
| `pages/proxies/actions.rs` | 633 | 169 行（26%） | `features/proxies/actions/tests.rs`（同步拆分 `proxies/view.rs` → `view/{inspector, proxy_card}.rs`） |
| `pages/runtime/network/model.rs` | 330 | 162 行（49%） | `features/network/model/tests.rs`（同步拆分 `network.rs` → `{actions, dialogs, latency_view, probes_view}.rs`） |
| `app/system_proxy.rs` | 302 | 159 行（52%） | `shell/controllers/system_proxy/tests.rs` |
| `startup_failure.rs` | 277 | 150 行（54%） | `shell/platform/startup_failure/tests.rs` |
| `design.rs` | 424 | 147 行（34%） | `foundation/design/tests.rs` |
| `pages/runtime/lifecycle.rs` | 1,545 | 136 行（8%） | `features/host/lifecycle/tests.rs` + 拆分为 `lifecycle/{config_apply, live_loop, page_loading}.rs`（调度节律归口至 `runtime_host/lifecycle_schedule.rs`） |
| `pages/runtime/connections/timeline.rs` | 252 | 133 行（52%） | `features/connections/timeline/tests.rs` |
| `pages/runtime/profiles/state.rs` | 484 | 131 行（27%） | `features/profiles/state/tests.rs`（同步拆分 `profiles/view/catalog.rs` → `catalog/{card, presentation}.rs`） |

> 同步完成其余 5 个 >80 行内联测试剥离与超大视图拆分：`features/traffic/view/tests.rs`（120L）、`features/connections/projection/tests.rs`（106L）、`components/wave_percentage/tests.rs`（103L）、`components/form_primitives/tests.rs`（原 `settings/forms.rs` 91L）、`shell/tray/native/tests.rs`（原 `tray.rs` 87L），以及 `features/rules/dashboard.rs` → `dashboard/{details, table}.rs`，全 crate 生产文件内联测试均控制在 <= 80 行。

---

## 四、UI 集成测试重新设计（`crates/zenclash-ui/tests/`）

当前 228 个 `#[gpui_kit::test]` 全部混在 `--lib` 内部导致编译慢、边界不清。重新设计为 **3 个职责分明的聚合集成测试二进制**，与 GPUI Kit 官方测试工作流完全对齐：

### 4.1 三档集成测试金字塔

```text
crates/zenclash-ui/tests/
├── ui_components/                         # 【Tier 1】默认特性即跑（零 Core/Tokio，秒级验证视觉控件与键盘可访问性）
│   ├── main.rs
│   ├── theme_and_surface.rs               # Light/Dark 主题切换、Surface 层次、剪贴板复制与 WCAG AA 对比度
│   ├── sidebar_navigation.rs              # Sidebar 折叠展开、导航 Action 派发、模式切换按钮
│   └── controls.rs                        # MintSwitch / WavePercentage / FeedbackNotifications / Workspace 无头交互
│
├── ui_features/                           # 【Tier 2】required-features = ["test-support"]（单页面黑盒无头窗口流）
│   ├── main.rs
│   ├── home_and_proxies.rs                # RuntimePage Home 仪表盘与 ProxiesPage 节点筛选/排序流
│   ├── profiles_and_overrides.rs          # Profiles 目录展示与 Override 预览/删除端到端流
│   ├── connections_and_rules.rs           # Connections 过滤/暂停与 Rules 规则检视流
│   ├── capture_and_kernel.rs              # System Proxy / TUN / Mihomo / DNS / Sniffer 受控配置流
│   └── settings_and_network.rs            # Settings / Network / Traffic / Logs / Resources 交互流
│
└── ui_shell/                              # 【Tier 3】required-features = ["test-support"]（整壳跨页与托盘端到端流）
    ├── main.rs
    ├── app_navigation_and_window.rs       # ZenClashApp 整壳侧栏导航、折叠、主题切换、Safe Mode 与窗口重开
    └── tray_and_preferences.rs            # TrayMenuState / TrayCommandBindings / PreferencePreviews / OutboundMode 协调流
```

> **白盒边界保留说明**：原 `src/` 内需要访问 `RuntimePage` / `ZenClashApp` 私有字段或竞态注入钩子的 505 个既有白盒测试完整保留在 `src/` 对应 `tests.rs` / `ui_tests.rs` 中，避免为外迁测试破坏生产类型封装（遵循 GPUI Kit *No `pub` fields across the seam*）；外部 `tests/ui_{components,features,shell}/` 全部通过公开 API 与 `zenclash_ui::test_support` 门面做纯黑盒验证。

### 4.2 `Cargo.toml` 特性门控与显式目标声明（关键 Cargo 细节）

> **注意 Cargo 编译机制**：外部 `tests/*.rs` 集成测试编译被测 crate 时**不会**开启 `#[cfg(test)]`。因此若 `tests/ui_features` 与 `tests/ui_shell` 需要复用 `zenclash_ui::test_support` 夹具，必须在 `crates/zenclash-ui/Cargo.toml` 中像 `zenclash-core` / `zenclash-app` 一样显式声明 `test-support` feature（生产构建绝不开启）：

```toml
[features]
# 仅用于 UI 集成测试与隔离夹具；生产打包严禁启用。
test-support = [
    "gpui-kit/test-support",
    "zenclash-app/test-support",
    "zenclash-core/test-support",
]

[[test]]
name = "ui_components"
path = "tests/ui_components/main.rs"

[[test]]
name = "ui_features"
path = "tests/ui_features/main.rs"
required-features = ["test-support"]

[[test]]
name = "ui_shell"
path = "tests/ui_shell/main.rs"
required-features = ["test-support"]
```

**收益**：
- **开发期精准快跑**：
  - 改组件/主题：`cargo test -p zenclash-ui --test ui_components`（秒级，零 Tokio/Core）
  - 改纯状态逻辑：`cargo test -p zenclash-ui --lib`（仅剩快速单元测试，不再链接 1.5 万行无头窗口测试）
  - 改单页交互：`cargo test -p zenclash-ui --features test-support --test ui_features -- profiles`
  - 改整壳/托盘/提权流：`cargo test -p zenclash-ui --features test-support --test ui_shell`
- **全仓 CI 无缝衔接**：现有 `cargo test --workspace --all-features --locked` 自动覆盖全部 3 档 UI 集成测试 + `--lib` 单元测试，零遗漏。
- **消除测试代码侵入生产结构体**：通过 `test_support` 提供的公开受控构造函数/观测句柄驱动真实视图，逐步清理生产结构中的 `#[cfg(test)]` 字段。

---

## 五、CI 静态分层守护升级（防结构再次腐化）

在 [crates/zenclash-app/tests/architecture_guards.rs](file:///Users/haiwen/gitrepo/ZenClash/crates/zenclash-app/tests/architecture_guards.rs) 中，在已有全仓无 `mod.rs` 门禁与 `zenclash-core` 五层 DAG 门禁基础上，已落地 4 项 `zenclash-ui` 专属纯 `std` 静态门禁（加配套单元测试，整套 `architecture_guards` 共 **18 项测试**）：

```mermaid
flowchart TD
    L4["shell/ (Layer 4: ZenClashApp / Window / Tray / Controllers)"] --> L3["features/ (Layer 3: 独立垂直页面 Feature)"]
    L4 --> L2["runtime_host/ (Layer 2: Services / Observation / ConfigInputs / Schedule)"]
    L4 --> L1["components/ (Layer 1: 可复用 UI / Page / Form 原语)"]
    L4 --> L0["foundation/ (Layer 0: Design / Assets / Actions / Primitives)"]
    L3 --> L2
    L3 --> L1
    L3 --> L0
    L2 --> L1
    L2 --> L0
    L1 --> L0
```

1. **`zenclash_ui_top_level_layout_is_strictly_five_layers`**
   - 校验 `crates/zenclash-ui/src/` 顶层仅允许存在 `lib.rs`、`main.rs`、`foundation(.rs)`、`components(.rs)`、`runtime_host(.rs)`、`features(.rs)`、`shell(.rs)`、`test_support(.rs)`，且 5 个业务层均同时具备 `<layer>.rs` 与 `<layer>/`（禁止再往顶层平铺胶水文件）。
2. **`zenclash_ui_enforces_strict_five_layer_dag`**
   - 校验生产代码严格遵循 `foundation[0] < components[1] < runtime_host[2] < features[3] < shell[4]`：
     - 同时拦截显式层路径（`crate::<target>::...`、`use crate::{..., <target>::...}`、跨层 `super::...`）与 `crates/zenclash-ui/src/lib.rs` 根重导出快捷方式（`actions`/`assets`/`design` → `foundation`、`app`/`tray` → `shell`）。
     - 同步断言严格无环层 `foundation`、`components`、`runtime_host` 内部不存在任何直接双向子模块环（`shell` 因 `ZenClashApp` 根结构分拆 `impl` 允许 `app` 与其协调子模块双向引用）。
3. **`zenclash_ui_features_are_horizontally_isolated`**
   - 扫描 `crates/zenclash-ui/src/features/` 下全部兄弟 Feature（`capture`、`connections`、`home`、`kernel`、`logs`、`network`、`overrides`、`profiles`、`proxies`、`resources`、`rules`、`settings`、`traffic`，排除聚合路由宿主 `host`），断言任意兄弟 Feature `A` 生产代码**严禁通过 `crate::features::<B>` 或跨回 `features/` 的 `super::...::<B>` 引用另一兄弟 Feature `B`**；跨 Feature 联动必须经由 `foundation::actions`、`runtime_host`、`features/host` 或 `shell` 编排。
   - 配套白盒单测 `horizontal_feature_detector_catches_cross_feature_refs_and_ignores_host_and_cfg_test` 回归验证跨 Feature 检测同时放行 `#[cfg(test)]` 与 `features/host/*`。
4. **`zenclash_ui_has_zero_empty_source_directories`**
   - 递归遍历 `crates/zenclash-ui/src/` 断言不存在任何空目录（防止类似 `src/app/controllers/wifi/` 的死目录残留）。

---

## 六、零破坏实施路线（分 4 波次多 Agent 执行）

1. **Wave 1：物理 5 层目录迁移 + 清理空目录与双托盘分裂（零业务逻辑改动）**
   - 删除空目录 `src/app/controllers/wifi/`；通过 `git mv` 将现有模块迁入 `foundation/`、`components/`、`runtime_host/`、`features/`、`shell/` 与统一 `test_support/`；统一收拢 `tray/` 与 `app/tray/` 到 `shell/tray/`；`lib.rs` 保持对外公开 API 兼容。
   - **验收**：全仓 `fmt` / 11 条严格 `clippy` / `cargo test --workspace --all-features --locked` / `cargo doc -D warnings` 全绿，测试总数精确持平（508）。

2. **Wave 2：超大生产文件拆分 + 剥离全部 21 个文件内联单元测试**
   - 并行拆离 21 个生产文件（>80 行内联测试）至同级 `tests.rs`，并同步将 `home.rs`（2,483L）、`lifecycle.rs`（1,545L）、`config_inputs.rs`（1,275L）、`proxies/view.rs`（1,263L）、`network.rs`（1,185L）、`connections/dashboard.rs`（1,176L）、`rules/dashboard.rs`（1,095L）、`profiles/view/catalog.rs`（1,055L）拆分为 300–550 行高内聚子模块。
   - **验收**：全仓 `fmt` / 严格 `clippy` / 全量测试数精确持平。

3. **Wave 3：落地 `tests/` 三档 UI 集成测试 + 收口 `test_support/`**
   - 在 `crates/zenclash-ui/Cargo.toml` 新增 `test-support` 特性与 3 个 `[[test]]` 目标（`ui_components`、`ui_features`、`ui_shell`）。
   - 将散落在 `src/` 的重型无头窗口 UI 集成测试（`*_ui_tests.rs`、`pages/runtime/ui_tests/*`、`components` 交互测试等）迁出至 `tests/ui_{components,features,shell}/`，并补齐 Tier 1 `ui_components` 零 Core 快测套件。
   - 升级 `crates/zenclash-app/tests/architecture_guards.rs` 新增 4 项 `zenclash-ui` 静态架构门禁。
   - **验收**：默认特性 `cargo test -p zenclash-ui --test ui_components` 秒级全过，`--all-features` 全仓 5 项 CI 门禁全绿。

4. **Wave 4（可选增量演进）：核心重型页面独立 `Entity` 化**
   - 在架构和 CI 门禁稳定后，按 `HomeView` → `ProfilesView` → `NetworkView` → `SettingsView` 顺序渐进将 `impl RuntimePage` 拆分为独立 GPUI `Entity`，最终使 `RuntimePage` 退化为纯薄路由容器。
