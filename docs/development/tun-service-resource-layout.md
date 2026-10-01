# TUN 服务资源目录与持久节点状态

本文记录 `tun-service-plan.md` 的资源隔离细节；持久节点状态部分仍是待实施设计，不代表 Tailscale、ZeroTier 迁移已完成。

## 运行资源隔离

每个经过 OS 身份验证的会话使用独立私有目录：

```text
session/
  snapshots/revision-N/assets/       上传快照，不传给 Mihomo
  assets/revision-N/assets/          可重新生成的工作资源
  assets/revision-N/providers/       检查并重写后的本地 provider
  assets/revision-N/cache/providers/ HTTP provider 独立缓存
  configurations/revision-N/runtime.yaml
  home/                             Mihomo 运行缓存
  validation/                       Mihomo 校验缓存
```

`SAFE_PATHS` 仅包含 `session/assets`；Mihomo 自身的 home 仍由其原生安全路径检查允许。配置中的私有控制器地址、密钥位于 `configurations`，不在 home 或 `SAFE_PATHS` 中。禁止使用 `SKIP_SAFE_PATH_CHECK`。

`StagedRuntime::new_with_configuration` 接收上传目录与配置目录，工作资源目录从固定会话结构推导。目录由服务创建并保护，客户端只能上传 `assets/` 下的受限相对路径。配置主文件和 provider 中的 TLS、SSH、ZeroTier planet 等文件字段指向绝对工作资源路径。

本地 YAML provider 在独立副本中检查和改写；上传快照保持原样。HTTP provider 保留 URL、interval、proxy、header 和 override，缓存路径由服务生成，不能使用上传文件路径。动态下载和表达式执行后的节点仍接受 Mihomo 的原生 `IsSafePath` 检查。每次准备资源使用单文件原子替换，不删除正在运行的工作资源目录；回退从旧快照重建工作资源。

单个 stage 的上传快照、工作资源、副本 provider 和配置预留合计预算为 256 MiB。两种 provider 的总数最多 256；HTTP `size-limit` 按剩余预算平均分配，保留用户更小的正限额。重复准备按目标资源重新计算，不重复累计虚假的占用。会话还需观察真实磁盘用量，覆盖最多三个 stage 及运行、校验 home；这属于超限检测与停止策略，不能称为操作系统实时写入配额。

相关依据为固定版本 [Mihomo v1.19.30](https://github.com/MetaCubeX/mihomo/tree/v1.19.30)：`adapter/provider/parser.go`、`adapter/provider/provider.go`、`adapter/provider/override.go`、`component/ca/keypair.go`、`component/ech/key.go`、`adapter/outbound/ssh.go`、`adapter/outbound/zerotier.go` 和 `adapter/outbound/tailscale.go`。provider 的表达式在 `adapter.ParseProxy` 前执行；原生文件读取限制必须持续有效，不能只检查首次下载。

目前准备层只接受带 `proxies` 序列的本地 YAML proxy provider。Mihomo 的 Base64/V2Ray 转换输入需要客户端先规范化或另行实现等价准备，不能报告为已完整支持。资源准备与文件系统测试不能替代真实 Mihomo、原生服务、TUN 验收。

## 持久节点状态设计：待确认并实施

`state-dir` 不能简单重写到每个 revision 的工作目录，否则配置切换或应用重启可能丢失节点身份。计划将状态保存在独立受保护目录 `service_root/state/<owner-key>/<logical-state-key>`，跨会话和 revision 复用。同一个逻辑状态来源只允许同一 OS UID/SID 使用。目录键由经过验证的 OS 用户身份和客户端规范化的原状态来源生成摘要，不记录用户原绝对路径或身份文件内容到日志。

客户端先按固定 Mihomo 版本计算原状态目录，再以用户权限读取其完整文件清单和内容，通过现有受限资源上传传递。服务不能以管理员权限读取客户端指定的用户目录。导入清单需版本化，列明协议类型、逻辑来源键、相对路径、大小和摘要；也需区分“已检查且原目录不存在”的首次使用与“已有身份但读取失败”。后者应明确失败，不能创建新身份继续运行。

固定 v1.19.30 的默认路径必须保持：

- Tailscale 省略 `state-dir` 时为原 Mihomo home 下的 `tailscale`。
- ZeroTier 省略 `state-dir` 时为原 home 下的 `zerotier/<network>-<sha256(name) 的前六字节十六进制>`，其中 network 与 name 使用实际节点配置值。
- 显式 `state-dir` 按原 home 解析；不能用服务的新 home 代替原来源计算。

首次导入使用私有临时目录，校验完整清单、禁止符号链接或 reparse point，再原子发布并提交持久记录。持久目录已存在时复用其运行后的状态，不以旧上传快照覆盖。协议类型、用户身份或逻辑来源冲突应拒绝并保留现场。重新导入或重置节点身份需要单独的显式产品操作，普通修复服务、切换配置与重启不得隐式执行。

建议初始导入预算为 64 MiB、256 个文件、相对路径最多 16 层。持久状态记录与原子导入的 schema、配额执行、退出时的文件持久化以及卸载后的保留策略仍需单独实现和验证。建议卸载服务保留节点状态，由明确的数据清理操作删除；该建议尚不构成已实现行为。

## 后续验收

验证旧身份首次导入、同 revision 停止再启动、跨 revision 重载与回退、服务修复、应用和系统重启均保持身份；验证部分上传、导入中断、权限拒绝、预算超限和冲突不会创建新身份。真实 Tailscale/ZeroTier 身份应比较其实际状态文件或节点 ID，不能以目录名称相同作为通过依据。
