//! Retained log display: input, projection, pause, selection and settings drafts.

use super::{
    AppContext, Button, ClipboardItem, Context, Disableable, Entity, FluentBuilder, IconName,
    Input, InputEvent, InputState, IntoElement, LogTimeSource, MihomoLogLevel, ParentElement,
    Selectable, Sizable, Styled, Subscription, Window, contains_ascii_case_insensitive, div,
    empty_state, h_flex, v_flex,
};
use gpui_kit::component::ActiveTheme;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

mod dashboard;
mod dialogs;
mod export;
mod page_integration;
mod projection;
#[cfg(test)]
use projection::format_log_time;
use projection::{LogPresentation, LogProjectionWorker, LogRow, LogSnapshot};

const LOG_HEALTH_REFRESH: Duration = Duration::from_secs(1);

pub(super) struct LogsView {
    runtime: tokio::runtime::Handle,
    log_monitor: Arc<zenclash_core::LogMonitor>,
    core_kind: zenclash_core::CoreKind,
    context: Option<LogViewContext>,
    focus_handle: gpui_kit::FocusHandle,
    _filter_subscription: Subscription,
    filter: Entity<InputState>,
    scroll: gpui_kit::UniformListScrollHandle,
    auto_scroll: bool,
    query: String,
    presentation: Arc<LogPresentation>,
    worker: LogProjectionWorker,
    loading: bool,
    last_refresh: Option<Instant>,
    connected: bool,
    level: Option<MihomoLogLevel>,
    persistence: zenclash_core::LogPersistenceStatus,
    pending_entries: usize,
    copying: bool,
    exporting: bool,
    level_filter: Option<String>,
    oldest_first: bool,
    selected: Option<(Arc<zenclash_core::LogEntry>, LogRow)>,
    paused: bool,
    settings: Option<dialogs::LogSettingsDraft>,
}

/// Persisted log preferences supplied by the existing application preference owner.
#[derive(Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub(super) struct LogPersistencePreferences {
    pub(super) enabled: bool,
    pub(super) max_mebibytes: u16,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub(super) struct LogCommandAvailability {
    pub(super) can_change_level: bool,
    pub(super) can_save_settings: bool,
    pub(super) can_change_preferences: bool,
}

/// Display facts, not configuration or runtime mutation authority.
#[derive(Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub(super) struct LogViewContext {
    pub(super) navigation: super::PageTaskToken,
    pub(super) active: bool,
    pub(super) preferences: LogPersistencePreferences,
    pub(super) commands: LogCommandAvailability,
}

#[derive(Clone)]
pub(super) enum LogCommand {
    CopySupportSafe,
    Export,
    SaveSettings {
        level: Option<MihomoLogLevel>,
        preferences: LogPersistencePreferences,
    },
    SavePreferences {
        enabled: Option<bool>,
        max_mebibytes: Option<u16>,
        success: String,
    },
}

#[derive(Clone)]
pub(super) enum LogEvent {
    CommandRequested {
        context: super::PageTaskToken,
        command: LogCommand,
    },
    PresentationFailed {
        context: super::PageTaskToken,
        error: String,
    },
}

impl gpui_kit::EventEmitter<LogEvent> for LogsView {}

impl LogsView {
    #[cfg(all(test, target_os = "windows"))]
    pub(super) fn design_validation_log_id(&self) -> Option<usize> {
        self.presentation
            .matches
            .first()
            .map(|&index| Arc::as_ptr(&self.presentation.entries[index]) as usize)
    }

    #[cfg(all(test, target_os = "windows"))]
    pub(super) fn prepare_design_validation(&mut self) {
        let entries = ["info", "error", "debug", "warning"].into_iter().cycle().take(128).enumerate().map(|(index, level)| {
            let (payload, fields) = match index % 4 {
                0 => (format!("[TCP] example.com:443 → PROXY ({index})"), serde_json::json!({"network": "tcp", "group": "PROXY"})),
                1 => ("规则资源请求失败，可重试".into(), serde_json::json!({"type": "rule-provider", "error": "request failed"})),
                2 => (format!("DNS 查询完成 · service-{index}.example.com"), serde_json::Value::Null),
                _ => ("节点延迟测试超时".into(), serde_json::json!({"type": "healthcheck", "proxy": "fixture-node", "timeout": "5000ms"})),
            };
            Arc::new(zenclash_core::LogEntry { level: level.into(), payload, core_time: Some(format!("12:{:02}:{:02}", 32 + index / 60, index % 60)), time_source: LogTimeSource::Core, fields, ..Default::default() })
        }).collect();
        let mut presentation = LogPresentation::default();
        presentation.refresh(0, String::new(), || entries);
        self.presentation = Arc::new(presentation);
        self.selected = self
            .presentation
            .entries
            .first()
            .zip(self.presentation.rows.first())
            .map(|(entry, row)| (entry.clone(), row.clone()));
        self.level = Some(MihomoLogLevel::Info);
    }

