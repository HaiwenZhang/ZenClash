# 跨会话 provider 缓存实施方案

日期：2026-10-04。状态：待用户确认新增持久结构；本文是可审查方案，不代表已实现。对应 [开发计划](tun-service-plan.md) 的缓存与身份持久化工作；节点身份另见 [资源布局](tun-service-resource-layout.md)，不混入可丢弃 provider 缓存。

## 现状与目标

[RuntimeSession](../../crates/zenclash-core/src/service_runtime_session.rs) 已在确认 Stop 后导出 committed revision 的 HTTP provider 缓存，全部成功才替换内存 bundle。[CoreSession::shutdown](../../crates/zenclash-core/src/core_session.rs) 当前直接 Release，释放后清空 active/candidate；尚无退出缓存持久化。已批准的 `local-runtime/{slot0,slot1}` 用于恢复配置及持有资源，不能充当跨会话 provider 来源索引。

目标是在正常退出、显式服务重启与 Service→Local 恢复中，先冻结并保存已停止内核的缓存，再 Release 或重启。下次准备同一来源时复用最后成功保存的缓存；源或批准资源改变时不复用旧缓存。原订阅、TLS 源和 HTTP provider 原始路径不改写。服务不以管理员权限访问用户目录。

## 存储结构与版本

新增 `ControlledConfigStore.root()/provider-cache/`，与双槽恢复目录分开：

```text
provider-cache/
  current.json
  slot0/
    manifest.json
    blobs/<sha256>.bin
  slot1/
    manifest.json
    blobs/<sha256>.bin
```

`current.json` 为版本 1 的发布记录，字段为 `schema_version`、`slot`、`generation`、`manifest_sha256`；`slot` 仅允许 0 或 1，generation 仅为本地发布序号，不充当 Service revision、会话或 owner 证明。

`manifest.json` 字段为 `schema_version`、`generation`、`entries`。每项包含 `provider_kind`（proxy/rule）、`source_key`、`length`、`sha256`、`resources`。`resources` 只保存缓存中的批准资源别名与内容摘要，用于下次准备时映射到新 bundle 的已批准资源；不得据清单打开任意原始路径。未知字段、重复来源键、未知版本、非正规摘要、越界 slot 和长度冲突均拒绝。

`source_key` 是规范化 provider 声明的 SHA-256：包含实际源 home 的规范化身份、provider 名称/类型、URL、proxy、header、override 及其他影响缓存解释的参数；忽略由服务生成的工作目录，资源引用改用批准内容摘要。规范化采用固定字段排序与明确的缺失/null 区分，不能依赖 YAML 文本格式或 HashMap 遍历顺序。版本 1 不跨规范化规则迁移，规则改变须升级 schema。

清单不保存 URL、header、控制器密钥或原 TLS 路径明文。缓存内容自身仍可能含节点信息，目录和文件按普通用户私有权限保护；日志只记错误分类和有界计数。复用现有 SHA-256、原子写入、路径保护与 Data lease，不新增第三方依赖。缓存不加入既有备份归档，保持生成数据排除约定。

## 预算与事务

每个已发布 slot 的 payload 总量最多 256 MiB，proxy YAML 单项最多 4 MiB，rule/MRS 单项最多 128 MiB；最多 256 项，与现有回读预算一致。清单最多 1 MiB。两个 slot 最多 512 MiB；写入非活动 slot 前，仅删除已确认非活动的旧生成物，临时发布目录另计最多 256 MiB，峰值最多 768 MiB 加有界清单。不得删除不认识的目录以腾出预算，也不把此限制称为内核运行时文件写入的 OS 配额。

同一配置事务持有覆盖 store root 的写租约及既有 store mutation gate。先通过现有 Stop/状态复核冻结缓存，验证完整字节与摘要；在非活动 slot 的私有 staging 目录构建完整一代，检查同目录每个句柄和父目录，拒绝符号链接、junction、设备与非普通文件。文件及清单完成同步后原子发布 slot，最后原子更新 `current.json` 并同步父目录。

