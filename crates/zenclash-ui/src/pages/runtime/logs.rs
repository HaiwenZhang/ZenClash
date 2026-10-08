use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use super::{
    AppContext, Button, ClipboardItem, Context, Disableable, Entity, FluentBuilder, IconName,
    Input, InputEvent, InputState, IntoElement, LogTimeSource, MihomoLogLevel, Page, ParentElement,
    RuntimePage, Selectable, Sizable, Styled, Subscription, Window,
    contains_ascii_case_insensitive, div, empty_state, format_log_entries,
    format_log_entries_support_safe, h_flex, v_flex,
};

const LOG_HEALTH_REFRESH: Duration = Duration::from_secs(1);
mod dashboard;
mod dialogs;

pub(super) struct LogUiState {
    pub(super) filter: Entity<InputState>,
    pub(super) page: usize,
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

#[derive(Clone, Default)]
struct LogPresentation {
    revision: Option<u64>,
    query: String,
    entries: Vec<Arc<zenclash_core::LogEntry>>,
    rows: Vec<LogRow>,
    matches: Vec<usize>,
    level_counts: Vec<(String, u64)>,
    level_filter: Option<String>,
    oldest_first: bool,
}

#[derive(Clone)]
struct LogRow {
    level: gpui_kit::SharedString,
    payload: gpui_kit::SharedString,
    time: gpui_kit::SharedString,
    time_source: LogTimeSource,
}

impl From<&zenclash_core::LogEntry> for LogRow {
    fn from(entry: &zenclash_core::LogEntry) -> Self {
        Self {
            level: normalized_level(&entry.level).into(),
            payload: entry.payload.clone().into(),
            time: format_log_time(entry).into(),
            time_source: entry.time_source,
        }
    }
}

fn format_log_time(entry: &zenclash_core::LogEntry) -> String {
    if let Some(time) = &entry.core_time {
        if let Ok(time) = chrono::DateTime::parse_from_rfc3339(time) {
            return time
                .with_timezone(&chrono::Local)
                .format("%H:%M:%S")
                .to_string();
        }
        if chrono::NaiveTime::parse_from_str(time, "%H:%M:%S").is_ok() {
            return time.clone();
        }
    }
    i64::try_from(entry.timestamp_ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "—".into())
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
            self.rows = self
                .entries
                .iter()
                .map(|entry| LogRow::from(entry.as_ref()))
                .collect();
            self.revision = Some(revision);
            let mut counts = std::collections::BTreeMap::<String, u64>::new();
            for entry in &self.entries {
                *counts.entry(normalized_level(&entry.level)).or_default() += 1;
            }
            self.level_counts = counts.into_iter().collect();
        }
        if changed || self.query != query {
            self.query = query;
            self.matches = self
                .entries
                .iter()
                .enumerate()
                .rev()
                .filter(|(_, entry)| log_matches(entry, &self.query))
                .filter(|(_, entry)| {
                    self.level_filter
                        .as_ref()
                        .is_none_or(|level| normalized_level(&entry.level) == *level)
                })
                .map(|(index, _)| index)
                .collect();
            if self.oldest_first {
                self.matches.reverse();
            }
        }
    }

    fn set_order(&mut self, oldest_first: bool) {
        if self.oldest_first != oldest_first {
            self.matches.reverse();
            self.oldest_first = oldest_first;
        }
    }

    fn set_level_filter(&mut self, level: Option<String>) {
        self.level_filter = level;
        self.matches = self
            .entries
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, entry)| log_matches(entry, &self.query))
            .filter(|(_, entry)| {
                self.level_filter
                    .as_ref()
                    .is_none_or(|level| normalized_level(&entry.level) == *level)
            })
            .map(|(index, _)| index)
            .collect();
        if self.oldest_first {
            self.matches.reverse();
        }
    }
}

struct LogSnapshot {
    revision: u64,
    entries: Option<Vec<Arc<zenclash_core::LogEntry>>>,
    connected: bool,
    level: MihomoLogLevel,
    persistence: zenclash_core::LogPersistenceStatus,
    pending_entries: usize,
}

struct PreparedLogView {
    presentation: Arc<LogPresentation>,
    connected: bool,
    level: MihomoLogLevel,
    persistence: zenclash_core::LogPersistenceStatus,
    pending_entries: usize,
}