    pub(super) fn cancel_refresh(&mut self) {
        self.worker.cancel();
        self.loading = false;
    }

    pub(super) fn release_results(&mut self, cx: &mut Context<Self>) {
        self.cancel_refresh();
        self.presentation = Arc::default();
        self.last_refresh = None;
        self.level = None;
        self.persistence = zenclash_core::LogPersistenceStatus::default();
        self.pending_entries = 0;
        self.selected = None;
        self.paused = false;
        if let Some(context) = &mut self.context {
            context.active = false;
        }
        cx.notify();
    }

    pub(super) fn new(
        runtime: tokio::runtime::Handle,
        log_monitor: Arc<zenclash_core::LogMonitor>,
        core_kind: zenclash_core::CoreKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(zenclash_i18n::text("runtime.placeholders.log_filter"))
        });
        let subscription = cx.subscribe(&filter, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.query = normalize_log_query(&input.read(cx).value());
                this.cancel_refresh();
                this.update_log_presentation(cx);
                cx.notify();
            }
        });
        Self {
            runtime,
            log_monitor,
            core_kind,
            context: None,
            focus_handle: cx.focus_handle(),
            _filter_subscription: subscription,
            filter,
            scroll: gpui_kit::UniformListScrollHandle::default(),
            auto_scroll: true,
            query: String::new(),
            presentation: Arc::default(),
            worker: LogProjectionWorker::default(),
            loading: false,
            last_refresh: None,
            connected: false,
            level: None,
            persistence: zenclash_core::LogPersistenceStatus::default(),
            pending_entries: 0,
            copying: false,
            exporting: false,
            level_filter: None,
            oldest_first: false,
            selected: None,
            paused: false,
            settings: None,
        }
    }

    pub(super) fn suspend_presentation(&mut self, cx: &mut Context<Self>) {
        self.cancel_refresh();
        if let Some(context) = &mut self.context {
            context.active = false;
        }
        cx.notify();
    }

    pub(super) fn set_context(&mut self, context: LogViewContext, cx: &mut Context<Self>) {
        if self.context == Some(context) {
            return;
        }
        self.context = Some(context);
        if !context.active {
            self.cancel_refresh();
        }
        cx.notify();
    }

    pub(super) fn refresh_localized_placeholder(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.filter.update(cx, |input, cx| {
            input.set_placeholder(
                zenclash_i18n::text("runtime.placeholders.log_filter"),
                window,
                cx,
            )
        });
    }

    fn request_command(&self, command: LogCommand, cx: &mut Context<Self>) {
        if let Some(context) = self.context {
            cx.emit(LogEvent::CommandRequested {
                context: context.navigation,
                command,
            });
        }
    }

    fn request_support_copy(&mut self, cx: &mut Context<Self>) {
        if self.copying || self.context.is_none() {
            return;
        }
        self.copying = true;
        self.request_command(LogCommand::CopySupportSafe, cx);
        cx.notify();
    }

    fn request_export(&mut self, cx: &mut Context<Self>) {
        if self.exporting || self.context.is_none() {
            return;
        }
        self.exporting = true;
        self.request_command(LogCommand::Export, cx);
        cx.notify();
    }

    pub(super) fn finish_copy(&mut self, cx: &mut Context<Self>) {
        self.copying = false;
        cx.notify();
    }

    pub(super) fn finish_export(&mut self, cx: &mut Context<Self>) {
        self.exporting = false;
        cx.notify();
    }
}