发布 pointer 前失败保留旧 current 和活动 slot；pointer 已发布后，旧 slot 保留到下一次可安全轮换。无 pointer 的完整 slot 或 staging 不能被自动当作最新缓存。重开 store 时只按已验证 pointer 读取；摘要、清单或 blob 不一致不接受半代数据。不借“连续两次相同摘要”代替 Stop。

已准入保存由同一个 owned completion 保留门锁和租约，调用方取消等待不撤销保存，也不提前 Release。正常错误清理只处理本次已验证的私有 staging；崩溃遗留目录保留至受控检查后清理，不能盲目递归删除。

## 保存失败与再次启动

建议区分可丢弃缓存和运行资源：仅 provider 缓存落盘失败时，保留最后成功发布的一代，记录缓存保存警告，继续既有确认停止及 Release/Restart，不因可重新下载的数据阻止正常退出。Stop、Release 或维护恢复必需的资源导出未确认时，继续沿原有失败路径保留 owner 并禁止 Local 启动或原生维护，不能将它们降级为缓存警告。内核已停止不等同于服务 owner 已释放。保存成功但 Release 失败允许重试幂等收尾；下一次保存按同一冻结候选比较，不覆盖另一事务的新成功代。

新安装和旧用户不存在该目录时视为冷缓存，无用户迁移。损坏生成缓存不覆盖源数据；启动准备拒绝复用该缓存并提示重新下载，保留现场，不把损坏缓存用于内核启动。读取失败和来源改变均不伪装成有效命中。有效来源匹配后，proxy YAML 只将批准资源摘要映射到新 bundle；未知引用或批准资源变化时不继承，沿既有受限下载流程。rule/MRS 保留原始字节。

缓存是性能数据，节点身份不是。Tailscale/ZeroTier 身份不存入此目录，也不随 slot 淘汰；其稳定目录、导入清单、协议能力以及卸载保留策略需要独立确认，不能把本方案当作该批准。

## 接线与行为验收

1. core 增加私有缓存存储实现，测试跨实例重开、完整发布和来源匹配；不向 UI 暴露文件事务细节。
2. 配置准备在冻结候选时加载匹配缓存，继承后仍执行既有资源预算、路径重写和最终 TUN 判定；授权等待期间不持 Data lease。
3. CoreSession/RuntimeSession 在 Stop→保存→Release/Restart 的同一完成任务中接线；GUI 沿现有退出、维护与重启错误流程反馈，不新增恢复窗口。
4. 行为覆盖 URL/header/override/TLS 变化失效、缓存未下载、旧原始资源删除后批准内容继承、损坏清单/摘要、写失败、pointer 发布失败、取消、失败重试、退出并发与另一账户隔离。真实普通 Mihomo 验证 provider 下载后 Stop、重开新 session 的缓存内容及外部原始文件不变。
5. Linux/macOS/Windows 分别验证原子发布和静态链接目标拒绝；三平台真实服务退出、维护及重新启动另行验收。按 AGENTS.md 在 check 后独立审查，最多两轮；源码接线、存储测试或构建成功不能算完整功能交付。

## 决策依据与影响

上游 [owner 目录](../../examples/clash-verge-service-ipc/src/core/paths.rs) 与 [期望状态写入](../../examples/clash-verge-service-ipc/src/core/desired.rs) 可复用按 owner 隔离和原子发布思路。不能直接复制 `last_clash_config` 或自动拉起：ZenClash 继续由 CoreSession 管理唯一 owner、已提交配置和应用退出生命周期。

本方案新增普通用户生成目录与版本 1 清单；不改变用户订阅、设置结构、备份版本、服务线协议或固定内核版本。现有用户首次成功导出时生成，不需要迁移。推荐缓存落盘失败保留旧代并警告，继续正常退出；若要求缓存强持久化，则改为保存失败阻止退出/重启并保留 owner，但这会因可丢弃数据的 I/O 失败阻止关闭应用。持久结构和失败策略需一并确认后实施。
