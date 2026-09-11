# 代理组闲置卡顿排查记录

## 问题与结论边界

用户报告：开启系统代理、切到其他应用几分钟后，首次进入代理组会卡十几秒；恢复后连续切换正常。macOS 和 Windows 均出现。

2026-09-11 的本机测试尚未复现这次十几秒停顿，不能认定根因，也没有据此修改生产代码。此前 macOS 采样中的 `CAMetalLayer::nextDrawable` 等待只是一条平台线索，不能解释 Windows，也不能单独证明本次故障的原因。

## 已执行的测试

平台：macOS 26.6.2 / ARM64。源码基线：`abea97e`。

| 测试 | 结果 | 能说明什么 |
| --- | --- | --- |
| 当前源码代理组单元测试 | 15 个通过 | 分页、选择状态、过期任务等已有行为未失败 |
| 当前源码生命周期单元测试 | 6 个通过 | 暂停期间累计 revision 的同步等已有行为未失败 |
| 真实 GPUI 独立窗口，20 组、500 节点，立即响应，两次加载 | 50 ms UI 定时回调最大间隔 53 ms | 此数据规模未造成可观测的长时间主线程阻塞 |
| 同一窗口，每次接口响应延迟 15 秒，间隔约三分钟再次加载 | UI 定时回调最大间隔 69 ms | 此条件下等待接口不会同步阻塞 UI |
| 已安装应用，系统代理开启、TUN 关闭，Finder 置前约三分钟后激活并进入代理组 | 页面切换完成，未观察到十几秒卡顿 | 本次实机尝试未复现；不能排除间歇故障 |

安装包的文件修改时间为 2026-08-28，采样元数据显示 0.1.0；仓库是 0.1.1，且存在 8 月 29 日的后台刷新与内存优化提交。没有安装包的提交标识，不能断言其精确源码版本。源码测试和安装包实机测试须分别理解。

UI 回调间隔不是帧率、单帧耗时或鼠标输入延迟；上述数值包含定时器正常等待时间。独立窗口使用真实 `ProxiesPage` 和本机生成的 HTTP 响应，不启动内核、不加载订阅、不改变系统代理。它不包含首页、托盘、日志和进程管理，也不验证真实内核行为。定时回调在两次加载之间仍持续运行，因此这不是操作系统闲置/睡眠恢复测试。

## 重复运行独立窗口测试

探针源码：[`scripts/diagnostics/proxies_responsiveness.rs`](../../scripts/diagnostics/proxies_responsiveness.rs)。不添加依赖；复制到 Cargo 的 examples 目录运行，退出时自动关闭测试窗口。

macOS / Linux，从仓库根目录执行（Linux 尚未实测）：

```sh
mkdir -p crates/zenclash-ui/examples
cp scripts/diagnostics/proxies_responsiveness.rs crates/zenclash-ui/examples/proxies_responsiveness.rs
cargo run -p zenclash-ui --example proxies_responsiveness --locked -- 20 500 15 180
```

Windows PowerShell，从仓库根目录执行（Windows 尚未实测）：

```powershell
New-Item -ItemType Directory -Force crates/zenclash-ui/examples
Copy-Item scripts/diagnostics/proxies_responsiveness.rs crates/zenclash-ui/examples/proxies_responsiveness.rs
cargo run -p zenclash-ui --example proxies_responsiveness --locked -- 20 500 15 180
```

四个参数依次为代理组数、独立节点数、HTTP 响应延迟秒数、两次加载之间的额外间隔秒数。每组引用全部节点。延迟应小于客户端的 30 秒请求超时；否则测试的是错误状态。`fixture_response` 记录响应发送，`ui_gap_ms` 记录超过 250 ms 的 UI 回调间隔，`probe_complete` 输出整轮最大间隔。它是诊断探针，不会把超过阈值的结果自动转换成测试失败。

生成的 examples 文件被仓库现有 `.gitignore` 忽略，使用结束后可删除该生成文件。

## 尚需验证

- 故障实际发生时的主线程调用栈，以及当时的 CPU、内存和控制器响应时间。
- Windows 上使用相同条件、相同源码版本的实机结果；当前环境只有 macOS。
- 完整应用在实际代理流量下的后台活动与页面切换是否相关。单独给接口增加延迟不能代替这一测试。

在取得故障时证据前，不把节点数量、共享锁、Metal 或某个旧版刷新逻辑定为根因。
