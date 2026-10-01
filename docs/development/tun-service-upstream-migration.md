# TUN 服务上游代码移植计划

- 修订日期：2026-10-01。
- 对应 [三平台开发计划](tun-service-plan.md)；当前进度与验证见 [实施记录](tun-service-progress.md)。
- 开发方式：直接移植有用的上游实现及对应行为测试，再按 ZenClash 当前实现适配。以下是待实施清单，尚未完成移植。

## 1. 来源与复用单位

本地源码位于 [examples/clash-verge-service-ipc](../../examples/clash-verge-service-ipc/)，[Cargo.toml](../../examples/clash-verge-service-ipc/Cargo.toml) 声明包版本 `2.7.5`、edition `2024`、许可 `GPL-3.0`。本地快照对应的上游提交或发布 tag 尚未核实；开始复制时记录可确认的来源，不能把清单版本当作提交证明。本项目清单声明 `GPL-3.0-only`；移植保留上游版权、许可文本和来源记录，不在本次文档修改中改变项目许可。

以能够独立验证的模块或函数为单位复制到 `crates/zenclash-service`，同步移植适用的行为测试。复制前比对现有实现：已有同等能力时复用现有模块；有缺口时优先使用上游可用代码，避免再实现一套同责模块。替换后的冗余在该批修改中清理。`examples` 保留参考源码用途，不作为正式产物运行路径。

## 2. 优先移植清单

下表全部为待移植或待比对事项；具体目标函数在开始该批修改时确定。

| 上游入口 | 可复用内容 | ZenClash 接入与必要适配 |
| --- | --- | --- |
| [service.rs](../../examples/clash-verge-service-ipc/src/bin/service.rs)、[install_service.rs](../../examples/clash-verge-service-ipc/src/bin/install_service.rs)、[uninstall_service.rs](../../examples/clash-verge-service-ipc/src/bin/uninstall_service.rs) | 三平台服务注册、状态/停止回调、安装卸载；Windows `configure_windows_service_recovery` 和删除完成等待 | 映射到现有 `platform`、`installer` 和服务入口；保留固定批准路径、安装事务、包管理文件归属与失败恢复。SCM 恢复的是服务待命进程，不自动恢复失去所有者的内核 |
| [management.rs](../../examples/clash-verge-service-ipc/src/management.rs)、[channel.rs](../../examples/clash-verge-service-ipc/src/channel.rs) | 三平台授权与身份常量的组织方式 | 对照现有授权入口和平台路径集中定义，移植缺失部分；替换所有 Clash 名称、IPC、应用标识和内核名，保留实际连接身份验证及用户取消语义 |
| [installer/selinux.rs](../../examples/clash-verge-service-ipc/src/bin/installer/selinux.rs) | `ensure_executable_label`：检测 SELinux 并对执行目录标记，标记失败单独传播 | 接到 Linux 受保护副本暂存/安装流程，限定 ZenClash 执行目录；在 enforcing/permissive/未启用及命令失败场景验证，不关闭 SELinux |
| [runtime_generation/staging.rs](../../examples/clash-verge-service-ipc/src/core/runtime_generation/staging.rs) | `plan_stage`、`declared_remote_providers`：差异计划、URL 变化使缓存失效、未知文件保留规则 | 输入改为普通权限准备并上传后已持有的资源信息；保持 immutable 资源快照、内容身份和预算。时间戳仅作优化，不能授权 root 读取用户源路径 |
| [runtime_generation/assets.rs](../../examples/clash-verge-service-ipc/src/core/runtime_generation/assets.rs)、[staging.rs](../../examples/clash-verge-service-ipc/src/core/runtime_generation/staging.rs) | 路径/Windows 别名规则、`runtime_cleanup_retry_delay` 和 `while_the_core_lets_go` 等有界重试 | 合并到现有资产路径、保护目录、原子替换和显式清理所有者；保留 active/candidate 共用资源引用及有界退休槽，不删除仍被使用的资源 |
| [runtime_generation/readback.rs](../../examples/clash-verge-service-ipc/src/core/runtime_generation/readback.rs) | `read_runtime_file`、`read_chunk`：manifest 声明范围内的 provider 缓存分块回读 | 接现有 ServiceClient 会话、revision/generation、帧及总字节预算；复用当前固定句柄、无链接/重解析点校验，拒绝配置秘密和任意系统文件回读 |
| [assets.rs](../../examples/clash-verge-service-ipc/src/core/runtime_generation/assets.rs)、[runtime.rs](../../examples/clash-verge-service-ipc/src/core/runtime.rs)、[process.rs](../../examples/clash-verge-service-ipc/src/core/process.rs) | 稳定所有者运行目录、准备/落盘分离、残留进程记录及进程出生身份核验 | 接当前会话所有者、稳定 home 与退出回收；配置密钥、上传快照和内核可写状态分开。Windows 保留现有真实句柄和 Job 回收，Unix 保留原生身份及停止确认 |

