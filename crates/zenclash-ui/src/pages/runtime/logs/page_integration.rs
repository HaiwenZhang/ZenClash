//! Connects retained log presentation events to the original page command owners.

use super::super::{Page, RuntimePage};
use super::export::{export_log_entries, prepare_log_payload};
use super::*;

impl RuntimePage {
    pub(in crate::pages::runtime) fn synchronize_log_context(&self, cx: &mut Context<Self>) {
        let context = LogViewContext {
            navigation: self.page_task_token_for(self.page),
            active: self.page == Page::Logs && self.live_updates_enabled(),
            preferences: LogPersistencePreferences {
                enabled: self.preferences.log_file_enabled,
                max_mebibytes: self.preferences.log_file_max_mebibytes,
            },
            commands: LogCommandAvailability {
                can_change_level: !self.core_busy()
                    && !self.config_inputs.is_loading()
                    && self
                        .config_inputs
                        .is_for_profile(self.profile_path.as_deref()),
                can_save_settings: self.preferences_store.is_some() && !self.core_busy(),
                can_change_preferences: self.preferences_store.is_some()
                    && !self.mutation_busy(super::super::busy::MutationDomain::Logs),
            },
        };
        self.logs
            .update(cx, |view, cx| view.set_context(context, cx));
    }

    pub(in crate::pages::runtime) fn handle_log_event(
        &mut self,
        event: LogEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            LogEvent::PresentationFailed { context, error } => self.set_page_error(context, error),
            LogEvent::CommandRequested { context, command } => {
                if !self.is_page_task_current(context) {
                    self.logs.update(cx, |view, cx| match command {
                        LogCommand::CopySupportSafe => view.finish_copy(cx),
                        LogCommand::Export => view.finish_export(cx),
                        _ => {}
                    });
                    return;
                }
                match command {
                    LogCommand::CopySupportSafe => self.copy_support_safe_logs(cx),
                    LogCommand::Export => self.choose_log_export(cx),
                    LogCommand::SavePreferences {
                        enabled,
                        max_mebibytes,
                        success,
                    } => self.persist_log_preferences(enabled, max_mebibytes, success, cx),
                    LogCommand::SaveSettings { level, preferences } => {
                        if let Some(level) = level {
                            self.apply_controlled_config(
                                serde_json::json!({"log-level":level.api_value()}),
                                zenclash_i18n::text("logs.collection_level_updated"),
                                cx,
                            );
                        }
                        self.persist_log_preferences(
                            Some(preferences.enabled),
                            Some(preferences.max_mebibytes),
                            zenclash_i18n::text("logs.notices.limit_saved"),
                            cx,
                        );
                    }
                }
            }
        }
    }

    pub(in crate::pages::runtime) fn update_log_presentation(&mut self, cx: &mut Context<Self>) {
        self.synchronize_log_context(cx);
        self.logs
            .update(cx, |view, cx| view.update_log_presentation(cx));
    }

    pub(in crate::pages::runtime) fn render_logs(&self) -> gpui_kit::AnyElement {
        self.logs.clone().into_any_element()
    }

    pub(in crate::pages::runtime) fn render_log_actions(
        &self,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        self.logs.update(cx, |view, cx| view.render_log_actions(cx))
    }

    pub(in crate::pages::runtime) fn log_stream_status(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        self.logs.read(cx).log_stream_status(theme)
    }

    pub(in crate::pages::runtime) fn render_log_preferences_controls(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        self.logs.update(cx, |view, cx| {
            view.render_log_preferences_controls(theme, cx)
        })
    }

    pub(in crate::pages::runtime) fn open_log_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.synchronize_log_context(cx);
        self.logs
            .update(cx, |view, cx| view.open_log_settings(window, cx));
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
        let token = self.page_task_token_for(Page::Logs);
        let monitor = self.log_monitor.clone();
        let task = self
            .runtime
            .spawn_blocking(move || prepare_log_payload(monitor.shared_entries(), true));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.logs.update(cx, |view, cx| view.finish_copy(cx));
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
                    this.logs.update(cx, |view, cx| view.finish_export(cx));
                    cx.notify();
                    return None;
                }
                match seed {
                    Ok((directory, entries)) => Some((
                        cx.prompt_for_new_path(&directory, Some("zenclash-mihomo.log")),
                        entries,
                    )),
                    Err(error) => {
                        this.logs.update(cx, |view, cx| view.finish_export(cx));
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
                this.logs.update(cx, |view, cx| view.finish_export(cx));
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
        token: super::super::PageTaskToken,
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
                this.logs.update(cx, |view, cx| view.finish_export(cx));
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