#[derive(Default)]
struct LogProjectionWorker {
    generation: Arc<AtomicU64>,
    gate: Arc<tokio::sync::Mutex<()>>,
    task: super::loader::PageReadTask,
}

impl LogProjectionWorker {
    fn cancel(&mut self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.task.cancel();
    }

    fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::Acquire) == generation
    }

    #[cfg(test)]
    fn start(
        &mut self,
        runtime: &tokio::runtime::Handle,
        previous: Arc<LogPresentation>,
        query: String,
        source: impl FnOnce() -> LogSnapshot + Send + 'static,
    ) -> (
        u64,
        tokio::task::JoinHandle<Result<Option<PreparedLogView>, String>>,
    ) {
        self.start_filtered(runtime, previous, query, None, false, source)
    }

    fn start_filtered(
        &mut self,
        runtime: &tokio::runtime::Handle,
        previous: Arc<LogPresentation>,
        query: String,
        level_filter: Option<String>,
        oldest_first: bool,
        source: impl FnOnce() -> LogSnapshot + Send + 'static,
    ) -> (
        u64,
        tokio::task::JoinHandle<Result<Option<PreparedLogView>, String>>,
    ) {
        self.cancel();
        let generation = self.generation.load(Ordering::Acquire);
        let current = self.generation.clone();
        let gate = self.gate.clone();
        let task = runtime.spawn(async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            let permit = gate.lock_owned().await;
            if current.load(Ordering::Acquire) != generation {
                return Ok(None);
            }
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let snapshot = source();
                if current.load(Ordering::Acquire) != generation {
                    return None;
                }
                let presentation = if previous.revision == Some(snapshot.revision)
                    && previous.query == query
                    && previous.level_filter == level_filter
                    && previous.oldest_first == oldest_first
                {
                    previous
                } else {
                    let mut presentation = previous.as_ref().clone();
                    presentation.refresh(snapshot.revision, query, || {
                        snapshot.entries.unwrap_or_default()
                    });
                    if presentation.level_filter != level_filter {
                        presentation.set_level_filter(level_filter);
                    }
                    presentation.set_order(oldest_first);
                    Arc::new(presentation)
                };
                (current.load(Ordering::Acquire) == generation).then_some(PreparedLogView {
                    presentation,
                    connected: snapshot.connected,
                    level: snapshot.level,
                    persistence: snapshot.persistence,
                    pending_entries: snapshot.pending_entries,
                })
            })
            .await
            .map_err(|error| error.to_string())
        });
        self.task.replace(&task);
        (generation, task)
    }
}

impl Drop for LogProjectionWorker {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl LogUiState {
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

    pub(super) fn release_results(&mut self) {
        self.cancel_refresh();
        self.presentation = Arc::default();
        self.last_refresh = None;
        self.level = None;
        self.persistence = zenclash_core::LogPersistenceStatus::default();
        self.pending_entries = 0;
        self.selected = None;
        self.paused = false;
    }

    pub(super) fn new(window: &mut Window, cx: &mut Context<RuntimePage>) -> (Self, Subscription) {
        let filter = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(zenclash_i18n::text("runtime.placeholders.log_filter"))
        });
        let subscription = cx.subscribe(&filter, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.logs.page = 0;
                this.logs.query = normalize_log_query(&this.logs.filter.read(cx).value());
                this.logs.cancel_refresh();
                this.update_log_presentation(cx);
                cx.notify();
            }
        });
        (
            Self {
                filter,
                page: 0,
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
            },
            subscription,
        )
    }
}

