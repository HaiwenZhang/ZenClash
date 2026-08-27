# Mihomo 客户端 M0 行为基线

> 记录日期：2026-08-27
>
> 代码基线：`7415b25f4a3e70af9a50f690dd56bfc403cd808e`
>
> 环境：macOS Darwin 25.5.0 arm64，Rust/Cargo 1.94.0

## 自动验证

| 命令 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | 通过 |
| `cargo check --workspace --all-targets --all-features --locked` | 通过 |
| `cargo test --workspace --all-features --locked` | 通过；324 个测试通过，4 个需要真实内核或在线资源的测试按约定忽略 |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | 通过 |

真实 Mihomo 与 meow-rs harness 未在基线阶段运行，因为当前环境没有设置
`ZENCLASH_MIHOMO_BINARY` 或 `ZENCLASH_MEOW_BINARY`。它们不能由 mock 结果替代，必须在
对应里程碑验收时提供真实可执行文件后单独记录。

## P0 行为测试映射

44 项逐条可执行证据与仍待真实 OS 主机完成的平台项目记录在
[`mihomo-client-behavior-acceptance.md`](mihomo-client-behavior-acceptance.md)。

| 里程碑 | 行为测试 | 主要测试位置 |
|---|---|---|
| M1 代理组正确性 | 20–28 | `crates/zenclash-core/src/proxy.rs`、`proxy_operations.rs`、`tests/real_mihomo.rs`；代理页与首页行为测试 |
| M2 配置统一事务 | 7–12 | `crates/zenclash-core/src/profiles`、`controlled_config`、待新增 `profile_application`；`tests/real_mihomo.rs` |
| M3 四层运行状态 | 13–19、32 | 待新增 `operational_status`；现有 process、traffic、logs、system proxy 测试 |
| M4 流量接管事务 | 14–18 | `crates/zenclash-core/src/system_proxy`、`tun_permissions`、待新增 `traffic_capture_session` |
| M5 首页与首次使用 | 35–40 | `crates/zenclash-ui/src/pages/runtime/home.rs`、Sidebar、Profile 与连接行为测试 |

平台接管的 macOS、Windows、Linux 手工矩阵沿用开发计划 6.3 节；M0 只固定测试范围，
未把未执行的平台验证标记为通过。

## 首个代码切片

M1 从代理组领域模型和连接策略开始：解析 `hidden`、`fixed` 与组类型，并通过
`ConnectionPolicy::KeepExisting` 固定普通代理切换不删除现有连接的默认行为。
