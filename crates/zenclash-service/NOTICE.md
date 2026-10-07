# ZenClash Service — Fork / 修改声明

本目录 Fork 自 https://github.com/clash-verge-rev/clash-verge-service-ipc 。
上游导入版本为 2.7.5；上游 Cargo 清单署名 Tunglies，其他原贡献者的权利和声明保留。
原始 LICENSE 完整保留；本 Fork 及修改按 GNU GPL 第 3 版（GPL-3.0-only）发布。
本软件不提供保证；完整条款见 LICENSE。

**修改者：ZenClash contributors。修改日期：2026-10-04。**
Fork 版本：2.7.5+zenclash.1。

本次修改：

- 增加服务 `--version` 的无副作用版本查询，供构建工具检查产物，避免版本查询误启动后台服务。
- Rust package / library 与服务、安装、卸载可执行文件改为 ZenClash 名称。
- Windows SCM、命名管道、互斥锁和受保护目录，macOS Bundle/launchd/helper 标识，Linux systemd/socket/安装目录，以及开发、测试通道统一为 ZenClash。
- 内核捆绑名称改为本应用使用的 mihomo / mihomo-alpha；Linux 残留服务检测适配 zenclash-service 的 15 字节进程名截断。
- 调整协议头、所有者令牌文件名、测试环境变量和安装器资源品牌；保留上游内核控制 API、协议版本和核心监督实现。
- 移除上游 macOS 安装器对旧 Clash Verge helper 的清理，避免操作其他产品的服务。
- 构建采用 workspace release profile，Makefile 测试限定本服务包；NSIS 模板显示许可证并安装 LICENSE、NOTICE 和 README。
- 添加 README、来源快照校验记录和实际接入边界说明，替换未填写的安全政策模板。

UPSTREAM.json 记录修改前的文件哈希与最初适配的文件清单，不是二进制对应源码本身。没有可验证的上游 Git 提交号，不虚构提交归属。原依赖仓库链接保留，未将第三方依赖伪称为 ZenClash 原创。

后续发布必须随二进制提供可获得的完整对应源码，并保留本声明和 LICENSE；详见 README 的分发章节。

2026-10-05 追加 ZenClash CI 适配：补充公共接口的实际错误说明；原子 JSON 写入的泛型增加 Sync 约束，以满足跨线程异步检查；测试用 watchdog 配置记录 poisoned mutex 的 panic 条件。原服务协议和监督行为未因此改变。

2026-10-07（修改者：ZenClash contributors）：根据用户提供的 debug/clash-verge-service-ipc
源码同步 management::elevate 的原生 Windows ShellExecuteExW 授权、CoreManager 的
Unix SIGTERM 宽限、有界输出 drain 和 IPC Start/Stop 截止预算。保留本产品身份、路径和
原有接口文档；增加 COM/进程 handle 的资源释放与等待错误检查、跨准备子进程的结构化
原生授权错误、停止回归测试。来源文件和 SHA-256 见
../../docs/research/clash-verge-tun-sources.json，研究和验收记录见同目录研究文档。
原作者和 GPL 声明继续保留；本次改动未进入以前生成的安装包。

同日交叉检查修正卸载器残留的 macOS uninstall_old_service 引用；旧函数早已在本 Fork
移除，这次仅删除失效导入与调用，继续只卸载 ZenClash 自己的 helper。
