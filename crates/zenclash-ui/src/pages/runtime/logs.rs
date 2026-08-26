use super::{
    div, empty_state, format_bytes, format_log_entries, h_flex, info_row, metric, px, setting_card,
    setting_switch, v_flex, Button, Context, Disableable, FluentBuilder, IconName, Input,
    InteractiveElement, IntoElement, MihomoLogLevel, Page, ParentElement, PreferencesRestored,
    RuntimePage, Selectable, Sizable, Styled,
};

const MAX_VISIBLE_LOGS: usize = 500;

impl RuntimePage {
    pub(super) fn render_logs(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let all_entries = self.log_monitor.entries();
        let connected = self.log_monitor.connected();
        let persistence = self.log_monitor.persistence_status();
        let query = normalize_log_query(&self.log_filter.read(cx).value());
        let entries = all_entries
            .iter()
            .filter(|entry| log_matches(entry, &query))
            .rev()
            .take(MAX_VISIBLE_LOGS)
            .cloned()
            .collect::<Vec<_>>();
        let entry_count = entries.len();
        v_flex()
            .gap_3()
            .child(render_log_header(
                all_entries.len(),
                entry_count,
                query.is_empty(),
                connected,
                &persistence,
                theme,
            ))
            .child(self.render_log_persistence(theme, cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.log_filter).small()))
                    .child(
                        Button::new("export-logs")
                            .icon(IconName::File)
                            .label("导出")
                            .small()
                            .outline()
                            .disabled(all_entries.is_empty() || self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| this.choose_log_export(cx))),
                    )
                    .child(
                        Button::new("clear-logs")
                            .icon(IconName::Delete)
                            .label("清空")
                            .small()
                            .outline()
                            .disabled(all_entries.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.log_monitor.clear();
                                this.notice = Some("内存日志已清空".into());
                                cx.notify();
                            })),
                    ),
            )
            .child(Self::render_log_entries(
                entries,
                all_entries.is_empty(),
                theme,
            ))
            .into_any_element()
    }

    fn render_log_persistence(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let status = self.log_monitor.persistence_status();
        let path = status.path.as_deref().map_or_else(
            || "应用数据目录不可用".into(),
            |path| path.display().to_string(),
        );
        let state = status.last_error.as_deref().unwrap_or(if status.enabled {
            "新日志正在按接收顺序持续写入"
        } else {
            "内存日志继续接收，磁盘文件暂停写入"
        });

        setting_card("持续日志文件", theme)
            .child(setting_switch(
                "记录到磁盘",
                "写入有界日志文件；达到上限后保留最新内容并继续记录",
                self.preferences.log_file_enabled,
                "logs-file-enabled",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.set_log_file_enabled(*checked, cx);
                }),
            ))
            .child(info_row("文件", &path, theme))
            .child(info_row("写入状态", state, theme))
            .child(info_row(
                "实时采集",
                log_level_description(self.log_monitor.level()),
                theme,
            ))
            .child(info_row("当前占用", &format_log_disk_usage(&status), theme))
            .when(status.dropped_entries > 0, |card| {
                card.child(info_row(
                    "队列丢弃",
                    &format!("{} 条", status.dropped_entries),
                    theme,
                ))
            })
            .child(
                h_flex()
                    .min_h(px(58.))
                    .px_4()
                    .gap_3()
                    .justify_between()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_sm().child("文件上限"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("触顶后回落到约一半容量，避免每条日志都重写文件"),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .children([5_u16, 10, 25, 50].into_iter().enumerate().map(
                                |(index, mebibytes)| {
                                    Button::new(("log-file-limit", index))
                                        .label(format!("{mebibytes} MiB"))
                                        .small()
                                        .outline()
                                        .selected(
                                            self.preferences.log_file_max_mebibytes == mebibytes,
                                        )
                                        .disabled(
                                            !self.preferences.log_file_enabled || self.mutating,
                                        )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.set_log_file_limit(mebibytes, cx);
                                        }))
                                },
                            )),
                    ),
            )
    }

    fn set_log_file_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.persist_log_preferences(
            Some(enabled),
            None,
            if enabled {
                "持续日志文件已启用"
            } else {
                "持续日志文件已暂停"
            },
            cx,
        );
    }

    fn set_log_file_limit(&mut self, mebibytes: u16, cx: &mut Context<Self>) {
        self.persist_log_preferences(None, Some(mebibytes), "日志文件大小上限已保存", cx);
    }

    fn persist_log_preferences(
        &mut self,
        enabled: Option<bool>,
        max_mebibytes: Option<u16>,
        success: &'static str,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.preferences_store.clone() else {
            self.error = Some("应用设置存储不可用；请检查应用数据目录权限".into());
            cx.notify();
            return;
        };
        let log_path = store.log_file_path();
        let Some(token) = self.begin_mutation(Page::Logs) else {
            return;
        };
        let task = self.runtime.spawn_blocking(move || {
            store
                .update(|preferences| {
                    if let Some(enabled) = enabled {
                        preferences.log_file_enabled = enabled;
                    }
                    if let Some(mebibytes) = max_mebibytes {
                        preferences.log_file_max_mebibytes = mebibytes;
                    }
                })
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("日志设置保存任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(preferences) if this.is_page_task_current(token) => {
                        match this.log_monitor.configure_persistence(
                            log_path,
                            preferences.log_file_enabled,
                            preferences.log_file_max_mebibytes,
                        ) {
                            Ok(()) => {
                                this.preferences = preferences.clone();
                                this.notice = Some(success.into());
                                cx.emit(PreferencesRestored { preferences });
                            }
                            Err(error) => this.set_page_error(token, error.to_string()),
                        }
                    }
                    Ok(_) => {}
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_log_entries(
        entries: Vec<zenclash_core::LogEntry>,
        all_empty: bool,
        theme: &gpui_component::Theme,
    ) -> gpui::Div {
        v_flex()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .when(entries.is_empty(), |this| {
                this.child(empty_state(
                    if all_empty {
                        "等待内核日志事件…"
                    } else {
                        "没有匹配的日志"
                    },
                    theme,
                ))
            })
            .children(entries.into_iter().enumerate().map(|(index, entry)| {
                let color = match entry.level.as_str() {
                    "error" => theme.danger,
                    "warning" | "warn" => theme.warning,
                    "debug" => theme.muted_foreground,
                    _ => theme.success,
                };
                h_flex()
                    .id(("log-row", index))
                    .items_start()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .w(px(62.))
                            .text_xs()
                            .text_color(color)
                            .child(entry.level.to_uppercase()),
                    )
                    .child(div().flex_1().text_xs().child(entry.payload))
            }))
    }

    fn choose_log_export(&mut self, cx: &mut Context<Self>) {
        let token = self.page_task_token_for(Page::Logs);
        let directory = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
        let receiver = cx.prompt_for_new_path(&directory, Some("zenclash-mihomo.log"));
        let payload = format_log_entries(&self.log_monitor.entries());
        cx.spawn(async move |this, cx| {
            let selection = receiver.await;
            let _ = this.update(cx, |this, cx| match selection {
                Ok(Ok(Some(path))) if this.is_page_task_current(token) => {
                    this.write_log_export(path, payload, token, cx);
                }
                Ok(Ok(Some(_))) => tracing::info!("discarded log export after leaving logs page"),
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    this.set_page_error(token, format!("无法打开日志保存对话框：{error}"));
                    cx.notify();
                }
                Err(error) => {
                    this.set_page_error(token, format!("日志保存对话框异常结束：{error}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn write_log_export(
        &mut self,
        path: std::path::PathBuf,
        payload: String,
        token: super::PageTaskToken,
        cx: &mut Context<Self>,
    ) {
        let Some(_) = self.begin_mutation(Page::Logs) else {
            return;
        };
        let display_path = path.display().to_string();
        let task = self.runtime.spawn(tokio::fs::write(path, payload));
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("日志导出任务异常结束：{error}"))
                .and_then(|result| result.map_err(|error| format!("写入日志失败：{error}")));
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(()) if this.is_page_task_current(token) => {
                        this.notice = Some(format!("日志已导出到 {display_path}"));
                    }
                    Ok(()) => {}
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

fn render_log_header(
    total_entries: usize,
    filtered_entries: usize,
    query_is_empty: bool,
    connected: bool,
    persistence: &zenclash_core::LogPersistenceStatus,
    theme: &gpui_component::Theme,
) -> gpui::Div {
    let disk_color = if persistence.last_error.is_some() {
        theme.danger
    } else if persistence.enabled {
        theme.success
    } else {
        theme.muted_foreground
    };
    h_flex()
        .justify_between()
        .child(metric(
            if query_is_empty {
                "日志条目"
            } else {
                "过滤结果"
            },
            if query_is_empty {
                total_entries.to_string()
            } else {
                format!("{filtered_entries} / {total_entries}")
            },
            theme.primary,
            theme,
        ))
        .child(metric(
            "磁盘占用",
            if persistence.enabled {
                format_log_disk_usage(persistence)
            } else {
                "已关闭".into()
            },
            disk_color,
            theme,
        ))
        .child(
            h_flex()
                .gap_2()
                .text_xs()
                .text_color(if connected {
                    theme.success
                } else {
                    theme.danger
                })
                .child(div().size_2().rounded_full().bg(if connected {
                    theme.success
                } else {
                    theme.danger
                }))
                .child(if connected {
                    "实时流已连接"
                } else {
                    "正在重连"
                }),
        )
}

fn format_log_disk_usage(persistence: &zenclash_core::LogPersistenceStatus) -> String {
    if persistence.max_bytes == 0 {
        return format_bytes(persistence.size_bytes);
    }
    format!(
        "{} / {}",
        format_bytes(persistence.size_bytes),
        format_bytes(persistence.max_bytes)
    )
}

const fn log_level_description(level: MihomoLogLevel) -> &'static str {
    match level {
        MihomoLogLevel::Silent => "静默 · 不接收内核事件",
        MihomoLogLevel::Error => "错误 · 仅保留故障",
        MihomoLogLevel::Warning => "警告 · 推荐日常精简使用",
        MihomoLogLevel::Info => "信息 · 默认，包含连接与运行事件",
        MihomoLogLevel::Debug => "调试 · 仅排障时临时开启",
    }
}

fn log_matches(entry: &zenclash_core::LogEntry, query: &str) -> bool {
    query.is_empty()
        || entry.level.to_ascii_lowercase().contains(query)
        || entry.payload.to_ascii_lowercase().contains(query)
}

fn normalize_log_query(query: &str) -> String {
    query.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_filter_matches_level_and_payload_case_insensitively() {
        let entry = zenclash_core::LogEntry {
            level: "warning".into(),
            payload: "Proxy connection timeout".into(),
            timestamp_ms: 0,
        };

        assert!(log_matches(&entry, &normalize_log_query("WARN")));
        assert!(log_matches(
            &entry,
            &normalize_log_query(" PROXY connection ")
        ));
        assert!(!log_matches(&entry, "dns"));
    }

    #[test]
    fn disk_usage_always_includes_the_configured_limit() {
        let status = zenclash_core::LogPersistenceStatus {
            size_bytes: 2_621_440,
            max_bytes: 5_242_880,
            ..Default::default()
        };

        assert_eq!(format_log_disk_usage(&status), "2.5 MiB / 5.0 MiB");
    }

    #[test]
    fn log_level_descriptions_distinguish_daily_use_from_diagnostics() {
        assert!(log_level_description(MihomoLogLevel::Warning).contains("推荐"));
        assert!(log_level_description(MihomoLogLevel::Debug).contains("排障"));
    }
}
