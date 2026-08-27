use super::{
    AppContext, Button, ClipboardItem, Context, Disableable, Entity, FluentBuilder, IconName,
    Input, InputEvent, InputState, InteractiveElement, IntoElement, LogTimeSource, MihomoLogLevel,
    Page, ParentElement, PreferencesRestored, RuntimePage, Selectable, Sizable, Styled,
    Subscription, Window, compact_text, div, empty_state, format_bytes, format_log_entries,
    format_log_entries_support_safe, h_flex, info_row, metric, px, setting_card, setting_switch,
    v_flex,
};

const MAX_VISIBLE_LOGS: usize = 500;

pub(super) struct LogUiState {
    pub(super) filter: Entity<InputState>,
}

impl LogUiState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<RuntimePage>) -> (Self, Subscription) {
        let filter = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(zenclash_i18n::text("runtime.placeholders.log_filter"))
        });
        let subscription = cx.subscribe(&filter, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        (Self { filter }, subscription)
    }
}

impl RuntimePage {
    pub(super) fn render_logs(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let all_entries = self.log_monitor.entries();
        let connected = self.log_monitor.connected();
        let persistence = self.log_monitor.persistence_status();
        let query = normalize_log_query(&self.logs.filter.read(cx).value());
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
                    .child(div().flex_1().child(Input::new(&self.logs.filter).small()))
                    .child(
                        Button::new("export-logs")
                            .icon(IconName::File)
                            .label(zenclash_i18n::text("logs.actions.export"))
                            .small()
                            .outline()
                            .disabled(all_entries.is_empty() || self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| this.choose_log_export(cx))),
                    )
                    .child(
                        Button::new("copy-support-safe-logs")
                            .icon(IconName::Copy)
                            .label(zenclash_i18n::text("logs.actions.copy_safe"))
                            .small()
                            .outline()
                            .disabled(all_entries.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let payload =
                                    format_log_entries_support_safe(&this.log_monitor.entries());
                                cx.write_to_clipboard(ClipboardItem::new_string(payload));
                                this.notice = Some(zenclash_i18n::text("logs.notices.safe_copied"));
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("clear-logs")
                            .icon(IconName::Delete)
                            .label(zenclash_i18n::text("logs.actions.clear"))
                            .small()
                            .outline()
                            .disabled(all_entries.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.log_monitor.clear();
                                this.notice = Some(zenclash_i18n::text("logs.notices.cleared"));
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
            || zenclash_i18n::text("logs.persistence.data_directory_unavailable"),
            |path| path.display().to_string(),
        );
        let state = status.last_error.clone().unwrap_or_else(|| {
            if status.enabled {
                zenclash_i18n::text("logs.persistence.writing")
            } else {
                zenclash_i18n::text("logs.persistence.paused")
            }
        });

        setting_card(zenclash_i18n::text("logs.persistence.title"), theme)
            .child(setting_switch(
                zenclash_i18n::text("logs.persistence.enabled.title"),
                zenclash_i18n::text("logs.persistence.enabled.description"),
                self.preferences.log_file_enabled,
                "logs-file-enabled",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.set_log_file_enabled(*checked, cx);
                }),
            ))
            .child(info_row(
                zenclash_i18n::text("logs.persistence.file"),
                &path,
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("logs.persistence.state"),
                state,
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("logs.persistence.capture"),
                log_level_description(self.log_monitor.level()),
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("logs.persistence.usage"),
                format_log_disk_usage(&status),
                theme,
            ))
            .when(status.dropped_entries > 0, |card| {
                card.child(info_row(
                    zenclash_i18n::text("logs.persistence.dropped"),
                    zenclash_i18n::text_with(
                        "logs.persistence.dropped_count",
                        &[("count", status.dropped_entries.to_string())],
                    ),
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
                            .child(
                                div()
                                    .text_sm()
                                    .child(zenclash_i18n::text("logs.persistence.limit")),
                            )
                            .child(
                                div().text_xs().text_color(theme.muted_foreground).child(
                                    zenclash_i18n::text("logs.persistence.limit_description"),
                                ),
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
                zenclash_i18n::text("logs.notices.persistence_enabled")
            } else {
                zenclash_i18n::text("logs.notices.persistence_paused")
            },
            cx,
        );
    }

    fn set_log_file_limit(&mut self, mebibytes: u16, cx: &mut Context<Self>) {
        self.persist_log_preferences(
            None,
            Some(mebibytes),
            zenclash_i18n::text("logs.notices.limit_saved"),
            cx,
        );
    }

    fn persist_log_preferences(
        &mut self,
        enabled: Option<bool>,
        max_mebibytes: Option<u16>,
        success: String,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.preferences_store.clone() else {
            self.error = Some(zenclash_i18n::text("logs.errors.preferences_unavailable"));
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
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "logs.errors.preferences_task",
                        &[("error", error.to_string())],
                    )
                })
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
                                this.notice = Some(success);
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
                        zenclash_i18n::text("logs.empty.waiting")
                    } else {
                        zenclash_i18n::text("logs.empty.filtered")
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
                let time_source = match entry.time_source {
                    LogTimeSource::Core => zenclash_i18n::text("logs.time.core"),
                    LogTimeSource::LocalReceive => zenclash_i18n::text("logs.time.local_receive"),
                };
                let time = entry
                    .core_time
                    .clone()
                    .unwrap_or_else(|| entry.timestamp_ms.to_string());
                let fields = (!entry.fields.is_null()).then(|| {
                    compact_text(
                        &serde_json::to_string(&entry.fields).unwrap_or_else(|_| "{}".into()),
                        180,
                    )
                });
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
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(div().text_xs().child(entry.payload))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{time} · {time_source}")),
                            )
                            .when_some(fields, |this, fields| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .font_family(theme.mono_font_family.clone())
                                        .text_color(theme.muted_foreground)
                                        .child(fields),
                                )
                            }),
                    )
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
                    this.set_page_error(
                        token,
                        zenclash_i18n::text_with(
                            "logs.errors.export_dialog",
                            &[("error", error.to_string())],
                        ),
                    );
                    cx.notify();
                }
                Err(error) => {
                    this.set_page_error(
                        token,
                        zenclash_i18n::text_with(
                            "logs.errors.export_dialog_task",
                            &[("error", error.to_string())],
                        ),
                    );
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
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "logs.errors.export_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| {
                    result.map_err(|error| {
                        zenclash_i18n::text_with(
                            "logs.errors.write",
                            &[("error", error.to_string())],
                        )
                    })
                });
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(()) if this.is_page_task_current(token) => {
                        this.notice = Some(zenclash_i18n::text_with(
                            "logs.notices.exported",
                            &[("path", display_path)],
                        ));
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
                zenclash_i18n::text("logs.metrics.entries")
            } else {
                zenclash_i18n::text("logs.metrics.filtered")
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
            zenclash_i18n::text("logs.metrics.disk_usage"),
            if persistence.enabled {
                format_log_disk_usage(persistence)
            } else {
                zenclash_i18n::text("logs.metrics.disabled")
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
                    zenclash_i18n::text("logs.stream.connected")
                } else {
                    zenclash_i18n::text("logs.stream.reconnecting")
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

fn log_level_description(level: MihomoLogLevel) -> String {
    match level {
        MihomoLogLevel::Silent => zenclash_i18n::text("logs.levels.silent"),
        MihomoLogLevel::Error => zenclash_i18n::text("logs.levels.error"),
        MihomoLogLevel::Warning => zenclash_i18n::text("logs.levels.warning"),
        MihomoLogLevel::Info => zenclash_i18n::text("logs.levels.info"),
        MihomoLogLevel::Debug => zenclash_i18n::text("logs.levels.debug"),
    }
}

fn log_matches(entry: &zenclash_core::LogEntry, query: &str) -> bool {
    query.is_empty()
        || entry.level.to_ascii_lowercase().contains(query)
        || entry.payload.to_ascii_lowercase().contains(query)
        || entry.fields.as_object().is_some_and(|fields| {
            fields.iter().any(|(key, value)| {
                key.to_ascii_lowercase().contains(query)
                    || value.to_string().to_ascii_lowercase().contains(query)
            })
        })
        || (!entry.fields.is_object()
            && entry
                .fields
                .to_string()
                .to_ascii_lowercase()
                .contains(query))
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
            ..zenclash_core::LogEntry::default()
        };

        assert!(log_matches(&entry, &normalize_log_query("WARN")));
        assert!(log_matches(
            &entry,
            &normalize_log_query(" PROXY connection ")
        ));
        assert!(!log_matches(&entry, "dns"));
    }

    #[test]
    fn log_filter_matches_structured_field_names_and_values() {
        let entry = zenclash_core::LogEntry {
            fields: serde_json::json!({"network": "tcp"}),
            ..zenclash_core::LogEntry::default()
        };

        assert!(log_matches(&entry, "network"));
        assert!(log_matches(&entry, "tcp"));
        assert!(!log_matches(&entry, "udp"));
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
        let warning = log_level_description(MihomoLogLevel::Warning);
        let debug = log_level_description(MihomoLogLevel::Debug);
        assert!(!warning.is_empty());
        assert!(!debug.is_empty());
        assert_ne!(warning, debug);
    }
}
