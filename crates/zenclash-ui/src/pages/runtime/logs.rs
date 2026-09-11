use std::sync::Arc;

use super::{
    AppContext, Button, ClipboardItem, Context, Disableable, Entity, FluentBuilder, IconName,
    Input, InputEvent, InputState, InteractiveElement, IntoElement, LogTimeSource, MihomoLogLevel,
    Page, ParentElement, RuntimePage, Selectable, Sizable, Styled, Subscription, Window,
    compact_text, contains_ascii_case_insensitive, div, empty_state, format_bytes,
    format_log_entries, format_log_entries_support_safe, h_flex, info_row, list_page, metric,
    pagination_summary, px, setting_card, v_flex,
};

const LOGS_PER_PAGE: usize = 100;

pub(super) struct LogUiState {
    pub(super) filter: Entity<InputState>,
    pub(super) page: usize,
    presentation: LogPresentation,
    copying: bool,
    exporting: bool,
}

#[derive(Default)]
struct LogPresentation {
    revision: Option<u64>,
    query: String,
    entries: Vec<Arc<zenclash_core::LogEntry>>,
    matches: Vec<usize>,
}

impl LogPresentation {
    fn refresh(
        &mut self,
        revision: u64,
        query: String,
        snapshot: impl FnOnce() -> Vec<Arc<zenclash_core::LogEntry>>,
    ) {
        let changed = self.revision != Some(revision);
        if changed {
            self.entries = snapshot();
            self.revision = Some(revision);
        }
        if changed || self.query != query {
            self.query = query;
            self.matches = self
                .entries
                .iter()
                .enumerate()
                .rev()
                .filter(|(_, entry)| log_matches(entry, &self.query))
                .map(|(index, _)| index)
                .collect();
        }
    }
}

impl LogUiState {
    pub(super) fn release_results(&mut self) {
        self.presentation = LogPresentation::default();
    }

    pub(super) fn new(window: &mut Window, cx: &mut Context<RuntimePage>) -> (Self, Subscription) {
        let filter = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(zenclash_i18n::text("runtime.placeholders.log_filter"))
        });
        let subscription = cx.subscribe(&filter, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.logs.page = 0;
                cx.notify();
            }
        });
        (
            Self {
                filter,
                page: 0,
                presentation: LogPresentation::default(),
                copying: false,
                exporting: false,
            },
            subscription,
        )
    }
}