impl LogsView {
    pub(super) fn update_log_presentation(&mut self, cx: &mut Context<Self>) {
        if !self.context.is_some_and(|context| context.active) || self.loading {
            return;
        }
        let previous = self.presentation.clone();
        let paused = self.paused;
        let revision = if paused {
            previous.revision.unwrap_or_default()
        } else {
            self.log_monitor.revision()
        };
        if previous.revision == Some(revision)
            && previous.query == self.query
            && previous.level_filter == self.level_filter
            && previous.oldest_first == self.oldest_first
            && self
                .last_refresh
                .is_some_and(|at| at.elapsed() < LOG_HEALTH_REFRESH)
        {
            return;
        }
        let previous_revision = previous.revision;
        let monitor = self.log_monitor.clone();
        let (generation, task) = self.worker.start_filtered(
            &self.runtime,
            previous,
            self.query.clone(),
            self.level_filter.clone(),
            self.oldest_first,
            move || {
                let revision = if paused { revision } else { monitor.revision() };
                LogSnapshot {
                    revision,
                    entries: (previous_revision != Some(revision))
                        .then(|| monitor.shared_entries()),
                    connected: monitor.connected(),
                    level: monitor.level(),
                    persistence: monitor.persistence_status(),
                    pending_entries: monitor.pending_persistence_entries(),
                }
            },
        );
        self.loading = true;
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if !this.worker.is_current(generation) {
                    return;
                }
                this.loading = false;
                this.worker.task.cancel();
                if !this.context.is_some_and(|context| context.active) {
                    return;
                }
                this.last_refresh = Some(Instant::now());
                match result {
                    Ok(Ok(Some(view))) => {
                        let changed = !Arc::ptr_eq(&this.presentation, &view.presentation)
                            || this.connected != view.connected
                            || this.level != Some(view.level)
                            || this.persistence != view.persistence
                            || this.pending_entries != view.pending_entries;
                        this.presentation = view.presentation;
                        this.connected = view.connected;
                        this.level = Some(view.level);
                        this.persistence = view.persistence;
                        this.pending_entries = view.pending_entries;
                        if changed {
                            if this.auto_scroll {
                                this.jump_to_latest_log(cx);
                            }
                            cx.notify();
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        let error = zenclash_i18n::text_with(
                            "logs.errors.presentation_task",
                            &[("error", error)],
                        );
                        if let Some(context) = this.context {
                            cx.emit(LogEvent::PresentationFailed {
                                context: context.navigation,
                                error,
                            });
                        }
                        cx.notify();
                    }
                    Err(error) if error.is_cancelled() => {}
                    Err(error) => {
                        let error = zenclash_i18n::text_with(
                            "logs.errors.presentation_task",
                            &[("error", error.to_string())],
                        );
                        if let Some(context) = this.context {
                            cx.emit(LogEvent::PresentationFailed {
                                context: context.navigation,
                                error,
                            });
                        }
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
    fn jump_to_latest_log(&mut self, cx: &mut Context<Self>) {
        let index = if self.oldest_first {
            self.presentation.matches.len().saturating_sub(1)
        } else {
            0
        };
        self.scroll.scroll_to_item(
            index,
            if self.oldest_first {
                gpui_kit::ScrollStrategy::Bottom
            } else {
                gpui_kit::ScrollStrategy::Top
            },
        );
        cx.notify();
    }
    pub(super) fn render_log_preferences_controls(
        &self,
        _theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
        let owner = cx.entity().downgrade();
        let disabled = !self
            .context
            .is_some_and(|context| context.commands.can_change_preferences);
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .child(zenclash_i18n::text("logs.persistence.enabled.title")),
                    )
                    .child(
                        crate::components::mint_switch::MintSwitch::new(
                            "settings-log-file-enabled",
                        )
                        .accessibility_label(zenclash_i18n::text("logs.persistence.enabled.title"))
                        .checked(
                            self.context
                                .is_some_and(|context| context.preferences.enabled),
                        )
                        .disabled(disabled)
                        .on_click(cx.listener(|this, checked, _, cx| {
                            this.set_log_file_enabled(*checked, cx)
                        })),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .child(zenclash_i18n::text("logs.persistence.limit")),
                    )
                    .child(
                        Button::new("settings-log-file-limit")
                            .label(format!(
                                "{} MiB",
                                self.context
                                    .map_or(0, |context| context.preferences.max_mebibytes)
                            ))
                            .small()
                            .outline()
                            .disabled(
                                disabled
                                    || !self
                                        .context
                                        .is_some_and(|context| context.preferences.enabled),
                            )
                            .dropdown_caret(true)
                            .dropdown_menu(move |mut menu, _, _| {
                                for mebibytes in [5_u16, 10, 25, 50] {
                                    let owner = owner.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(format!("{mebibytes} MiB")).on_click(
                                            move |_, _, cx| {
                                                let _ = owner.update(cx, |view, cx| {
                                                    view.set_log_file_limit(mebibytes, cx)
                                                });
                                            },
                                        ),
                                    );
                                }
                                menu
                            }),
                    ),
            )
    }

    fn set_log_file_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.request_preferences_change(
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
        self.request_preferences_change(
            None,
            Some(mebibytes),
            zenclash_i18n::text("logs.notices.limit_saved"),
            cx,
        );
    }
    fn request_preferences_change(
        &self,
        enabled: Option<bool>,
        max_mebibytes: Option<u16>,
        success: String,
        cx: &mut Context<Self>,
    ) {
        self.request_command(
            LogCommand::SavePreferences {
                enabled,
                max_mebibytes,
                success,
            },
            cx,
        );
    }
}

fn normalized_level(level: &str) -> String {
    if level.eq_ignore_ascii_case("warn") {
        "WARNING".into()
    } else {
        level.to_uppercase()
    }
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

impl gpui_kit::Render for LogsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        self.log_dashboard(&theme, cx)
    }
}

#[cfg(test)]
pub(in crate::pages::runtime) mod tests;

#[cfg(test)]
mod host_ui_tests;
