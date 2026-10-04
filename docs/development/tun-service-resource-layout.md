# TUN 服务资源目录与持久节点状态

本文记录 [开发计划](tun-service-plan.md) 的资源隔离细节；修订日期：2026-10-04。同会话缓存继承、provider 底层回读及部分高层恢复已有阶段验证；跨会话缓存保存、稳定 home 和持久节点状态仍待实施，不代表 Tailscale、ZeroTier 迁移已完成。新增存储与失败策略的具体提案见 [跨会话缓存实施方案](tun-service-cache-persistence.md)，尚待用户确认。来源与阶段证据见 [移植计划](tun-service-upstream-migration.md) 和 [实施记录](tun-service-progress.md)。

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

## 同会话缓存继承：首批已验证

上游差异规划适配为内存 manifest，记录完成上传资源的 SHA-256/长度及声明的远程 provider URL。每次准备复核固定文件内容，manifest 只在准备成功后发布；Validate 后继续上传会使旧校验失效，正式 materialization 后才冻结上传。

相同 URL 的 provider 缓存从当前 accepted 资源复制到独立候选，按实际字节重新计入预算。proxy 缓存仅允许将声明且内容相同的已批准 TLS/SSH 等资源映射到新工作目录；未知路径、资源改变、解析超过 4 MiB 或字节预算不足时不继承该缓存，保留原有受限下载流程。服务不会打开缓存中任意被引用的系统文件。rule 缓存保留字节语义，URL 改变使候选缓存失效，旧 accepted 不被修改。

固定 v1.19.30 的 `component/resource/vehicle.go` 使用 `os.WriteFile` 原地更新缓存，而非原子 rename。当前缓存继承的长度/mtime 前后检查只能拒绝观察到的变化，不能证明运行中读取的是完整版本；本批未验证原生 writer 暂停在半份文件时的强一致性。完整缓存同步需在高层确认内核停止后导出，或后续提供可验证的 writer 协调，不能以连续两次摘要相同宣称已解决。

整个 stage 共用三次有界等待，累计 175 ms；不会按每个 provider 重新发放重试预算。准备成功后释放 prior 资源引用，不形成无限 revision 保留链。runtime 34 项及实际服务 Stage 调度 1 项通过，首审通过；真实内核下载和原生文件竞争仍待验收。

## provider 缓存回读：底层阶段通过，高层保存待接入

回读只接受经过会话授权、匹配 committed revision 且 manifest 已声明的 provider 标识；不接受用户文件路径，不开放主配置、控制器秘密或任意系统文件。路径解析、固定句柄和父目录保护继续复用服务现有校验。

已确认回读实施方式：Begin 只在新鲜状态确认内核已停止、无未提交候选且 committed revision 匹配时接受，接口自身不 Stop。高层捕获/退出/重启事务负责原生停止、导出及后续启动或恢复；只落服务端端点不能算完整缓存同步完成。

当前未发布协议源码已包含 `BeginProviderCacheRead`、`ReadProviderCache` 和 `FinishProviderCacheRead`，客户端与服务端已接入具名操作，分块快照适配自上游 `runtime_generation/readback.rs`。首审前相关测试 16 项、完整服务 195 项、service 默认/服务端完整 CI lint、Windows 和 Linux/macOS 交叉 check 通过。首审发现被拒绝 Start 重置总预算的问题，3 项实际 State 测试先失败再通过；修后相关测试 19 项、check/完整 CI lint 通过，第二轮审查结束。随后最后增量完整服务回归 198 项、Linux/macOS service all-targets/all-features 交叉 check 均通过；普通权限保存与高层恢复仍需接线。这些结果不包含真实 Mihomo 或原生文件竞争验收。

采用单个有界内存快照，避免分页期间文件变化或长期持有 Windows 文件锁。rule/MRS 原始字节最多 128 MiB，proxy YAML 解析及批准资源反向映射最多 4 MiB；传输块 256 KiB，整个 revision 回读最多 256 MiB、256 次尝试、15 秒总期限，分页不续期，finish 不重置整批预算。临时快照最多增加 128 MiB 服务内存，不能以单块大小代表总占用。排队取消不准入；已准入 Begin 的调用方取消等待后，完成任务仍持门栓直到实际文件复制结束。finish、过期、revision 替换和 owner 释放回收快照，但不将丢失响应当成明确取消或可盲目重试。不新增用户落盘格式或依赖；跨会话缓存保存和节点身份迁移分别实施。

## 高层缓存保存与本地恢复：已有阶段接线，跨会话仍待实施

CoreSession 已接停止与内存导出入口：已准入完成任务持有租约和会话门栓，确认停止后读取当前 accepted revision 的 HTTP provider 缓存，发布前复核状态。全部成功才替换内存 bundle；失败保留旧 bundle 和停止意图。原 YAML 与非 HTTP 缓存资源保留。随后 GUI 维护已接到 Service→Local 恢复，使用用户确认的 `ControlledConfigStore.root()/local-runtime/{slot0,slot1}` 生成 TUN-off 配置和持有资源，并保留原 Mihomo home；GeoData 仅对固定名称有界替换并覆盖失败回滚。Local→Service 接续也已接 HTTP provider 缓存导出。阶段回归见 [实施记录](tun-service-progress.md)，完整原生串联和跨会话持久化仍未完成；历次证据按批次区分。

高层在确认 Stop 后、Release 前冻结准确的 revision 与资源 bundle，回读声明的缓存，验证完整长度与摘要，再以普通权限原子保存到托管缓存。不能写回导入的订阅、TLS 源文件或 HTTP provider 的任意原始路径。保存工作与后续启动/释放由同一完成任务持有，外层取消等待不丢弃已准入写入；丢失 Begin/Read 响应时保留准确的未知结果，不自动重发。已确认的双槽目录不等同于跨会话 provider 映射与节点身份清单的批准；后两者的持久格式及保存失败对退出/重启的影响仍需明确。

修复/卸载恢复 Local 保留不可变启动描述与最新 accepted bundle，在普通用户的受控生成目录物化 TLS、文件 provider 等资源，保留原 Mihomo home。2026-10-03 用户已确认 `ControlledConfigStore.root()/local-runtime/{slot0,slot1}` 双槽生成与固定 GeoData 原子激活/失败回滚的范围：无既有用户迁移，不改写订阅/TLS 源。双槽文件生成已有 Windows/普通 WSL Linux 阶段验证，随后 GUI 维护已接入 Manager 恢复流程，完整原生串联仍未验收。GeoData 从 home 下固定名称读取，不能仅改 YAML 引用；`with_local_geodata` 先有界备份再原子激活，正常错误恢复旧字节与原先不存在的状态，静态链接目标拒绝。维护调用方必须确认内核停止并正确发布 Local owner；已有阶段接线和失败回归不能代替真实服务验证。详细边界见 [core 接入文档](tun-service-core-integration.md#55-修复与卸载接线已确认方案分批实施)。

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