impl RuntimePage {
    pub(super) fn render_logs(
        &mut self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let query = normalize_log_query(&self.logs.filter.read(cx).value());
        self.logs
            .presentation
            .refresh(self.log_monitor.revision(), query.clone(), || {
                self.log_monitor.shared_entries()
            });
        let all_entries = &self.logs.presentation.entries;
        let connected = self.log_monitor.connected();
        let persistence = self.log_monitor.persistence_status();
        let filtered_count = self.logs.presentation.matches.len();
        let page = list_page(filtered_count, self.logs.page, LOGS_PER_PAGE);
        let entries = self.logs.presentation.matches[page.start..page.end]
            .iter()
            .map(|&index| all_entries[index].as_ref())
            .collect::<Vec<_>>();
        let previous_page = page.index.saturating_sub(1);
        let next_page = page.index + 1;
        v_flex()
            .gap_3()
            .child(render_log_header(
                all_entries.len(),
                filtered_count,
                query.is_empty(),
                connected,
                &persistence,
                theme,
            ))
            .when(!self.remote, |view| {
                view.child(self.render_log_persistence(theme, cx))
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.logs.filter).small()))
                    .child(
                        Button::new("export-logs")
                            .icon(crate::assets::AppIcon::SquareArrowRightExit)
                            .label(zenclash_i18n::text("logs.actions.export"))
                            .small()
                            .outline()
                            .loading(self.logs.exporting)
                            .disabled(all_entries.is_empty() || self.logs.exporting)
                            .on_click(cx.listener(|this, _, _, cx| this.choose_log_export(cx))),
                    )
                    .child(
                        Button::new("copy-support-safe-logs")
                            .icon(IconName::Copy)
                            .label(zenclash_i18n::text("logs.actions.copy_safe"))
                            .small()
                            .outline()
                            .loading(self.logs.copying)
                            .disabled(all_entries.is_empty() || self.logs.copying)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.copy_support_safe_logs(cx)),
                            ),
                    )
                    .child(
                        Button::new("clear-logs")
                            .icon(IconName::Globe)
                            .label(zenclash_i18n::text("logs.actions.clear"))
                            .small()
                            .outline()
                            .disabled(all_entries.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.log_monitor.clear();
                                this.logs.page = 0;
                                this.notice = Some(zenclash_i18n::text("logs.notices.cleared"));
                                cx.notify();
                            })),
                    ),
            )
            .child(Self::render_log_entries(
                &entries,
                all_entries.is_empty(),
                page.start,
                theme,
            ))
            .when(page.count > 1, |this| {
                this.child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(pagination_summary(page, filtered_count)),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("previous-logs-page")
                                        .icon(IconName::ChevronLeft)
                                        .label(zenclash_i18n::text("common.actions.previous_page"))
                                        .small()
                                        .outline()
                                        .disabled(page.index == 0)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.set_logs_page(previous_page, cx);
                                        })),
                                )
                                .child(
                                    Button::new("next-logs-page")
                                        .icon(IconName::ChevronRight)
                                        .label(zenclash_i18n::text("common.actions.next_page"))
                                        .small()
                                        .outline()
                                        .disabled(page.index + 1 >= page.count)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.set_logs_page(next_page, cx);
                                        })),
                                ),
                        ),
                )
            })
            .into_any_element()
    }

    fn set_logs_page(&mut self, page: usize, cx: &mut Context<Self>) {
        self.logs.page = page;
        cx.notify();
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
            .child(crate::pages::runtime::common::setting_switch_disabled(
                zenclash_i18n::text("logs.persistence.enabled.title"),
                zenclash_i18n::text("logs.persistence.enabled.description"),
                self.preferences.log_file_enabled,
                "logs-file-enabled",
                theme,
                self.mutation_busy(crate::pages::runtime::busy::MutationDomain::Logs),
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
                                    .selected(self.preferences.log_file_max_mebibytes == mebibytes)
                                    .disabled(
                                        !self.preferences.log_file_enabled
                                            || self.mutation_busy(
                                                crate::pages::runtime::busy::MutationDomain::Logs,
                                            ),
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
        let Some(token) = self.begin_scoped_mutation(
            Page::Logs,
            crate::pages::runtime::busy::MutationDomain::Logs,
        ) else {
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
                this.finish_mutation(token);
                match result {
                    Ok(preferences) => {
                        match this.log_monitor.configure_persistence(
                            log_path,
                            preferences.log_file_enabled,
                            preferences.log_file_max_mebibytes,
                        ) {
                            Ok(()) => {
                                if this.is_page_task_current(token) {
                                    this.notice = Some(success);
                                }
                                this.accept_preferences(
                                    preferences,
                                    crate::pages::runtime::PreferenceScope::Logs,
                                    cx,
                                );
                            }
                            Err(error) => this.set_page_error(token, error.to_string()),
                        }
                    }
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_log_entries(
        entries: &[&zenclash_core::LogEntry],
        all_empty: bool,
        page_start: usize,
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
            .children(entries.iter().enumerate().map(|(offset, entry)| {
                let index = page_start + offset;
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
                            .child(div().text_xs().child(entry.payload.clone()))
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

    fn copy_support_safe_logs(&mut self, cx: &mut Context<Self>) {
        if self.logs.copying {
            return;
        }
        self.logs.copying = true;
        let token = self.page_task_token_for(Page::Logs);
        let entries = self.log_monitor.shared_entries();
        let task = self
            .runtime
            .spawn_blocking(move || prepare_log_payload(entries, true));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.logs.copying = false;
                match result {
                    Ok(payload) => {
                        cx.write_to_clipboard(ClipboardItem::new_string(payload));
                        if this.is_page_task_current(token) {
                            this.notice = Some(zenclash_i18n::text("logs.notices.safe_copied"));
                        }
                    }
                    Err(error) => this.set_page_error(
                        token,
                        zenclash_i18n::text_with(
                            "logs.errors.copy_task",
                            &[("error", error.to_string())],
                        ),
                    ),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn choose_log_export(&mut self, cx: &mut Context<Self>) {
        if self.logs.exporting {
            return;
        }
        self.logs.exporting = true;
        self.error = None;
        self.notice = None;
        let token = self.page_task_token_for(Page::Logs);
        let directory = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
        let receiver = cx.prompt_for_new_path(&directory, Some("zenclash-mihomo.log"));
        let entries = self.log_monitor.shared_entries();
        cx.spawn(async move |this, cx| {
            let selection = receiver.await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(Ok(Some(path))) = &selection
                    && this.is_page_task_current(token)
                {
                    this.write_log_export(path.clone(), entries, token, cx);
                    return;
                }
                this.logs.exporting = false;
                match selection {
                    Ok(Ok(Some(_))) => {
                        tracing::info!("discarded log export after leaving logs page")
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        this.set_page_error(
                            token,
                            zenclash_i18n::text_with(
                                "logs.errors.export_dialog",
                                &[("error", error.to_string())],
                            ),
                        );
                    }
                    Err(error) => {
                        this.set_page_error(
                            token,
                            zenclash_i18n::text_with(
                                "logs.errors.export_dialog_task",
                                &[("error", error.to_string())],
                            ),
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn write_log_export(
        &mut self,
        path: std::path::PathBuf,
        entries: Vec<Arc<zenclash_core::LogEntry>>,
        token: super::PageTaskToken,
        cx: &mut Context<Self>,
    ) {
        let display_path = path.display().to_string();
        let task = self.runtime.spawn(export_log_entries(path, entries));
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
                this.logs.exporting = false;
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

async fn export_log_entries(
    path: std::path::PathBuf,
    entries: Vec<Arc<zenclash_core::LogEntry>>,
) -> std::io::Result<()> {
    let payload = tokio::task::spawn_blocking(move || prepare_log_payload(entries, false))
        .await
        .map_err(std::io::Error::other)?;
    tokio::fs::write(path, payload).await
}

fn prepare_log_payload(entries: Vec<Arc<zenclash_core::LogEntry>>, support_safe: bool) -> String {
    let entries = entries
        .into_iter()
        .map(|entry| entry.as_ref().clone())
        .collect::<Vec<_>>();
    if support_safe {
        format_log_entries_support_safe(&entries)
    } else {
        format_log_entries(&entries)
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
        || contains_ascii_case_insensitive(&entry.level, query)
        || contains_ascii_case_insensitive(&entry.payload, query)
        || entry.fields.as_object().is_some_and(|fields| {
            fields.iter().any(|(key, value)| {
                contains_ascii_case_insensitive(key, query) || json_value_matches(value, query)
            })
        })
        || (!entry.fields.is_object() && json_value_matches(&entry.fields, query))
}

fn normalize_log_query(query: &str) -> String {
    query.trim().to_owned()
}

fn json_value_matches(value: &serde_json::Value, query: &str) -> bool {
    match value {
        serde_json::Value::Null => contains_ascii_case_insensitive("null", query),
        serde_json::Value::Bool(value) => {
            contains_ascii_case_insensitive(if *value { "true" } else { "false" }, query)
        }
        serde_json::Value::Number(value) => {
            contains_ascii_case_insensitive(&value.to_string(), query)
        }
        serde_json::Value::String(value) => contains_ascii_case_insensitive(value, query),
        serde_json::Value::Array(values) => {
            values.iter().any(|value| json_value_matches(value, query))
        }
        serde_json::Value::Object(fields) => fields.iter().any(|(key, value)| {
            contains_ascii_case_insensitive(key, query) || json_value_matches(value, query)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log_fixture(payload: &str) -> Arc<zenclash_core::LogEntry> {
        Arc::new(zenclash_core::LogEntry {
            payload: payload.into(),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn export_reports_write_failure_and_next_export_preserves_snapshot() {
        let directory = std::env::temp_dir().join(format!(
            "zenclash-log-export-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let entries = vec![log_fixture("first entry"), log_fixture("second entry")];
        let expected = prepare_log_payload(entries.clone(), false);
        let failed = export_log_entries(directory.clone(), entries.clone()).await;
        let path = directory.join("export.log");
        let saved = export_log_entries(path.clone(), entries).await;
        let content = tokio::fs::read_to_string(&path).await;
        std::fs::remove_dir_all(directory).unwrap();

        assert!(failed.is_err(), "writing a directory must report an error");
        saved.unwrap();
        assert_eq!(content.unwrap(), expected);
    }

    #[test]
    fn background_payload_preserves_export_and_redacts_support_copy() {
        let entries = vec![
            log_fixture("private-target.example"),
            log_fixture("second entry"),
        ];
        let raw = prepare_log_payload(entries.clone(), false);
        let safe = prepare_log_payload(entries, true);
        assert!(raw.find("private-target.example").unwrap() < raw.find("second entry").unwrap());
        assert!(!safe.contains("private-target.example"));
        assert!(!safe.contains("second entry"));
        assert_eq!(safe.lines().count(), 2);
    }

    #[test]
    fn redraw_and_search_reuse_the_same_log_snapshot() {
        let mut presentation = LogPresentation::default();
        presentation.refresh(1, String::new(), || {
            vec![log_fixture("old"), log_fixture("new")]
        });
        assert_eq!(presentation.matches, [1, 0]);
        presentation.refresh(1, String::new(), || {
            panic!("redraw fetched another snapshot")
        });
        presentation.refresh(1, "old".into(), || {
            panic!("search fetched another snapshot")
        });
        assert_eq!(presentation.matches, [0]);
    }

    #[test]
    fn log_rotation_and_clear_replace_filtered_indices() {
        let mut presentation = LogPresentation::default();
        presentation.refresh(1, "match".into(), || {
            vec![log_fixture("match"), log_fixture("other")]
        });
        assert_eq!(presentation.matches, [0]);
        presentation.refresh(2, "match".into(), || {
            vec![log_fixture("other"), log_fixture("match new")]
        });
        assert_eq!(presentation.matches, [1]);
        presentation.refresh(3, "match".into(), Vec::new);
        assert!(presentation.entries.is_empty());
        assert!(presentation.matches.is_empty());
    }

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
