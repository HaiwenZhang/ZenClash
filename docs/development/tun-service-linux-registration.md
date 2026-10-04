# Linux 有效服务注册查询方案

日期：2026-10-04。状态：**提案，新增直接依赖待用户确认；尚未实现**。

对应 [开发计划 §7.3–7.4](tun-service-plan.md#73-维护原子准入2026-10-03-已确认开发中) 与 [上游迁移记录](tun-service-upstream-migration.md)。本提案只解决维护前的 Linux 有效注册证据，不替代旧会话退出、内核停止、管理员授权、共享/排他锁或事务恢复。

## 已核实的缺口

- 当前 `MaintenanceAdmission::establish` 对已有 v3 安装要求运行中的可信 IPC 宿主；没有可用 IPC 的已安装服务不能因此获得维护准入。
- 显式 `--offline-migration` 目前要求管理器证明注册不存在，再确认 IPC 端点不存在。Windows 拒绝任何仍注册的服务；Linux 要求 not-found/inactive/MainPID=0；macOS 要求固定 job 未加载。它不能直接完成“保留已停止注册”的迁移。
- 本地上游 `examples/clash-verge-service-ipc/src/bin/install_service.rs` 的 Linux 分支仍通过 `systemctl` 维护，且停止命令的错误被忽略；没有有效 unit 执行归属查询可直接移植。停止失败后替换文件的做法不满足本项目已确认的 §7.3。
- 仓库锁文件已有 `zbus 5.19.0`、`zvariant 5.15.0`，由现有桌面依赖使用；当前 `zenclash-service` 没有直接依赖它们。新增到服务 crate 仍属于新增依赖，不能由已有锁记录推断用户授权。

## 推荐决策

保留上游方向和现有 `systemctl` 注册、启动、停止、卸载路径；**归属查询使用现有锁定版本的 zbus，直接读取 systemd 的类型化 D-Bus 属性**。不增加固定 `/usr/bin/busctl` 要求，不解析 `systemctl show ExecStart` 的显示文本，不手写 libsystemd 的动态装载和 unsafe ABI。

| 选择 | 成本与限制 |
| --- | --- |
| 复用 zbus（推荐） | 服务 crate 增加 Linux/server 专用直接依赖，独立服务的代码体积与构建依赖增加；使用 Rust 类型检查总线数据，没有新增查询工具或 libsystemd 动态链接要求 |
| 固定 busctl | 不增加 Rust 依赖，但增加系统运行时工具要求、子进程及文本解析边界；这是原 §7.4 未获批准的方案 |
| 原生 libsystemd | 不增加 Rust crate，但增加共享库运行时要求和动态装载、FFI、分配释放及类型签名审查成本，不推荐 |

不新增用户配置、安装元数据、备份或协议字段；不修改订阅源。已有用户无需数据迁移。缺少系统总线、systemd、受支持属性或可信证据时明确拒绝维护，不退回磁盘模板或 PID=0 的推断。非 systemd Linux 不因此伪装支持系统服务。

依赖配置拟仅作用于 Linux 的 `server` 功能，复用已锁定版本、禁用默认功能并启用 `async-io`；不在 workspace 中为 zbus 开启 `tokio`，避免 feature 合并改变现有桌面依赖的执行器选择。具体清单与 lockfile 变化在获批后检查；不固定 Rust 工具链版本。当前只取得依赖图与源码证据，新增依赖的 release 体积和运行资源成本尚未测量，实施时记录前后基线。

## 查询与准入边界

1. 使用固定系统总线 socket，清除地址与用户会话覆盖。核实原生对端及 systemd 的总线身份，绑定唯一 owner；查询途中 owner 改变即失败。
2. 只查询固定 `zenclash-service.service`。`GetUnit` 的失败不证明不存在；通过 `LoadUnit` 取得固定对象并核对实际 `Id`、加载状态、磁盘来源、实际 drop-in、transient 与待重新加载状态。
3. 类型化读取有效 `ExecStart` 路径与 argv，仅接受保护目录的固定 helper 和唯一 `run` 参数；核对 simple、root/root、动态用户、替代根、附加执行入口及影响执行或停止行为的属性。运行日志与资源限制类可信定制可保留，不能只要求 drop-in 为空，也不能只读磁盘模板。
4. 维护排他锁始终由原 worker 持有。已有运行中 v3 宿主仍须同一连接的原生身份、握手能力与管理器身份；新查询不能放宽旧协议零控制行为。
5. 只有有效注册归属已确认，且管理器没有活动、启动、停止、控制进程或排队任务、服务 cgroup 与 IPC 端点均确认无活动内核时，才考虑放行已停止安装。旧 v2 还要求显式 offline-migration 与旧应用/会话结束，不能在普通修复里静默停止旧版本。
6. 类型化查询不是管理员并发修改的原子锁。沿用项目的管理员信任边界，副作用前复核有效执行归属和宿主身份；未知或变化保留部署与恢复材料。包管理器拥有的 `/usr/lib` unit 不由应用删除。

上述方法与属性语义以 [systemd 官方 D-Bus 接口](https://raw.githubusercontent.com/systemd/systemd/main/man/org.freedesktop.systemd1.xml) 为依据；`ExecStart` 是结构数组，包含独立路径和参数，PID=0 只表示没有当前主 PID，不能代表内核或整个 cgroup 已结束。

## 实施与行为验收

- 先在 service crate 内实现有期限、有限消息预算的查询，限制队列和收集数量；原始总线消息体在反序列化前检查业务预算。检查锁定 zbus 的底层帧上限，不能把业务预算当作传输层分配上限。拒绝类型、字段、来源及范围不符的结果。
- 使用隔离的普通用户 D-Bus 测试总线及真实类型化消息，覆盖加载但未运行、文件已移除而 unit 仍加载、同名外来服务、全局/前缀 drop-in、transient、替代根、附加停止命令、owner 更换、超限和超时。测试总线不得替换生产固定地址或产生真实系统维护。
- 将已确认归属、未确认归属、活动宿主/内核、停止超时与协议不兼容分别送入真实生产调度路径，观察 start/stop/disable/unlink/文件替换效果；拒绝必须零副作用，不能仅测试解析器。
- 在原准入、journal 与 deploy 路径接入停止状态，覆盖停止、暂存、替换、启动、回读、提交和恢复的失败及强制中断。旧产物恢复不重新启动未经 v3 证明的 helper，不重新设置 setuid。
- 运行相应 check、行为测试、严格 Clippy；按 AGENTS.md 在 check 通过后独立审查。Linux 原生管理员和包升级/卸载、macOS typed job 归属及 Windows 已停止宿主证据各自验收，不能由普通总线 fixture 替代。

本提案获批也不表示上述停止状态已可放行。先完成查询和零副作用测试，再把证明接入已确认的迁移事务；不会用一个较弱的“无 IPC”分支替代整个迁移目标。
