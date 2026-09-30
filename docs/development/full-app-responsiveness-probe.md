# 完整应用响应性探针

[`full_app_probe.py`](../../scripts/diagnostics/full_app_probe.py) 在隔离源码和数据目录运行完整 `ZenClashApp` 与真实 Mihomo。脚本和 [GPUI 诊断驱动](../../scripts/diagnostics/full_app_probe.rs) 已入库，仅支持 macOS；使用 Python 3.12 或更新版本，无第三方 Python 依赖。

## 复现

从工作区根目录运行：

```sh
python3 -m unittest discover -s scripts/diagnostics -p test_full_app_probe.py
python3 scripts/diagnostics/full_app_probe.py --run \
  --core-binary /Applications/ZenClash.app/Contents/Resources/mihomo
```

默认使用 release、500 个 direct 节点、20 个 select 组、10,001 条规则，隐藏窗口 180 秒。只准备源码副本和夹具时省略 `--run`。本地默认工具链无法编译依赖时，可显式指定已安装的工具链，例如 `--cargo-toolchain 1.95.0`；这不会改变项目清单或安装工具链。构建使用 `--locked --offline`，依赖须已在本机缓存。

脚本输出临时根目录和结果路径。保留其中的 `probe.json`、`build.log`、`run.log`、`result.json` 与源码副本；失败返回非零，构建错误见 `build.log`。不要把只有准备成功的输出当成完整应用运行通过。

同条件对照可运行：

```sh
python3 scripts/diagnostics/full_app_probe.py --run --baseline --idle-seconds 3
python3 scripts/diagnostics/full_app_probe.py --run --idle-seconds 3
```

`--baseline` 从跟踪的 `HEAD` 复制源码，普通运行复制当前工作区；结果记录数据规模、平台、架构和构建模式。比较前确认两次的内核版本、窗口、节点、规则和观察时长一致。短隐藏场景不能替代三分钟闲置场景。

原生交互可添加 `--interactive-seconds 180 --interactive-page rules`；支持 `proxies`、`profiles`、`override`、`traffic`、`rules`、`connections`、`logs`、`mihomo`。自动导航结束后停留，随后调用生产退出流程。这提供交互窗口，不能自动证明鼠标、键盘或视觉验收通过。

## 隔离和验证条件

- 复制 `crates`、`platforms`、Cargo 清单和锁文件；不修改工作区源代码或 HOME。
- 核对五处 macOS 默认数据路径，每处只替换一次；源码不匹配立即失败，不回退到用户目录。
- 副本可执行文件单独命名并放在临时 `.app` 中；注入导航、计时和 render 计数，扩大必要的窗口方法模块可见性。所有诊断变化只在副本内。
- 配置、配置索引、偏好、流量数据库、内核工作目录均使用新临时目录；不读取私人订阅，不继承 `ZENCLASH_*` 环境。后台使用应用实际客户端验证内核数据，不打印随机控制器密钥。
- 诊断偏好关闭系统代理、无所有权凭据，夹具关闭 TUN。仍会读取原生系统状态，但不会接管或清除主机已有代理。
- 控制器回读必须确认生成节点和规则实际加载；两轮共 20 次导航，包含本地配置、覆写、流量和规则页。隐藏/恢复使用生产窗口逻辑，结束使用生产 `begin_quit`。
- 启动器只检查本次应用的子进程，分别采样 UI 与内核 RSS。正常退出后若仍有已观察到的子进程，结果判失败并清理这些测试进程。

动态端口选择与应用启动之间存在短暂窗口，端口被抢占会令回读失败，不能视为运行通过。系统代理故障注入通过可替换原生后端与真实 PAC 回环服务自动测试完成；该探针不主动改变主机代理或 TUN。

## 指标解释

- `navigation.microseconds`：生产导航调用的同步耗时，不包含页面后台加载完成。
- `timer_gaps.microseconds`：每页一秒观察段内 20 次定时回调间隔，包含正常等待的 50 ms；不是帧耗时或输入呈现延迟。
- `hidden_finished.renders`：隐藏期间 `RuntimePage` 的渲染次数，覆盖诊断注入位置，不代表全应用或内核停止工作。
- `samples.ui_rss_kib` 与 `core_rss_kib`：macOS 进程 RSS，分别报告；不是 Rust 堆或图形资源的分项。无法唯一识别子进程时内核值为空，不记为零。
- `passed`：实际夹具加载、导航记录齐全、正常退出码、退出记录及无已观察到的子进程残留均满足。

使用 release 结果讨论面向用户的性能，统计口径至少给出样本数、p50、p95、最大值；一次读数或 debug 构建不能支持性能提升结论。历史测量和旧脚本未保存的高级场景见 [历史记录](full-app-probe-history.md)，不计作当前验收。
