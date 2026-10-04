# macOS 已加载服务归属查询研究

研究日期：2026-10-02。对应 [开发计划](tun-service-plan.md) 的有效注册归属门禁；仅为源码与官方接口研究，没有执行 macOS 原生查询、安装或授权。

当前固定 plist 检查已经验证磁盘文件及维护副作用的前置条件，不能证明同名已加载 job 的实际执行路径、参数与账户属于 ZenClash。必须同时处理 active 与 inactive job；仅有活进程检查不能完成该目标。

| 候选 | 可用事实 | 尚缺的证据 |
| --- | --- | --- |
| `SMJobCopyDictionary(kSMDomainSystemLaunchd, label)` | 返回结构化 job description | 接口已弃用，公开契约不保证完整有效账户字段；没有公开的调用超时契约 |
| `proc_pidinfo`、`proc_pidpath` 与前后出生身份 | 活进程的 UID/GID、执行文件与 PID 复用核验 | inactive job 没有 PID，且进程路径不能代替注册参数 |
| `SMAppService` | bundle 内服务注册与授权状态 | 不提供任意已加载 job 的完整配置查询；采用它涉及部署方案及最低系统版本决策 |
| `launchctl print` | 可观察系统诊断输出 | 未找到稳定字段语法契约，不据诊断文本构造可靠归属门禁 |

接口依据见 [Apple SMJobCopyDictionary](https://developer.apple.com/documentation/servicemanagement/smjobcopydictionary(_:_:))、[Apple libproc](https://github.com/apple-oss-distributions/xnu/blob/main/libsyscall/wrappers/libproc/libproc.h) 和 [Apple WWDC22 服务管理说明](https://developer.apple.com/videos/play/wwdc2022/10096/)。

[Apple 公开的旧 launchd 源码](https://github.com/apple-oss-distributions/launchd/blob/main/src/core.c) 中，`job_export` 导出程序、参数和 PID，但未导出 `UserName`/`GroupName`。这些历史实现只能解释风险，不能作为当前系统返回契约；缺字段不能据此推断实际账户为 root/wheel。[Apple launch.h](https://github.com/apple-oss-distributions/launchd/blob/main/liblaunch/launch.h) 将 `launch_msg` 用途限定为 check-in，不采用私有 GetJob 协议填补缺口。

下一步需要在最低支持系统及当前 macOS 上作只读接口验证，记录 SDK、系统版本、完整字段类型及查询失败行为。候选 typed API 不能在未经验证时当作完成方案；线程取消也不能证明原生调用已经结束。新增查询子命令、改用 SMAppService 或改变系统支持范围须另行决策。

2026-10-03，用户确认 §7.3 原子维护后，active host 关联查询进入适配开发：只使用固定 system domain 与固定标签的 `SMJobCopyDictionary`，严格读取 integral CFNumber PID，再与原生 socket 对端的 PID、出生身份、UID 和固定 helper 执行路径核对。缺字段、错误类型、NULL、查询变化均为 Unknown，不能由缺少 UserName 推断 root。此项仅证明维护将控制的活动宿主关联，不补全 inactive job 或已加载参数归属。

为约束原生 API 的阻塞，查询放入固定 helper 的只读子命令，由既有有界 Unix 子进程捕获设置期限、输出预算、超时终止及回收；不使用线程取消证明 API 已退出。链接系统 ServiceManagement/CoreFoundation 框架，不新增 Rust 依赖、持久化结构或改变最低系统版本。当前未取得 macOS SDK 链接或原生调用结果，不能把 portable 行为及交叉 check 记作原生查询通过；若目标系统不提供可用结果，维护必须拒绝并保留恢复材料。

上游 `core/process.rs::process_identity` 的 macOS 路径及出生身份检查可以适配 active host 核验，再补 UID/GID；它不能解决 inactive job 的维护归属。源码复用不能缩小原交付要求。

后续行为回归至少覆盖缺账户或参数、字段类型错误、NULL、查询失败、PID 复用及查询期间 job 改变：保持 Unknown，零启动、停止、覆盖或删除。还须验证合法 inactive job 的修复路径，不能以永久拒绝维护代替完整实现。当前没有这批回归或原生验收证据，完整 loaded-job 门禁保持未完成。
