# 特权服务安全模型

- 制定日期：2026-10-18。
- 适用范围：`zenclash-service` 特权 IPC 服务、Unix Socket / Windows 控制管道，以及服务与 GUI、Mihomo 之间的认证边界。
- 本文记录当前实现的信任假设，不代表未来多用户支持承诺。

## 资产

服务以 root / LocalSystem 运行，能够：

- 启动和停止官方批准目录中的 Mihomo；
- 修改系统代理、PAC 与 TUN 相关配置；
- 读写服务自身运行目录与 owner 会话状态；
- 代表当前登录用户执行需要特权的维护操作。

## 信任假设

- 目标环境是**单用户工作站**：本机所有已登录账户被视为可信，或至少不试图互相隔离。
- 服务由管理员安装；安装目录、批准内核目录和运行目录由 root / SYSTEM 控制。
- GUI 以普通用户运行，服务不能假设 GUI 进程已提权。

如果未来需要支持多用户隔离，必须引入安装期允许账户列表或每用户服务实例；当前实现不提供跨用户隔离。

## 认证

### Unix

1. 传输层提供内核 peer credentials（uid/gid）。
2. 请求携带声明的 `OwnerIdentity`，其 uid/gid 必须与 peer credentials 完全一致。
3. 服务 canonicalize 调用方声明的 app data root，并验证其目录 owner 就是该 uid。
4. 通过后返回 owner key；后续会话请求必须携带服务签发的会话令牌。

因此，**任一本地用户都可以使用自己的身份成为 owner**，但无法冒充其他 uid，也无法使用不属于自己的 app data root。

### Windows

1. 请求携带当前用户 SID 与 32 字节 owner token。
2. 服务打开 app data root 与 token 文件时拒绝 reparse point，验证文件类型、owner SID 与 DACL，并使用常量时间比较 token。
3. 控制管道使用 SDDL，限制为 SYSTEM、Administrators 和已认证用户（AU）。

## 传输权限

- Unix IPC socket 位于 macOS `/var/run/zenclash-service/service.sock`、Linux `/run/zenclash-service/service.sock`，监听模式为 **0o666**，运行目录为 0o755。
- 原因：服务以 root 运行，GUI 以普通用户运行，socket 必须对桌面用户可写；认证不依赖文件权限，而是每个请求的 peer credentials 与 app data root 校验。
- Windows 控制管道使用显式 SDDL，等价地限制为已认证本地用户，因此两个平台的授权边界是“认证用户可以连接，按请求身份授权”，而不是“只有某个固定用户能连接”。

## 已知后果

- 任意本地已认证用户都可以尝试成为 owner，并触发服务停止旧 owner、启动批准内核、修改全局系统代理。
- 任意本地用户都可以连接 socket 并发送未认证请求；服务需要依赖每请求认证、超时和生命周期门控抵御资源消耗。
- 恶意本地用户无法直接让服务执行任意二进制：内核必须来自管理员批准目录，并经过 SHA-256、候选程序 `-v` 与配置预检。
- 服务不提供跨用户的数据隔离；不同用户的 app data root 由各自 owner 路径隔离，但全局系统代理与 TUN 状态是机器级的。

## 加固选项（未实现）

- 安装时记录允许的 UID/SID，并在 `authenticate_owner` 中校验。
- 将 socket owner 改为安装用户并收紧为 0o600，运行目录收紧为 0o700。
- 为多用户场景提供每用户 socket 与服务实例。
- 为 socket 连接增加速率限制和并发上限。

修改上述权限或认证路径时，需要同步更新本文与 `crates/zenclash-service/src/core/server.rs`、`auth.rs` 的测试。