impl RuntimePage {
    pub(super) fn update_log_presentation(&mut self, cx: &mut Context<Self>) {
        if self.page != Page::Logs || !self.live_updates_enabled() || self.logs.loading {
            return;
        }
        let previous = self.logs.presentation.clone();
        let paused = self.logs.paused;
        let revision = if paused {
            previous.revision.unwrap_or_default()
        } else {
            self.log_monitor.revision()
        };
        if previous.revision == Some(revision)
            && previous.query == self.logs.query
            && previous.level_filter == self.logs.level_filter
            && previous.oldest_first == self.logs.oldest_first
            && self
                .logs
                .last_refresh
                .is_some_and(|at| at.elapsed() < LOG_HEALTH_REFRESH)
        {
            return;
        }
        let previous_revision = previous.revision;
        let monitor = self.log_monitor.clone();
        let (generation, task) = self.logs.worker.start_filtered(
            &self.runtime,
            previous,
            self.logs.query.clone(),
            self.logs.level_filter.clone(),
            self.logs.oldest_first,
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
        self.logs.loading = true;
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if !this.logs.worker.is_current(generation) {
                    return;
                }
                this.logs.loading = false;
                this.logs.worker.task.cancel();
                if this.page != Page::Logs || !this.live_updates_enabled() {
                    return;
                }
                this.logs.last_refresh = Some(Instant::now());
                match result {
                    Ok(Ok(Some(view))) => {
                        let changed = !Arc::ptr_eq(&this.logs.presentation, &view.presentation)
                            || this.logs.connected != view.connected
                            || this.logs.level != Some(view.level)
                            || this.logs.persistence != view.persistence
                            || this.logs.pending_entries != view.pending_entries;
                        this.logs.presentation = view.presentation;
                        this.logs.connected = view.connected;
                        this.logs.level = Some(view.level);
                        this.logs.persistence = view.persistence;
                        this.logs.pending_entries = view.pending_entries;
                        if changed {
                            if this.logs.auto_scroll {
                                this.jump_to_latest_log(cx);
                            }
                            cx.notify();
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        this.error = Some(zenclash_i18n::text_with(
                            "logs.errors.presentation_task",
                            &[("error", error)],
                        ));
                        cx.notify();
                    }
                    Err(error) if error.is_cancelled() => {}
                    Err(error) => {
                        this.error = Some(zenclash_i18n::text_with(
                            "logs.errors.presentation_task",
                            &[("error", error.to_string())],
                        ));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub(super) fn render_logs(
        &mut self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        self.log_dashboard(theme, cx)
    }

    fn jump_to_latest_log(&mut self, cx: &mut Context<Self>) {
        let index = if self.logs.oldest_first {
            self.logs.presentation.matches.len().saturating_sub(1)
        } else {
            0
        };
        self.logs.scroll.scroll_to_item(
            index,
            if self.logs.oldest_first {
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
        let disabled = self.preferences_store.is_none()
            || self.mutation_busy(crate::pages::runtime::busy::MutationDomain::Logs);
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
                        .checked(self.preferences.log_file_enabled)
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
                            .label(format!("{} MiB", self.preferences.log_file_max_mebibytes))
                            .small()
                            .outline()
                            .disabled(disabled || !self.preferences.log_file_enabled)
                            .dropdown_caret(true)
                            .dropdown_menu(move |mut menu, _, _| {
                                for mebibytes in [5_u16, 10, 25, 50] {
                                    let owner = owner.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(format!("{mebibytes} MiB")).on_click(
                                            move |_, _, cx| {
                                                let _ = owner.update(cx, |page, cx| {
                                                    page.set_log_file_limit(mebibytes, cx)
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
        if !matches!(self.page, Page::Logs | Page::Settings) {
            return;
        }
        let Some(store) = self.preferences_store.clone() else {
            self.error = Some(zenclash_i18n::text("logs.errors.preferences_unavailable"));
            cx.notify();
            return;
        };
        let log_path = store.log_file_path();
        let Some(token) = self
            .begin_scoped_mutation(self.page, crate::pages::runtime::busy::MutationDomain::Logs)
        else {
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

    fn copy_support_safe_logs(&mut self, cx: &mut Context<Self>) {
        if self.logs.copying {
            return;
        }
        self.logs.copying = true;
        let token = self.page_task_token_for(Page::Logs);
        let monitor = self.log_monitor.clone();
        let task = self
            .runtime
            .spawn_blocking(move || prepare_log_payload(monitor.shared_entries(), true));
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
        let monitor = self.log_monitor.clone();
        let task = self.runtime.spawn_blocking(move || {
            (
                std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir()),
                monitor.shared_entries(),
            )
        });
        cx.spawn(async move |this, cx| {
            let seed = task.await;
            let Ok(Some((receiver, entries))) = this.update(cx, |this, cx| {
                if !this.is_page_task_current(token) {
                    this.logs.exporting = false;
                    cx.notify();
                    return None;
                }
                match seed {
                    Ok((directory, entries)) => Some((
                        cx.prompt_for_new_path(&directory, Some("zenclash-mihomo.log")),
                        entries,
                    )),
                    Err(error) => {
                        this.logs.exporting = false;
                        this.set_page_error(
                            token,
                            zenclash_i18n::text_with(
                                "logs.errors.export_task",
                                &[("error", error.to_string())],
                            ),
                        );
                        cx.notify();
                        None
                    }
                }
            }) else {
                return;
            };
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

#[cfg(test)]
pub(in crate::pages::runtime) mod tests {
    use super::*;
    use gpui_kit::component::WindowExt;
    use gpui_kit::test::TestWindowExt;

    #[gpui_kit::test]
    fn pausing_log_display_keeps_filterable_rows_and_resume_reads_the_monitor(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use super::super::ui_tests::{Fixture, open};
        let fixture = Fixture::new();
        let (window, page) = open(cx, &fixture, Page::Logs);
        fixture.settle(cx, &page, |page| {
            !page.persistent_loading && !page.logs.loading
        });
        let entries = vec![
            log_fixture("frozen info"),
            Arc::new(zenclash_core::LogEntry {
                level: "warning".into(),
                payload: "frozen warning".into(),
                ..Default::default()
            }),
        ];
        let ids = entries
            .iter()
            .map(|entry| Arc::as_ptr(entry) as usize)
            .collect::<Vec<_>>();
        cx.update_window(window, |_, window, cx| {
            page.update(cx, |page, cx| {
                page.ui_visibility = super::super::lifecycle::UiVisibility::new(true);
                page.logs.cancel_refresh();
                let mut presentation = LogPresentation::default();
                presentation.refresh(page.log_monitor.revision(), String::new(), || entries);
                page.logs.presentation = Arc::new(presentation);
                cx.notify();
            });
            window.render_frame(cx);
            window.click("pause-logs-display", cx);
            window.click("clear-logs", cx);
            assert!(window.has_active_dialog(cx));
            window.press("escape", cx);
            assert!(!window.has_active_dialog(cx));
            assert_eq!(page.read(cx).logs.presentation.entries.len(), 2);
            assert!(page.read(cx).logs.paused);
            window.click("log-display-order", cx);
            window.press("escape", cx);
            assert!(!page.read(cx).logs.oldest_first);
            page.update(cx, |page, cx| {
                page.log_monitor.clear();
                page.log_monitor.set_level(MihomoLogLevel::Warning);
                page.update_log_presentation(cx);
            });
            window.render_frame(cx);
            window.find(("inspect-log", ids[0]));
            window.find(("inspect-log", ids[1]));
            window.click(
                (
                    gpui_kit::ElementId::from("log-level-filter"),
                    "WARNING".to_owned(),
                ),
                cx,
            );
            assert_eq!(page.read(cx).logs.level_filter.as_deref(), Some("WARNING"));
        })
        .unwrap();
        fixture.settle(cx, &page, |page| {
            !page.logs.loading && page.logs.presentation.level_filter.as_deref() == Some("WARNING")
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(("inspect-log", ids[0])).is_none());
            window.find(("inspect-log", ids[1]));
            assert!(page.read(cx).logs.paused);
            window.click("pause-logs-display", cx);
        })
        .unwrap();
        fixture.settle(cx, &page, |page| !page.logs.loading);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(!page.read(cx).logs.paused);
            assert!(window.try_find(("inspect-log", ids[1])).is_none());
            page.update(cx, |page, _| page.logs.release_results());
            assert!(!page.read(cx).logs.paused);
            window.remove_window();
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn settings_log_preferences_save_updates_owner_and_preserves_other_preferences(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        log_preferences_from_settings(cx, false);
    }

    #[gpui_kit::test]
    fn settings_log_preferences_save_failure_preserves_owner_and_releases_busy_state(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        log_preferences_from_settings(cx, true);
    }

    #[gpui_kit::test]
    fn settings_clear_history_removes_real_records_and_resets_confirmation(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        clear_history_from_settings(cx, false);
    }

    #[gpui_kit::test]
    fn settings_clear_history_failure_keeps_totals_and_releases_busy_state(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        clear_history_from_settings(cx, true);
    }

    fn clear_history_from_settings(cx: &mut gpui_kit::TestAppContext, fail: bool) {
        use crate::pages::runtime::busy::MutationDomain;
        use zenclash_core::{
            AppPreferences, AppPreferencesStore, TrafficDimension, TrafficHistoryEntry,
            TrafficHistoryQuery, TrafficHistoryStore,
        };
        let directory = settings_test_directory("clear");
        let history = TrafficHistoryStore::new(directory.join("history.sqlite"));
        history
            .insert_and_cleanup(
                &[TrafficHistoryEntry {
                    timestamp_ms: 1000,
                    source_ip: "127.0.0.1".into(),
                    host: "example.test".into(),
                    outbound: "DIRECT".into(),
                    process: "test".into(),
                    upload: 10,
                    download: 20,
                }],
                0,
            )
            .unwrap();
        let query = TrafficHistoryQuery {
            dimension: TrafficDimension::Host,
            start_ms: 0,
            end_ms: 2000,
            bucket_ms: 1000,
        };
        let overview = history.overview(&query).unwrap();
        assert_eq!(overview.totals.total, 30);
        let store = AppPreferencesStore::new(directory.join("preferences.json"));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let services = log_test_services(&runtime, &directory, &store, AppPreferences::default());
        let status = services.operational_status.clone();
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let mut owner = None;
        let window = cx.open_window(
            gpui_kit::size(gpui_kit::px(1200.), gpui_kit::px(2600.)),
            |window, cx| {
                let page = cx.new(|cx| RuntimePage::new(Page::Settings, services, window, cx));
                owner = Some(page.clone());
                gpui_kit::component::Root::new(page, window, cx)
            },
        );
        let page = owner.unwrap();
        cx.foreground_executor().clone().block_test(async {
            settle_log_preferences(cx, &runtime, &page, |page| !page.persistent_loading).await;
            if fail {
                std::fs::remove_file(history.path()).unwrap();
                std::fs::create_dir(history.path()).unwrap();
            }
            cx.update(|cx| {
                page.update(cx, |page, _| {
                    page.error = None;
                    page.traffic_history_store = Some(history.clone());
                    page.traffic_history.overview = overview;
                    page.traffic_history.clear_confirmation = true;
                })
            });
            cx.update_window(window.into(), |_, window, cx| {
                window.render_frame(cx);
                window.render_frame(cx);
                window.click("confirm-clear-traffic", cx);
            })
            .unwrap();
            assert!(cx.update(|cx| page.read(cx).mutation_busy(MutationDomain::TrafficHistory)));
            settle_log_preferences(cx, &runtime, &page, |page| {
                !page.mutation_busy(MutationDomain::TrafficHistory)
            })
            .await;
        });
        cx.update(|cx| {
            let current = page.read(cx);
            assert_eq!(current.page, Page::Settings);
            assert!(!current.traffic_history.clear_confirmation);
            assert!(!current.mutation_busy(MutationDomain::TrafficHistory));
            if fail {
                assert!(current.error.is_some());
                assert_eq!(current.traffic_history.overview.totals.total, 30);
            } else {
                assert!(current.error.is_none());
                assert_eq!(current.traffic_history.overview.totals.total, 0);
                assert_eq!(history.overview(&query).unwrap().totals.samples, 0);
            }
        });
        cx.update_window(window.into(), |_, window, _| window.remove_window())
            .unwrap();
        status.stop();
        runtime.shutdown_timeout(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(directory);
    }

    pub(in crate::pages::runtime) fn settings_test_directory(name: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "zenclash-settings-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    pub(in crate::pages::runtime) fn log_test_services(
        runtime: &tokio::runtime::Runtime,
        directory: &std::path::Path,
        store: &zenclash_core::AppPreferencesStore,
        preferences: zenclash_core::AppPreferences,
    ) -> crate::pages::runtime::RuntimePageServices {
        use crate::pages::runtime::RuntimePageServices;
        use zenclash_core::{
            ControlledConfigStore, CoreKind, CoreSession, MihomoClient, MihomoEndpoint,
            OperationalStatus, TrafficCaptureSession, TrafficMonitor,
        };
        let endpoint = MihomoEndpoint::new("http://127.0.0.1:1", "");
        let client = MihomoClient::new(endpoint.clone()).unwrap();
        let core =
            CoreSession::open_with_config(CoreKind::Mihomo, client.clone(), None, Vec::new())
                .unwrap();
        let traffic = TrafficMonitor::start(runtime.handle(), endpoint.clone());
        let logs =
            zenclash_core::LogMonitor::start(runtime.handle(), endpoint, MihomoLogLevel::Info);
        let status = OperationalStatus::start(
            runtime.handle(),
            core.clone(),
            None,
            traffic.clone(),
            logs.clone(),
        );
        let controlled = ControlledConfigStore::new(directory.join("controlled"));
        RuntimePageServices {
            profile_store: None,
            override_store: None,
            core_kind: CoreKind::Mihomo,
            core_session: core.clone(),
            profile_service: crate::ProfileService::new(core.clone(), None),
            client,
            runtime: runtime.handle().clone(),
            traffic_monitor: traffic,
            log_monitor: logs,
            operational_status: status,
            traffic_capture: TrafficCaptureSession::new(core, controlled.clone(), None, None),
            profile_path: None,
            controlled_config_store: controlled,
            preferences_store: Some(store.clone()),
            preferences,
            system_proxy_session: None,
            traffic_history_store: None,
            startup_notice: None,
            startup_error: None,
        }
    }

    #[expect(
        clippy::future_not_send,
        reason = "GPUI test contexts remain on the foreground thread"
    )]
    pub(in crate::pages::runtime) async fn settle_log_preferences(
        cx: &mut gpui_kit::TestAppContext,
        runtime: &tokio::runtime::Runtime,
        page: &Entity<RuntimePage>,
        predicate: impl Fn(&RuntimePage) -> bool,
    ) {
        for _ in 0..1000 {
            if cx.update(|cx| predicate(page.read(cx))) {
                return;
            }
            runtime
                .spawn(async {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        panic!("log preference operation did not settle");
    }

    fn log_preferences_from_settings(cx: &mut gpui_kit::TestAppContext, fail: bool) {
        use crate::pages::runtime::busy::MutationDomain;
        use zenclash_core::{AppPreferences, AppPreferencesStore, AppearancePreference};
        let directory = settings_test_directory("log");
        let preference_path = directory.join("preferences.json");
        if fail {
            std::fs::create_dir(&preference_path).unwrap();
        }
        let store = AppPreferencesStore::new(preference_path);
        let preferences = AppPreferences {
            appearance: AppearancePreference::Dark,
            log_file_enabled: false,
            ..Default::default()
        };
        if !fail {
            store.update(|saved| *saved = preferences.clone()).unwrap();
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let services = log_test_services(&runtime, &directory, &store, preferences);
        let logs = services.log_monitor.clone();
        let status = services.operational_status.clone();
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let mut owner = None;
        let window = cx.open_window(
            gpui_kit::size(gpui_kit::px(1200.), gpui_kit::px(2600.)),
            |window, cx| {
                let page = cx.new(|cx| RuntimePage::new(Page::Settings, services, window, cx));
                owner = Some(page.clone());
                gpui_kit::component::Root::new(page, window, cx)
            },
        );
        let page = owner.unwrap();
        cx.foreground_executor().clone().block_test(async {
            settle_log_preferences(cx, &runtime, &page, |page| !page.persistent_loading).await;
            cx.update(|cx| {
                page.update(cx, |page, _| {
                    page.error = None;
                })
            });
            cx.update_window(window.into(), |_, window, cx| {
                window.render_frame(cx);
                window.render_frame(cx);
                window.click("settings-log-file-enabled", cx);
            })
            .unwrap();
            assert!(cx.update(|cx| page.read(cx).mutation_busy(MutationDomain::Logs)));
            settle_log_preferences(cx, &runtime, &page, |page| {
                !page.mutation_busy(MutationDomain::Logs)
            })
            .await;
        });
        cx.update(|cx| {
            let current = page.read(cx);
            assert!(!current.mutation_busy(MutationDomain::Logs));
            assert_eq!(current.page, Page::Settings);
            assert_eq!(current.preferences.appearance, AppearancePreference::Dark);
            assert_eq!(current.preferences.log_file_enabled, !fail);
            if fail {
                assert!(current.error.is_some());
                assert!(!logs.persistence_status().enabled);
            } else {
                assert!(current.error.is_none());
                assert!(store.load().unwrap().log_file_enabled);
                assert!(logs.persistence_status().enabled);
            }
        });
        cx.update_window(window.into(), |_, window, _| window.remove_window())
            .unwrap();
        status.stop();
        runtime.shutdown_timeout(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn level_filter_combines_with_search_and_counts_the_whole_buffer() {
        let warning = Arc::new(zenclash_core::LogEntry {
            level: "warn".into(),
            payload: "network timeout".into(),
            ..Default::default()
        });
        let info = Arc::new(zenclash_core::LogEntry {
            level: "INFO".into(),
            payload: "network connected".into(),
            ..Default::default()
        });
        let mut presentation = LogPresentation::default();
        presentation.refresh(1, "network".into(), || vec![warning, info]);
        presentation.set_level_filter(Some("WARNING".into()));
        assert_eq!(presentation.matches, [0]);
        assert_eq!(
            presentation.level_counts,
            [("INFO".into(), 1), ("WARNING".into(), 1)]
        );
        presentation.refresh(1, "connected".into(), || {
            panic!("search fetched another snapshot")
        });
        assert!(presentation.matches.is_empty());
        presentation.set_level_filter(None);
        assert_eq!(presentation.matches, [1]);
    }

    #[tokio::test]
    async fn changing_only_level_filter_reuses_snapshot_and_updates_visible_entries() {
        let mut worker = LogProjectionWorker::default();
        let (_, task) = worker.start(
            &tokio::runtime::Handle::current(),
            Arc::default(),
            String::new(),
            || {
                snapshot(
                    1,
                    Some(vec![
                        Arc::new(zenclash_core::LogEntry {
                            level: "debug".into(),
                            ..Default::default()
                        }),
                        Arc::new(zenclash_core::LogEntry {
                            level: "info".into(),
                            ..Default::default()
                        }),
                    ]),
                )
            },
        );
        let previous = task.await.unwrap().unwrap().unwrap().presentation;
        let (_, task) = worker.start_filtered(
            &tokio::runtime::Handle::current(),
            previous,
            String::new(),
            Some("DEBUG".into()),
            false,
            || snapshot(1, None),
        );
        let filtered = task.await.unwrap().unwrap().unwrap().presentation;
        assert_eq!(filtered.matches, [0]);
        assert_eq!(filtered.entries.len(), 2);
    }

    fn log_fixture(payload: &str) -> Arc<zenclash_core::LogEntry> {
        Arc::new(zenclash_core::LogEntry {
            payload: payload.into(),
            ..Default::default()
        })
    }

    fn snapshot(revision: u64, entries: Option<Vec<Arc<zenclash_core::LogEntry>>>) -> LogSnapshot {
        LogSnapshot {
            revision,
            entries,
            connected: true,
            level: MihomoLogLevel::Info,
            persistence: zenclash_core::LogPersistenceStatus::default(),
            pending_entries: 0,
        }
    }

    #[tokio::test]
    async fn cancelling_a_pending_log_read_never_reads_the_source() {
        let reads = Arc::new(AtomicU64::new(0));
        let source_reads = reads.clone();
        let mut worker = LogProjectionWorker::default();
        let (generation, task) = worker.start(
            &tokio::runtime::Handle::current(),
            Arc::default(),
            String::new(),
            move || {
                source_reads.fetch_add(1, Ordering::AcqRel);
                snapshot(1, Some(vec![log_fixture("cancelled")]))
            },
        );
        worker.cancel();

        assert!(task.await.err().is_some_and(|error| error.is_cancelled()));
        assert!(!worker.is_current(generation));
        assert_eq!(reads.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn a_blocked_log_source_does_not_block_the_request_executor_or_publish_after_cancellation()
     {
        let caller_thread = std::thread::current().id();
        let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let mut worker = LogProjectionWorker::default();
        let (old, task) = worker.start(
            &tokio::runtime::Handle::current(),
            Arc::default(),
            "old".into(),
            move || {
                started_sender.send(std::thread::current().id()).unwrap();
                release_receiver
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
                snapshot(1, Some(vec![log_fixture("old")]))
            },
        );
        let source_thread = tokio::time::timeout(Duration::from_secs(1), started_receiver)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(source_thread, caller_thread);
        worker.cancel();
        assert!(task.await.err().is_some_and(|error| error.is_cancelled()));
        let (latest, task) = worker.start(
            &tokio::runtime::Handle::current(),
            Arc::default(),
            "latest".into(),
            || snapshot(2, Some(vec![log_fixture("old"), log_fixture("latest")])),
        );
        release_sender.send(()).unwrap();
        let view = task.await.unwrap().unwrap().unwrap();

        assert!(!worker.is_current(old));
        assert!(worker.is_current(latest));
        assert_eq!(view.presentation.matches, [1]);
        assert_eq!(view.presentation.rows[1].payload.as_ref(), "latest");
    }

    #[tokio::test]
    async fn searching_cached_logs_and_refreshing_health_do_not_need_another_entry_snapshot() {
        let mut worker = LogProjectionWorker::default();
        let runtime = tokio::runtime::Handle::current();
        let (_, task) = worker.start(&runtime, Arc::default(), String::new(), || {
            snapshot(1, Some(vec![log_fixture("old"), log_fixture("new")]))
        });
        let original = task.await.unwrap().unwrap().unwrap().presentation;
        let (_, task) = worker.start(&runtime, original.clone(), "old".into(), || {
            snapshot(1, None)
        });
        let filtered = task.await.unwrap().unwrap().unwrap().presentation;
        assert_eq!(filtered.matches, [0]);
        assert_eq!(filtered.entries.len(), 2);

        let (_, task) = worker.start(&runtime, filtered.clone(), "old".into(), || {
            let mut source = snapshot(1, None);
            source.connected = false;
            source.persistence.dropped_entries = 3;
            source
        });
        let health = task.await.unwrap().unwrap().unwrap();
        assert!(Arc::ptr_eq(&filtered, &health.presentation));
        assert!(!health.connected);
        assert_eq!(health.persistence.dropped_entries, 3);
        assert_eq!(health.presentation.matches, [0]);
    }

    #[test]
    fn prepared_log_rows_keep_the_full_export_source_and_structured_fields() {
        let entry = Arc::new(zenclash_core::LogEntry {
            level: "warning".into(),
            payload: "prepared payload".into(),
            fields: serde_json::json!({"detail": "界".repeat(300)}),
            ..Default::default()
        });
        let mut presentation = LogPresentation::default();
        presentation.refresh(1, String::new(), || vec![entry.clone()]);

        assert_eq!(presentation.rows[0].level.as_ref(), "WARNING");
        assert_eq!(presentation.rows[0].payload.as_ref(), "prepared payload");
        assert_eq!(presentation.entries[0].fields, entry.fields);
        assert!(Arc::ptr_eq(&presentation.entries[0], &entry));
        assert_eq!(
            presentation.entries[0].fields["detail"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            300
        );
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
    fn chronological_order_survives_filters_and_new_snapshots() {
        let mut presentation = LogPresentation::default();
        presentation.refresh(1, String::new(), || {
            vec![log_fixture("old"), log_fixture("new")]
        });
        presentation.set_order(true);
        assert_eq!(presentation.matches, [0, 1]);
        presentation.set_level_filter(Some(String::new()));
        assert_eq!(presentation.matches, [0, 1]);
        presentation.refresh(2, String::new(), || {
            vec![
                log_fixture("old"),
                log_fixture("new"),
                log_fixture("latest"),
            ]
        });
        assert_eq!(presentation.matches, [0, 1, 2]);
        presentation.set_order(false);
        assert_eq!(presentation.matches, [2, 1, 0]);
    }

    #[test]
    fn log_rows_normalize_time_and_warning() {
        let mut entry = zenclash_core::LogEntry {
            level: "warn".into(),
            core_time: Some("12:34:56".into()),
            ..Default::default()
        };
        let row = LogRow::from(&entry);
        assert_eq!(row.level.as_ref(), "WARNING");
        assert_eq!(row.time.as_ref(), "12:34:56");
        entry.core_time = Some("invalid".into());
        entry.timestamp_ms = u64::MAX;
        assert_eq!(format_log_time(&entry), "—");
        entry.timestamp_ms = 1_728_000_000_000;
        let expected = chrono::DateTime::from_timestamp_millis(1_728_000_000_000)
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%H:%M:%S")
            .to_string();
        assert_eq!(format_log_time(&entry), expected);
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
    fn log_level_descriptions_distinguish_daily_use_from_diagnostics() {
        let warning = log_level_description(MihomoLogLevel::Warning);
        let debug = log_level_description(MihomoLogLevel::Debug);
        assert!(!warning.is_empty());
        assert!(!debug.is_empty());
        assert_ne!(warning, debug);
    }
}
