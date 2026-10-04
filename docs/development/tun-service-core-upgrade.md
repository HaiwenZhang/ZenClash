# Service 内核升级接线方案

日期：2026-10-04。状态：范围待确认，尚未实施。本方案对应 [主计划 §7.2](tun-service-plan.md) 的受保护内核副本升级；普通 Local 升级和恢复 owner 的双槽许可已有实现，不能算作本方案完成。已交付的 Windows 测试 ZIP 不因后续开发被覆盖。

## 当前证据

- [GUI 升级入口](../../crates/zenclash-ui/src/pages/runtime/mihomo/maintenance.rs) 明确拒绝 Service backend；[CoreSession](../../crates/zenclash-core/src/core_session.rs) 的 install_release 只接受 Local owner。
- [候选准备](../../crates/zenclash-core/src/core_update/service.rs) 已有下载摘要、解压、版本预检和同目录暂存，PreparedCoreUpdate 负责暂存清理；其 activate 路径会替换普通 Local binary，不能直接用于“只升级服务副本”。
- [维护准备](../../crates/zenclash-core/src/service_manager/maintenance.rs) 已支持 Stop/导出/Release 后恢复真实 Local，再请求原生维护。当前 Repair 的 core 来源固定为 ordinary_launch.binary，没有独立候选来源。
- [安装 worker](../../crates/zenclash-service/src/installer.rs) 已将已批准 helper/core 的摘要副本部署到服务私有目录，并由 [安装 journal](../../crates/zenclash-service/src/maintenance_journal.rs) 管理提交前回滚和中断恢复。journal 提交只证明安装与宿主就绪，不证明后续配置、捕获和 Mihomo /version 已成功。
- 本地上游 [install_service.rs](../../examples/clash-verge-service-ipc/src/bin/install_service.rs) 的 prepare_requested_cores、publish_requested_cores 可参考候选校验与发布顺序，但其逐文件发布明确不回滚此前成功项；本项目继续使用已有 journal，不能原样替换成逐文件复制。

## 建议范围：升级受保护服务副本

Service 运行时点击内核升级，下载并校验候选，通过既有 Repair 部署候选 core 和当前应用配套 helper，然后恢复原捕获意图。普通用户目录的 Local binary 保留原版本，作为既有失败恢复身份；不调用 PreparedCoreUpdate.activate，也不把候选暂存路径保存为 Local 的常驻 binary。

使用现有 Repair 意味着 helper 与 core 仍按既有成对部署事务处理，不承诺仅替换一个文件。已安装 helper 不兼容、归属不可信、存在其他 owner 或旧 v2 注册未完成迁移时，继续拒绝；升级不能成为绕过原子维护、同名注册归属或多账户策略的入口。

该范围不增加第三方依赖、用户配置格式或安装 journal 格式。旧用户无需数据迁移；只替换服务已批准产物和其既有摘要元数据。普通 Local 版本可能与服务内核不同，界面按实际 backend 的 /version 显示版本；以后恢复 Local 时仍对冻结配置进行真实预检，不把旧普通内核假装成新版本。

另一选项是同时升级 Local 和 Service 两份副本。它需要协调普通文件切换、原生 journal、运行配置与两处提交结果；不能用“Local 更新成功后再 Repair”冒充一个可回滚事务。新增持久化协调记录及中断恢复的迁移影响需单独说明和确认，当前不先实现这一选项。

## 接线与所有权

1. ProfileService 的 owned 业务任务持有一次升级意图；ServiceManager 绑定 session、binding、generation 和 capture revision。GUI 仅发起请求并接收业务收据，页面销毁不丢弃已准入操作。
2. 在变更 capture/owner 前下载候选、校验 release 摘要和版本，保留暂存所有者及候选内容摘要。授权等待不得持 Data、capture、transition 或 store mutation 锁；等待结束重新核对意图和候选。
3. 候选身份与 ordinary recovery launch 分开保存。维护准备恢复的是原 Local binary/home 与 held TUN-off 配置；Repair 的批准 core 来源改为候选。绝不把候选当作恢复 binary，也不重新打开变化中的订阅或 TLS 源。
4. 复用确认 Stop→完整缓存导出→Release→真实 Local 恢复。导出、释放、预检、Local 就绪或保存未确认时，不发起管理员维护，保留已有恢复材料和唯一 owner。
5. 原生请求必须携带准备时的候选摘要，而不是在授权后重新计算并批准变化后的文件。扩展现有维护 API 的类型化 attestation，worker 保留 pinned source、大小预算和摘要复核；候选在授权前后被替换都必须拒绝。这个跨 crate API 改动须在确认范围后实施，不新增 IPC 任意文件执行能力。
6. 复用 v3 原子维护门锁、原生注册归属、现有 Repair journal 和替换后宿主验证。完成后刷新可信 health，再用 held 恢复资源返回 Service；只有 native 结果确认才能进一步恢复捕获。
7. 实际 Service /version、运行配置和保存收据均确认后，才发布升级完成。释放候选暂存不能早于 worker 完成或结果已对账；调用方取消等待不提前删除 worker 尚需读取的候选。

## 失败与提交边界

| 边界 | 要求 |
| --- | --- |
| 下载、候选校验或意图复核失败 | 零 Stop、零捕获变更、零原生维护；只清理本次暂存 |
| Service Stop/导出/Release 未确认 | 不启动 Local 或提交维护；保留 owner/资源并等待对账 |
| Local 恢复失败 | 禁止维护，保留失败收据；退出仍清理可达 owner |
| 用户取消原生授权 | 不部署候选；只能在已确认状态下恢复旧 Service 与捕获，失败不能隐藏 |
| 原生 journal 提交前失败 | 沿既有事务恢复旧批准产物及原服务状态，回读后才能恢复捕获 |
| 原生结果未知 | 不重发部署、不假定回滚成功、不自动开启捕获；保留暂存及对账所需身份 |
| 安装已提交，但配置/版本/捕获恢复失败 | 明确返回“安装已提交、运行恢复未完成”的业务结果；保留真实 owner，不声称整个升级已回滚 |
| 退出/等待者取消 | owned completion 与 shutdown 协调唯一 owner；不遗弃 Local 或服务内核，不在主线程等待 |

安装提交后若需退回旧正式内核，应作为一次新的、明确授权的维护事务；不能擅自使用已清理的旧 journal 回滚，也不能把运行恢复警告改写成升级成功。

## 行为验收与实施顺序

先补能观察 Stop、部署、保存和版本回读顺序的失败回归，再接候选 attestation 与 Manager 业务事务，最后改 GUI Service 分支及双语文案。超过三十行时按项目规约在 cargo check 后独立审查，不以 mock 流程代替原生验收。

- 候选校验拒绝、等待期间配置/捕获变化、candidate bytes 替换、陈旧会话：提交前拒绝且零管理员操作。
- 所有者取消、退出竞争、未知 Stop/Release、导出不完整、恢复失败：维持唯一可达 owner 和恢复材料。
- 候选暂存由已准入 completion 持有；外层等待者取消后，worker 仍可读取准确字节且不会再次部署。
- 原生失败与未知回执分别测试；已提交安装不能被 UI 当成未提交回滚。
- 成功后通过可信 Service 控制器确认请求版本、有效配置、捕获状态和提交收据；普通 Local binary 的字节保持原样。
- Windows/macOS/Linux 原生维护与真实 Mihomo 各自验收，覆盖权限取消、旧 v2、其他账户 owner、进程崩溃、升级中断及退出。没有原生证据的项继续标为未验收。