上游资源目录代码可以帮助保留已生成的缓存和节点状态，但本地源码中尚未找到旧 Tailscale/ZeroTier 身份导入实现。旧身份授权导入、HTTP provider 更新中相对 TLS/SSH 文件映射，以及服务可写目录的动态预算仍需单独实现和验证，不能因移植稳定目录或缓存回读而勾选完成。

## 3. 保留的边界与行为差异

- `CoreSession` 继续拥有配置业务提交、后端切换、网络恢复和退出策略；GPUI、捕获协调及现有用户数据存储继续沿用。[core 接入文档](tun-service-core-integration.md) 是适配契约。
- 保留已验证连接上的身份校验、会话凭证、generation、请求序号、私密内核控制器和受限 API。上游协议 DTO 经适配接到当前接口；不将其整套协议和依赖清单直接替换进工作区。
- 上游 `stage_runtime` 会修改活动资源，不能原样替代本项目 prepare/apply/业务保存/commit 的事务。差异规划可以直接移植，资源落盘必须保持上一 accepted 配置和资源可恢复，见 [部分配置事务](tun-service-partial-config.md) 与 [资源布局](tun-service-resource-layout.md)。
- 上游 `desired::restore_desired_state` 的自动恢复策略需调整：系统启动或服务重启后先恢复待命及清理残留，内核启动要求有效应用会话；正常退出后内核必须停止。配置持久化失败不能仅记录告警后报告提交成功。
- 不带入针对 Clash Verge 的 legacy repair/cleanup、测试执行文件放行或服务入口失败后的无授权备用运行行为。所有安装/卸载只操作明确属于 ZenClash 的文件和注册。
- 不整体引入 `kode-bridge`、`windows-service`、上游 Git 依赖或新持久化结构。优先适配已有依赖；确有必要时先说明现依赖不足、维护与体积成本、迁移影响并确认。服务私有协议或元数据变化需记录兼容和修复路径。

## 4. 实施顺序与验收

1. **收尾当前差异**：先补齐当前 partial PATCH 测试模块和调用点，修复实际 owner 的生命周期及 UI 来源问题，取得相关编译与行为测试结果，再开始新的移植批次。前期绿结果不覆盖当前工作树。
2. **形成移植记录**：逐批列出源文件/函数、版本及可确认提交、目标模块、保留的许可、适配差异和预期行为。根据当前缺口选择最小完整批次，不为凑复用比例复制无关代码。
3. **资源基础复用**：优先复制纯差异计划及其行为测试、路径规则和有界重试，验证 URL 改变、重复目的地、Windows 别名、共享资源不误删和重试耗尽。
4. **平台补缺**：复制并适配 SCM 恢复/删除确认、SELinux 标记及三平台安装维护中的缺失部分；保留现有鉴权、原子安装和失败恢复。真实安装、授权及停止在目标系统验收。
5. **资源与生命周期接入**：增加声明范围内缓存回读及跨 revision 状态保留，接 core 配置事务和应用级服务操作；身份首次导入、动态资源和部分修改分别覆盖成功、取消、响应丢失及恢复路径。
6. **完整验收**：按主计划完成 Windows、macOS、Linux 的真实服务、Mihomo、TUN、退出、升级和卸载矩阵；再更新完成标记。

每批先运行最近的行为测试和 `cargo check`；Rust 改动超过三十行按项目规约做独立审查，同一修改最多两轮。适用的 fmt、test、严格 clippy 与 workspace 检查沿用 [主计划验证命令](tun-service-plan.md)。源码复制、上游 mock 测试、交叉检查和打包替身通过均不能替代 ZenClash 真实内核或三平台实机验收。
