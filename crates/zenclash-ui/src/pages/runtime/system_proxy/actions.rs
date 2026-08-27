use super::{SystemProxyEditorState, SystemProxyForm};
use crate::pages::runtime::{
    default_pac_script, default_system_proxy_bypass, load_page, normalize_pac_script,
    normalize_system_proxy_bypass, normalize_system_proxy_host, AppContext, AppPreferences,
    Context, InputState, Page, RuntimePage, SystemProxyController, SystemProxyMode, Window,
};
use zenclash_core::{SystemProxyOperation, SystemProxyOwnership};

impl SystemProxyForm {
    fn from_preferences(preferences: &AppPreferences) -> Self {
        Self {
            mode: preferences.system_proxy_mode,
            host: preferences.system_proxy_host.clone(),
            bypass: preferences.system_proxy_bypass.clone(),
            pac_script: preferences.system_proxy_pac_script.clone(),
        }
    }

    fn apply_to(&self, preferences: &mut AppPreferences) {
        preferences.system_proxy_mode = self.mode;
        preferences.system_proxy_host.clone_from(&self.host);
        preferences.system_proxy_bypass.clone_from(&self.bypass);
        preferences
            .system_proxy_pac_script
            .clone_from(&self.pac_script);
    }
}

impl RuntimePage {
    pub(in crate::pages::runtime) fn toggle_system_proxy(
        &mut self,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        let port = self.system_proxy_port();
        if enabled && port == 0 {
            self.error = Some(self.unavailable_system_proxy_message());
            cx.notify();
            return;
        }
        let page = self.page;
        let Some(token) = self.begin_mutation(page) else {
            return;
        };
        let Some(store) = self.preferences_store.clone() else {
            self.mutating = false;
            self.error = Some(zenclash_i18n::text("system_proxy.errors.ownership_store"));
            cx.notify();
            return;
        };
        let client = self.client.clone();
        let controller = self.system_proxy_controller.clone();
        let settings = SystemProxyForm::from_preferences(&self.preferences);
        let task = self.runtime.spawn(async move {
            let preferences = tokio::task::spawn_blocking(move || {
                persist_system_proxy_enabled(&store, &controller, enabled, port, &settings)
            })
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "system_proxy.errors.background_task",
                    &[("error", error.to_string())],
                )
            })??;
            let data = load_page(client, page).await;
            Ok::<_, String>((preferences, data))
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "system_proxy.errors.workflow_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok((preferences, data)) => {
                        this.preferences = preferences.clone();
                        cx.emit(super::super::PreferencesRestored { preferences });
                        match data {
                            Ok(data) => {
                                if this.replace_page_data(token, data) {
                                    this.notice = Some(if enabled {
                                        match this.preferences.system_proxy_mode {
                                            SystemProxyMode::Manual => zenclash_i18n::text(
                                                "system_proxy.notices.manual_enabled",
                                            ),
                                            SystemProxyMode::Pac => zenclash_i18n::text(
                                                "system_proxy.notices.pac_enabled",
                                            ),
                                        }
                                    } else {
                                        zenclash_i18n::text("system_proxy.notices.disabled")
                                    });
                                }
                            }
                            Err(error) => this.set_page_error(
                                token,
                                zenclash_i18n::text_with(
                                    "system_proxy.errors.refresh_after_toggle",
                                    &[("error", error)],
                                ),
                            ),
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

    pub(super) fn open_system_proxy_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let preferences = self.preferences.clone();
        let host = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("127.0.0.1")
                .default_value(preferences.system_proxy_host)
        });
        let bypass = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(zenclash_i18n::text(
                    "system_proxy.editor.bypass_placeholder",
                ))
                .default_value(preferences.system_proxy_bypass.join("\n"))
                .auto_grow(5, 12)
        });
        let pac_script = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("function FindProxyForURL(url, host) { ... }")
                .default_value(preferences.system_proxy_pac_script)
                .auto_grow(8, 20)
        });
        self.system_proxy_editor = Some(SystemProxyEditorState {
            mode: preferences.system_proxy_mode,
            host,
            bypass,
            pac_script,
        });
        cx.notify();
    }

    pub(super) fn set_system_proxy_editor_mode(
        &mut self,
        mode: SystemProxyMode,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.system_proxy_editor.as_mut() {
            editor.mode = mode;
            cx.notify();
        }
    }

    pub(super) fn reset_system_proxy_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.system_proxy_editor.as_ref() else {
            return;
        };
        editor.host.update(cx, |input, cx| {
            input.set_value("127.0.0.1", window, cx);
        });
        editor.bypass.update(cx, |input, cx| {
            input.set_value(default_system_proxy_bypass().join("\n"), window, cx);
        });
        editor.pac_script.update(cx, |input, cx| {
            input.set_value(default_pac_script(), window, cx);
        });
    }

    pub(super) fn cancel_system_proxy_editor(&mut self, cx: &mut Context<Self>) {
        self.system_proxy_editor = None;
        cx.notify();
    }

    pub(super) fn save_system_proxy_editor(&mut self, cx: &mut Context<Self>) {
        let form = match self.read_system_proxy_form(cx) {
            Ok(form) => form,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let Some(store) = self.preferences_store.clone() else {
            self.error = Some(zenclash_i18n::text("system_proxy.errors.settings_store"));
            cx.notify();
            return;
        };
        let active = self.preferences.system_proxy_enabled;
        let port = self.system_proxy_port();
        if active && port == 0 {
            self.error = Some(zenclash_i18n::text(
                "system_proxy.errors.active_without_port",
            ));
            cx.notify();
            return;
        }
        let Some(token) = self.begin_mutation(Page::SystemProxy) else {
            return;
        };
        let controller = self.system_proxy_controller.clone();
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            let preferences = tokio::task::spawn_blocking(move || {
                persist_system_proxy_form(&store, &controller, &form, port)
            })
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "system_proxy.errors.settings_task",
                    &[("error", error.to_string())],
                )
            })??;
            let data = load_page(client, Page::SystemProxy).await;
            Ok::<_, String>((preferences, data))
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "system_proxy.errors.settings_workflow",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok((preferences, data)) => {
                        this.preferences = preferences.clone();
                        this.system_proxy_editor = None;
                        cx.emit(super::super::PreferencesRestored { preferences });
                        if this.is_page_task_current(token) {
                            match data {
                                Ok(data) => {
                                    let _ = this.replace_page_data(token, data);
                                    this.notice =
                                        Some(zenclash_i18n::text("system_proxy.notices.saved"));
                                }
                                Err(error) => {
                                    this.error = Some(zenclash_i18n::text_with(
                                        "system_proxy.errors.refresh_native",
                                        &[("error", error)],
                                    ));
                                }
                            }
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

    fn read_system_proxy_form(&self, cx: &Context<Self>) -> Result<SystemProxyForm, String> {
        let editor = self
            .system_proxy_editor
            .as_ref()
            .ok_or_else(|| zenclash_i18n::text("system_proxy.errors.editor_closed"))?;
        let host = normalize_system_proxy_host(&editor.host.read(cx).value())
            .map_err(|error| error.to_string())?;
        let entries = editor
            .bypass
            .read(cx)
            .value()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let bypass = normalize_system_proxy_bypass(&entries).map_err(|error| error.to_string())?;
        let pac_script = normalize_pac_script(&editor.pac_script.read(cx).value())
            .map_err(|error| error.to_string())?;
        Ok(SystemProxyForm {
            mode: editor.mode,
            host,
            bypass,
            pac_script,
        })
    }

    fn system_proxy_port(&self) -> u16 {
        self.config()
            .and_then(zenclash_core::RuntimeConfig::system_proxy_port)
            .unwrap_or_default()
    }

    fn unavailable_system_proxy_message(&self) -> String {
        let listener_error = self.process.as_ref().and_then(|process| {
            process.snapshot().logs.into_iter().rev().find(|line| {
                let normalized = line.to_ascii_lowercase();
                normalized.contains("start http server error")
                    || normalized.contains("start mixed server error")
                    || normalized.contains("address already in use")
            })
        });
        unavailable_message(listener_error.as_deref())
    }
}

fn unavailable_message(listener_error: Option<&str>) -> String {
    match listener_error {
        Some(error)
            if error
                .to_ascii_lowercase()
                .contains("address already in use") =>
        {
            zenclash_i18n::text_with(
                "system_proxy.errors.port_in_use",
                &[("error", error.to_owned())],
            )
        }
        Some(error) => zenclash_i18n::text_with(
            "system_proxy.errors.port_not_listening",
            &[("error", error.to_owned())],
        ),
        None => zenclash_i18n::text("system_proxy.errors.no_http_port"),
    }
}

fn persist_system_proxy_form(
    store: &crate::pages::runtime::AppPreferencesStore,
    controller: &SystemProxyController,
    form: &SystemProxyForm,
    port: u16,
) -> Result<AppPreferences, String> {
    let operation = controller.begin_operation();
    let expected = store.load().map_err(|error| error.to_string())?;
    let previous = SystemProxyForm::from_preferences(&expected);
    let active = expected.system_proxy_enabled;
    let ownership = if active {
        Some(apply_owned_system_proxy(&operation, port, form)?)
    } else {
        None
    };
    match store.update(|preferences| {
        form.apply_to(preferences);
        if active {
            preferences.system_proxy_ownership.clone_from(&ownership);
        }
    }) {
        Ok(preferences) => Ok(preferences),
        Err(error) if active => Err(restore_after_persist_failure(
            store,
            &operation,
            port,
            &expected,
            &previous,
            &error.to_string(),
        )),
        Err(error) => Err(error.to_string()),
    }
}

fn persist_system_proxy_enabled(
    store: &crate::pages::runtime::AppPreferencesStore,
    controller: &SystemProxyController,
    enabled: bool,
    port: u16,
    form: &SystemProxyForm,
) -> Result<AppPreferences, String> {
    let operation = controller.begin_operation();
    let expected = store.load().map_err(|error| error.to_string())?;
    let previous = SystemProxyForm::from_preferences(&expected);
    let ownership = if enabled {
        Some(apply_owned_system_proxy(&operation, port, form)?)
    } else {
        release_system_proxy(&operation, &expected)?;
        None
    };
    match store.update(|preferences| {
        preferences.system_proxy_enabled = enabled;
        preferences.system_proxy_ownership.clone_from(&ownership);
    }) {
        Ok(preferences) => Ok(preferences),
        Err(error) => Err(restore_after_persist_failure(
            store,
            &operation,
            port,
            &expected,
            &previous,
            &error.to_string(),
        )),
    }
}

fn apply_owned_system_proxy(
    operation: &SystemProxyOperation<'_>,
    port: u16,
    form: &SystemProxyForm,
) -> Result<SystemProxyOwnership, String> {
    operation
        .apply(
            true,
            form.mode,
            &form.host,
            port,
            &form.bypass,
            &form.pac_script,
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| zenclash_i18n::text("system_proxy.errors.ownership_missing"))
}

fn release_system_proxy(
    operation: &SystemProxyOperation<'_>,
    preferences: &AppPreferences,
) -> Result<(), String> {
    if let Some(ownership) = &preferences.system_proxy_ownership {
        operation
            .release_if_owned(ownership)
            .map(|_| ())
            .map_err(|error| error.to_string())
    } else {
        operation
            .set_enabled(false, preferences.system_proxy_mode, "", 0, &[], "")
            .map_err(|error| error.to_string())
    }
}

fn restore_after_persist_failure(
    store: &crate::pages::runtime::AppPreferencesStore,
    operation: &SystemProxyOperation<'_>,
    port: u16,
    expected: &AppPreferences,
    previous: &SystemProxyForm,
    error: &str,
) -> String {
    if !expected.system_proxy_enabled {
        return match operation.set_enabled(false, previous.mode, "", 0, &[], "") {
            Ok(()) => zenclash_i18n::text_with(
                "system_proxy.errors.save_released",
                &[("error", error.to_owned())],
            ),
            Err(rollback) => zenclash_i18n::text_with(
                "system_proxy.errors.save_release_failed",
                &[
                    ("error", error.to_owned()),
                    ("rollback", rollback.to_string()),
                ],
            ),
        };
    }
    match apply_owned_system_proxy(operation, port, previous) {
        Ok(ownership) => {
            if expected.system_proxy_ownership.as_ref() == Some(&ownership) {
                return zenclash_i18n::text_with(
                    "system_proxy.errors.save_rolled_back",
                    &[("error", error.to_owned())],
                );
            }
            match store.update(|preferences| {
                preferences.system_proxy_ownership = Some(ownership.clone());
            }) {
                Ok(_) => zenclash_i18n::text_with(
                    "system_proxy.errors.save_rolled_back",
                    &[("error", error.to_owned())],
                ),
                Err(ownership_error) => {
                    let release = operation.release_if_owned(&ownership);
                    match release {
                        Ok(_) => zenclash_i18n::text_with(
                            "system_proxy.errors.save_ownership_released",
                            &[
                                ("error", error.to_owned()),
                                ("ownership_error", ownership_error.to_string()),
                            ],
                        ),
                        Err(release_error) => zenclash_i18n::text_with(
                            "system_proxy.errors.save_ownership_release_failed",
                            &[
                                ("error", error.to_owned()),
                                ("ownership_error", ownership_error.to_string()),
                                ("release_error", release_error.to_string()),
                            ],
                        ),
                    }
                }
            }
        }
        Err(rollback) => zenclash_i18n::text_with(
            "system_proxy.errors.save_rollback_failed",
            &[
                ("error", error.to_owned()),
                ("rollback", rollback.to_string()),
            ],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::unavailable_message;

    #[test]
    fn unavailable_message_explains_an_occupied_listener() {
        let message = unavailable_message(Some(
            "Start HTTP server error: listen tcp 127.0.0.1:7890: bind: address already in use",
        ));

        assert!(message.contains("HTTP/Mixed") && message.contains("7890"));
    }

    #[test]
    fn unavailable_message_rejects_socks_only_system_proxying() {
        let message = unavailable_message(None);

        assert!(message.contains("SOCKS-only") && message.contains("Mixed"));
    }
}
