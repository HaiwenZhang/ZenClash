# TUN 服务打包记录

本文对应 [开发计划](tun-service-plan.md) P5，记录已接入的产物路径及验证边界；不代表发布包已经完成三平台 TUN 验收。

## Linux 产物

[DEB 构建脚本](../../scripts/build_deb_package.sh) 和 [RPM 构建脚本](../../scripts/build_rpm_package.sh) 在构建 GUI 后单独执行：

```sh
cargo build --release --locked -p zenclash-service --features server --bin zenclash-service
```

构建脚本确认 helper 非空、可执行且 `--version` 成功并返回非空白内容后，才进入安装包生成阶段。helper 缺失、版本命令失败或输出为空时不能输出成功安装包。现有 Mihomo 下载版本、校验方式和发布架构保持原约定。

| 包管理器拥有的文件 | 用途 |
| --- | --- |
| `/usr/lib/zenclash/zenclash-service` | 普通用户应用调用的安装维护源程序 |
| `/usr/lib/systemd/system/zenclash-service.service` | 固定 systemd unit 资源 |
| `/usr/share/polkit-1/actions/org.zenclash.service.policy` | 固定 Polkit 授权资源 |

unit 的 `ExecStart` 指向 `/var/lib/zenclash-service/zenclash-service run`。该保护目录中的服务与 Mihomo 副本由管理员授权的安装事务部署，并记录批准摘要。打包脚本不创建安装元数据、不启动服务，也不修改网络；仅安装 DEB/RPM 不足以完成首次 TUN 开启。

应用内维护只管理自己的 `/etc/systemd/system/zenclash-service.service` 注册与保护目录数据，不删除上述包管理器文件。包升级、卸载时的真实运行服务协调尚待接入和验收；不能把现有 payload 测试视为升级/卸载已完成。

## Windows 产物

[Windows 构建脚本](../../scripts/build_windows_installer.ps1) 额外使用 `server` feature 构建 `x86_64-pc-windows-msvc` 服务二进制。helper 暂存到应用根目录的 `zenclash-service.exe`，校验非空及 `--version` 的退出码和输出后才调用 Inno Setup。

[安装包定义](../../platforms/windows/ZenClash.iss) 显式包含 helper，保留 `PrivilegesRequired=lowest`。安装包分发维护源程序；首次启用时另行请求 UAC，将批准的副本部署到保护目录。此阶段没有新增自动注册服务或执行 helper 的安装项。卸载 GUI 与已安装服务的协调尚待完成。

## macOS 产物

[App 构建脚本](../../scripts/build_macos_app.sh) 使用原有 `aarch64-apple-darwin` 目标构建服务，校验非空、可执行及版本检查后，将 helper 放入 `Contents/MacOS/zenclash-service`，LaunchDaemon 资源放入 `Contents/Resources/org.zenclash.service.plist`。plist 中的服务路径仍指向管理员保护目录，打包不会向 `/Library/LaunchDaemons` 写文件。

helper 按现有 Developer ID 或 ad-hoc 分支单独签名，再独立验证，之后签名和验证整个 App；未调整签名身份、最低系统版本或发布架构。App 构建脚本改用 macOS 自带 Bash 支持的语法，便于直接运行受控打包行为测试；DMG 入口仍使用原有脚本。

## 自动验证

```sh
bash scripts/tests/build_deb_package_test.sh
bash scripts/tests/build_rpm_package_test.sh
bash scripts/tests/build_macos_app_test.sh
pwsh -File scripts/tests/build_windows_installer_test.ps1
```

DEB 测试运行真实构建脚本，并检查传给 `dpkg-deb` 的暂存树包含原样 helper、unit 和 policy。RPM 测试运行真实构建脚本和 spec 的 `%install` 命令，检查暂存文件的内容。两者均覆盖成功、helper 缺失、版本空输出与非零退出；失败分支须在进入打包工具前停止。两个测试均用受控 Cargo 和打包工具替身，离线执行且不安装系统文件。

这些测试证明构建与暂存行为，不证明原生 DEB/RPM 格式、签名、systemd/Polkit 授权或真实服务生命周期。CI 与发布前 Linux 检查运行两个入口；原生包安装、升级、卸载和首次 TUN 仍需目标发行版实机验收。

macOS 测试直接执行原 App 构建逻辑，检查真实暂存文件、两种签名分支的调用顺序，以及 helper 缺失、空文件和版本失败时不得进入签名。Cargo、签名、架构探测及原生 plist 工具使用受控替身；这不证明 Mach-O 链接、有效签名、Gatekeeper 或 launchd 授权。

Windows 测试执行原 PowerShell 构建脚本，使用普通原生可执行 fixture 验证实际暂存字节及版本命令；覆盖缺失、空文件、构建失败、版本非零退出和空输出。Cargo、图标检查及 ISCC 使用替身，不生成真实安装包，也不操作 SCM。CI 和发布前流程分别在对应平台运行这些入口；本机已验证暂存行为，真实平台产物仍待验收。

## 待接入

三平台包升级/卸载与已安装服务的协调、旧 setuid 迁移，以及授权、签名、首次 TUN 和故障恢复的实机验收仍按 P5/P6 推进。
