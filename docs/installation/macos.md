# 在 macOS 上安装 ZenClash

简体中文 · [English](macos_en.md)

本文适用于从 ZenClash 官方 GitHub Release 下载的 macOS 安装包。

> [!IMPORTANT]
> 当前公开的 macOS 安装包使用 ad-hoc 签名，尚未使用 Apple Developer ID 签名或经过 Apple 公证。因此，第一次打开时 macOS 会提示无法验证开发者或无法验证应用是否包含恶意软件。这表示 Gatekeeper 无法验证发布者身份，并不表示系统已经检测到恶意软件。

## 系统要求

- Apple Silicon Mac（M1、M2、M3、M4 或更新型号）
- macOS 11 Big Sur 或更高版本
- 安装和首次打开时使用管理员账户

在“苹果菜单 → 关于本机”中可以查看芯片型号。也可以打开“终端”运行：

```sh
uname -m
```

输出应为 `arm64`。当前发布包不支持 Intel Mac 的 `x86_64` 架构。

## 下载安装包

1. 打开 [ZenClash Releases](https://github.com/HaiwenZhang/zenclash/releases)。
2. 下载当前版本的 `ZenClash-<version>-macOS-arm64.dmg`。
3. 同时下载同一 Release 中的 `SHA256SUMS`。

请只从 ZenClash 官方 GitHub 仓库下载。第三方重新打包的文件可能与公开构建不同。

## 校验下载文件

打开“终端”，计算 DMG 的 SHA-256。将下面的 `<version>` 替换为实际版本号：

```sh
cd ~/Downloads
shasum -a 256 "ZenClash-<version>-macOS-arm64.dmg"
grep "ZenClash-<version>-macOS-arm64.dmg" SHA256SUMS
```

两个命令显示的 64 位十六进制摘要必须完全相同。如果不一致，请删除 DMG 并从官方 Release 重新下载，不要继续安装。

## 安装 ZenClash

1. 双击下载的 DMG。
2. 将 `ZenClash.app` 拖到窗口中的 `Applications` 文件夹。
3. 等待复制完成，然后在 Finder 侧边栏推出 ZenClash 磁盘映像。
4. 从“应用程序”文件夹打开 ZenClash，不要直接从 DMG 中运行。

## 首次打开

由于当前安装包尚未经过 Apple 公证，直接双击时可能看到以下提示之一：

- “Apple 无法验证 ZenClash 是否包含可能危害 Mac 或泄露隐私的恶意软件”
- “无法打开 ZenClash，因为无法验证开发者”

请使用 macOS 提供的单应用放行方式：

1. 在警告窗口中点击“完成”或“取消”。
2. 打开“系统设置 → 隐私与安全”。
3. 向下滚动到“安全性”，找到关于 ZenClash 被阻止的说明。
4. 点击“仍要打开”。该按钮通常只在尝试打开应用后的一小时内显示。
5. 使用登录密码或 Touch ID 确认，然后再次点击“打开”。

macOS 会为这个 ZenClash 构建保存例外。安装新版本后，如果应用内容发生变化，可能需要再次执行以上步骤。另见 Apple 的[打开来自未知开发者的 Mac App](https://support.apple.com/guide/mac-help/open-a-mac-app-from-an-unknown-developer-mh40616/mac)。

### “仍要打开”不可用时

先确认 SHA-256 一致，再检查应用包内的 ad-hoc 签名是否完整：

```sh
codesign --verify --deep --strict --verbose=2 "/Applications/ZenClash.app"
```

如果验证失败，请删除应用和 DMG，然后重新下载。不要绕过验证继续运行。

如果签名验证成功，但“仍要打开”仍不可用，可以只移除 ZenClash 的下载隔离标记：

```sh
xattr -dr com.apple.quarantine "/Applications/ZenClash.app"
open -a ZenClash
```

这只影响 `/Applications/ZenClash.app`，不会全局关闭 Gatekeeper。不要使用 `spctl --master-disable` 等全局关闭系统安全检查的命令。

## 首次配置与 TUN 授权

1. 打开 ZenClash，进入“订阅管理”。
2. 添加在线订阅，或者导入本地 Clash/Mihomo YAML 文件。
3. 返回首页，选择配置和代理节点。
4. 根据需要启用“系统代理”或 TUN。

只有在你明确启用 TUN 时，ZenClash 才会请求 macOS 管理员授权。TUN 需要创建网络接口和路由，因此出现系统密码或 Touch ID 提示属于预期行为。请确认请求来自你刚刚安装的 ZenClash，再批准授权。

## 更新

ZenClash 只会通知有新版本并打开官方 Release 页面，不会静默安装应用更新。更新步骤如下：

1. 退出 ZenClash；更新前先关闭系统代理和 TUN。
2. 下载新版本 DMG 和 `SHA256SUMS`，并重新校验摘要。
3. 将新的 `ZenClash.app` 拖入“应用程序”，确认替换旧版本。
4. 按照“首次打开”一节放行新版本。

应用数据保存在 `~/Library/Application Support/ZenClash`，替换 `/Applications/ZenClash.app` 不会删除已有订阅、设置和本地历史。

## 常见问题

### 强制退出后无法上网

优先重新打开 ZenClash，并在首页关闭系统代理和 TUN，然后正常退出应用。如果已经删除 ZenClash，请打开“系统设置 → 网络 → 当前网络服务 → 详细信息 → 代理”，关闭 ZenClash 曾启用的网页代理、安全网页代理或自动代理配置。

### 安装后提示应用已损坏

1. 删除 `/Applications/ZenClash.app` 和已下载的 DMG。
2. 从官方 Release 重新下载 DMG 与 `SHA256SUMS`。
3. 确认 SHA-256 完全一致。
4. 重新安装并运行前述 `codesign --verify` 命令。

摘要或签名验证失败时，不要使用 `xattr` 强行运行；请在 [GitHub Issues](https://github.com/HaiwenZhang/zenclash/issues) 提交版本号、macOS 版本、Mac 芯片和完整错误信息。

### Intel Mac 无法运行

当前 DMG 仅包含 Apple Silicon `arm64` 版本，Intel Mac 暂不受支持。

## 卸载

1. 在 ZenClash 中关闭系统代理和 TUN。
2. 在应用设置中关闭“登录时启动”。
3. 正常退出 ZenClash。
4. 将 `/Applications/ZenClash.app` 移到废纸篓。

以上操作会保留订阅、设置和历史数据。若要彻底删除这些本地数据，请先确认不再需要任何配置或备份，然后运行：

```sh
rm -f "$HOME/Library/LaunchAgents/dev.zenclash.app.plist"
rm -rf "$HOME/Library/Application Support/ZenClash"
```

第二条命令会永久删除 ZenClash 的订阅、设置、日志、流量历史和受管内核，无法撤销。
