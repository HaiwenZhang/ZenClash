# Mihomo 客户端行为验收映射

> 更新日期：2026-08-27
>
> 对应清单：[`mihomo-client-code-design-comparison.md` 第 13 节](../research/mihomo-client-code-design-comparison.md#13-行为测试清单)
>
> 最近自动门禁：418 项通过，4 项按真实内核/在线资源约定忽略；Mihomo v1.19.30 真实
> harness 另行通过。macOS ad-hoc 签名实包通过 `codesign --verify --deep --strict`。

本文记录 44 项研究行为如何被可执行测试或实包步骤证明。测试名均省略 crate 前缀；
`real_mihomo::drives_the_supplied_profile_through_a_real_mihomo_process` 是 Mihomo v1.19.30
真实进程 harness，不由 mock 替代。平台原生 System Proxy/TUN 的发布矩阵单列在文末，
不能用 adapter 测试冒充真实 OS 验收。可重复步骤、原生 readback 与清理要求见
[`mihomo-client-platform-acceptance.md`](mihomo-client-platform-acceptance.md)。

## 生命周期与 Controller

| # | 自动化证据 | 结论 |
|---:|---|---|
| 1 | `core_session::concurrent_restarts_serialize_without_overlapping_children` | 并发维护经同一 transition 锁串行，generation 单调递增，子进程 guard 不重叠。 |
| 2 | `core_session::shutdown_cancels_a_restart_waiter_and_prevents_a_late_child` | quit 会取消 readiness waiter；shutdown 后拒绝新维护且不产生迟到子进程。 |
| 3 | `process::readiness_timeout_stops_and_reaps_the_unconfirmed_child`、`process::async_restart_rejection_does_not_stop_the_running_child` | 未 ready 候选被有界停止回收；预检拒绝不破坏仍运行的旧进程。 |
| 4 | `core_session::unexpected_exit_retries_are_bounded_and_end_in_visible_failure`、`manual_recovery_after_exhaustion_rearms_the_supervisor`、`home::process_recovery_attempts_have_a_visible_non_color_label` | 退出原因、恢复次数和耗尽后的 failed 都可观察；耗尽后停止自动重试但保持观察，手动恢复会重新启用下一次崩溃监管。 |
| 5 | `core_session::uncertain_external_mihomo_apply_never_becomes_an_owned_restart`、`core_session::external_meow_never_fakes_a_restart_capability` | 外部 core 的不确定结果不会获得 restart/kill 所有权。 |
| 6 | `traffic::late_frame_from_an_old_generation_is_rejected`、`logs::late_log_frame_from_an_old_generation_is_rejected`、`operational_status::async_result_from_an_old_generation_is_rejected` | 旧 generation 的流或异步结果不能写入新会话；断线转 stale 而非伪造 fresh。 |

## Profile 与配置事务

| # | 自动化证据 | 结论 |
|---:|---|---|
| 7 | `profiles::application::final_merged_candidate_is_validated_before_runtime_or_persistence` | 最终合并候选必须先通过目标内核验证，旧 source/active/runtime 保持不变。 |
| 8 | `rejected_active_remote_update_preserves_the_downloaded_source_lkg`、`rejected_active_yaml_edit_preserves_the_source_and_active_revision`、`rejected_local_import_never_commits_the_new_source` | 远程、本地和编辑入口共享 last-known-good 语义。 |
| 9 | `profiles::application::fallback_download_still_requires_final_runtime_acceptance` | 备用下载不能绕过解析、组合、验证和运行时接受。 |
| 10 | `profiles::application::override_parse_failure_only_removes_the_staging_candidate` | 解析失败只清 staging，不触碰 live 文件。 |
| 11 | `interrupted_runtime_response_is_reported_as_unknown_without_source_commit`、`runtime_rejection_leaves_the_previous_active_profile_untouched`、`active_profile_is_not_committed_before_runtime_accepts_the_candidate` | 明确拒绝、传输不确定和确认成功使用不同 outcome/恢复路径。 |
| 12 | 实包表单复验；实现位于 `profiles/actions.rs::add_remote_profile` 与 `profiles/view/forms.rs` | 失败保留名称、URL、User-Agent、Authorization 和 route；pending 结束，错误留在订阅字段组内。 |

## System Proxy、TUN 与四层状态

| # | 自动化证据 | 结论 |
|---:|---|---|
| 13 | `operational_status::traffic_activity_does_not_imply_capture` | traffic WS 活跃不能推导为已接管。 |
| 14 | `home::system_proxy_intent_actual_and_lost_ownership_have_distinct_copy`、`system_proxy::ownership_requires_the_complete_native_state_to_still_match` | intent on、actual off、外部启用和 ownership lost 分开表达。 |
| 15 | `traffic_capture::exit_does_not_overwrite_an_external_system_proxy_replacement` | 外部覆盖后退出不覆盖第三方新值。 |
| 16 | `traffic_capture::system_proxy_failure_restores_the_previous_tun_state`、`traffic_capture::failed_rollback_exposes_reconcile_needed` | 部分失败会回滚；回滚失败显式进入 reconcile-needed。 |
| 17 | `operational_status::configured_tun_requires_permission_device_and_route_evidence`、`configured_tun_is_unknown_until_runtime_activation_is_observed` | configured on 不能冒充 runtime Active。 |
| 18 | `operational_status::first_run_resumes_from_domain_facts_without_a_persisted_wizard_step`、`home::captured_but_failed_path_uses_the_explicit_path_failure_copy` | L3 成功且 L4 失败时明确保留“已接管，但目标路径未通过”。 |
| 19 | `operational_status::failure_preserves_last_successful_value_and_timestamp_as_stale`、`loader::dashboard_keeps_successful_slices_when_connections_fail` | 分片独立失败；旧值与 observed time 保留。 |

## 代理组、测速与连接

| # | 自动化证据 | 结论 |
|---:|---|---|
| 20 | `proxy_operations::visible_catalog_filters_hidden_groups_by_default`、`advanced_catalog_preserves_hidden_groups` | hidden 默认过滤，高级请求可见。 |
| 21 | `proxy::selector_group_has_explicit_selection_behavior`、`real_mihomo` | Selector PUT 后读取 `now`，不提供恢复自动。 |
| 22 | `proxy_operations::restore_auto_deletes_group_and_reads_back_automatic_state`、`proxy::automatic_group_accepts_real_mihomo_fixed_member_encoding`、`real_mihomo` | 自动组 fixed 与 DELETE 恢复自动均按真实编码读回。 |
| 23 | `proxies::load_balance_group_has_no_manual_selection_or_unique_current`、`home::load_balance_summary_does_not_offer_a_fake_manual_selection` | LoadBalance 不伪造唯一当前节点或普通手选。 |
| 24 | `proxy_operations::individual_measurement_never_calls_the_group_restore_endpoint` | 逐节点/provider 测速只访问 delay/healthcheck，不触发 group restore。 |
| 25 | `proxy_operations::group_measurement_names_restore_side_effect_and_reads_back_state` | 显式 group delay 操作单独命名，并在副作用后读回 fixed/now。 |
| 26 | `proxy_operations::keep_existing_selection_does_not_close_connections`、`real_mihomo` | 普通切换不发送 `DELETE /connections`。 |
| 27 | `proxy_operations::rebuild_affected_closes_only_chains_containing_the_previous_member` | affected 只关闭含旧节点的 chain；全量重建不作为普通入口暴露。 |
| 28 | `proxy_operations::rebuild_all_selection_survives_cleanup_failure_and_uses_readback_truth` | 选择已成功但清理失败返回 warning，不谎报选择失败。 |

## DNS、日志、诊断与支持包

| # | 自动化证据 | 结论 |
|---:|---|---|
| 29 | `client::dns_a_and_aaaa_queries_preserve_independent_answers`、`network_diagnostics::every_step_retains_independent_status_time_and_route` | A/AAAA 独立保存 status、answer、TTL 与错误。 |
| 30 | `client::dns_and_fake_ip_cache_flushes_use_separate_endpoints` | DNS 与 fake-IP flush 是两个独立请求；UI 使用各自确认和反馈。 |
| 31 | `logs::structured_log_preserves_core_time_level_message_and_fields`、`structured_log_preserves_array_shaped_mihomo_fields`、`stream_interruption_preserves_last_entry_time_and_format` | structured-first，普通回退标记时间来源且保留旧数据。 |
| 32 | `traffic::disconnect_preserves_last_successful_rates_and_timestamp`、`logs::stream_interruption_preserves_last_entry_time_and_format`、`state::dashboard_failure_keeps_only_the_affected_last_successful_slice` | 三类流失败都不以零值冒充 fresh。 |
| 33 | `network_diagnostics::every_step_retains_independent_status_time_and_route` | 八个步骤独立成功/失败，并显式标注 Mihomo/DIRECT route。 |
| 34 | `network_diagnostics::support_bundle_uses_only_allow_listed_facts`、`logs::support_safe_log_copy_omits_messages_fields_and_core_time` | 支持输出使用 allow-list，不包含 secret、token、凭据、原始路径或敏感日志字段。 |

## 首次使用、空状态与可访问性

| # | 自动化/实包证据 | 结论 |
|---:|---|---|
| 35 | 首页空状态实包复验；`OperationalSnapshot::first_run_stage` | 无 Profile 时显示可聚焦“导入订阅”和“选择本地 YAML”。 |
| 36 | `operational_status::first_run_resumes_from_domain_facts_without_a_persisted_wizard_step`、`old_generation_path_probe_cannot_advance_first_run_state` | 重开后从领域事实续接，旧 probe 不推进新流程。 |
| 37 | macOS 实包 Tab/Enter 复验；一级入口及主动作均使用 gpui-component `Button`/`Input` | 主路径不依赖鼠标、hover 或右键。 |
| 38 | macOS 实包焦点环与文件选择器 Escape 复验；`choose_profile` 保存并恢复触发焦点 | 焦点顺序跟随视觉顺序，对话框关闭回到触发控件。 |
| 39 | 实包错误卡复验；恢复动作均为带可见 label 的 `Button` | 错误恢复动作具有可见 accessible name，不只依赖颜色。 |
| 40 | `traffic::every_traffic_dimension_has_a_distinct_user_label`、`traffic::historical_data_becomes_stale_instead_of_disappearing_after_failure`、首页图表系列测试 | 上传/下载、Fresh/Stale/Failed 同时使用文字、数值和独立系列。 |

## 更新与高权限边界

| # | 自动化证据 | 结论 |
|---:|---|---|
| 41 | `core_update::releases_without_github_digest_are_not_offered`、`rejects_checksum_mismatch`、`validates_release_digest_and_platform_asset_name` | 缺失/错误摘要会拒绝候选，旧 core 不受影响。 |
| 42 | `core_update::release_download_activation_and_rollback_are_transactional`、`abandoned_staging_and_failed_activation_leave_the_old_core_intact` | 替换/启动失败回滚，staging、backup 和拒绝候选最终清理。 |
| 43 | `traffic_capture::permission_prompt_is_reachable_only_from_an_explicit_tun_plan`、`rejected_permission_performs_no_capture_write` | 提权只从用户显式 TUN plan 进入；GUI 不以管理员身份常驻。 |
| 44 | `app_update::external_links_require_https_without_credentials`、`untrusted_release_page_is_rejected`、支持包与安全日志复制测试 | 外链仅允许批准的无凭据 HTTPS；诊断复制默认脱敏。 |

## 真实平台发布矩阵

自动 adapter 与真实 Mihomo harness 已证明排序、回滚、generation 和协议语义，但以下项目仍须
在对应发布主机上执行，记录 OS 版本、应用构建、操作步骤、实际 readback 和退出后的系统状态：

| 场景 | macOS | Windows | Linux |
|---|---|---|---|
| System Proxy enable/readback/release | 待用户授权实测 | 待目标机 | 待目标机 |
| ownership 被外部覆盖 | 待用户授权实测 | 待目标机 | 待目标机 |
| TUN 权限拒绝/授权/重启 | 拒绝路径已实包复验；授权路径待用户授权 | 待目标机；当前应验证安全 Unsupported | 待目标机 |
| core 崩溃与应用退出 | 签名实包已验证：终止直属 core 后生成新 PID，显示“L1 内核已恢复（1 次）”与退出原因；正常退出同步回收应用和新 core | 待目标机 | 待目标机 |
| 路径探测明确经 Mihomo | 真实 Mihomo harness 已通过；系统接管组合待实测 | 待目标机 | 待目标机 |
| 安装包升级回归 | 发布前 | 发布前 | 发布前 |
