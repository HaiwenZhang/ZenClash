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
            self.error = Some("应用设置存储不可用；无法记录系统代理所有权".into());
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
            .map_err(|error| format!("系统代理后台任务异常结束：{error}"))??;
            let data = load_page(client, page).await;
            Ok::<_, String>((preferences, data))
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("系统代理工作流异常结束：{error}"))
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
                                            SystemProxyMode::Manual => "系统 HTTP/HTTPS 代理已启用",
                                            SystemProxyMode::Pac => "系统 PAC 自动代理已启用",
                                        }
                                        .into()
                                    } else {
                                        "系统代理已停用".into()
                                    });
                                }
                            }
                            Err(error) => this.set_page_error(
                                token,
                                format!("系统代理已切换并保存，但刷新页面状态失败：{error}"),
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
                .placeholder("每行一条，例如 localhost、192.168.0.0/16、*.example.com")
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
            self.error = Some("应用设置存储不可用；无法保存系统代理设置".into());
            cx.notify();
            return;
        };
        let active = self.preferences.system_proxy_enabled;
        let port = self.system_proxy_port();
        if active && port == 0 {
            self.error = Some("当前系统代理已启用，但内核没有可用代理端口".into());
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
            .map_err(|error| format!("系统代理设置任务异常结束：{error}"))??;
            let data = load_page(client, Page::SystemProxy).await;
            Ok::<_, String>((preferences, data))
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("系统代理设置工作流异常结束：{error}"))
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
                                        Some("系统代理设置已保存，并通过原生状态回读".into());
                                }
                                Err(error) => {
                                    this.error = Some(format!(
                                        "系统代理设置已经保存，但刷新原生状态失败：{error}"
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
            .ok_or_else(|| "系统代理编辑器尚未打开".to_owned())?;
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
        Some(error) if error.to_ascii_lowercase().contains("address already in use") => format!(
            "当前内核的 HTTP/Mixed 监听端口被其他程序占用，无法启用系统代理。请退出其他代理客户端（例如 Clash Party）或修改 ZenClash 监听端口后重试。内核日志：{error}"
        ),
        Some(error) => format!(
            "当前内核没有成功启动 HTTP/Mixed 监听端口，无法启用系统代理。内核日志：{error}"
        ),
        None => "当前内核没有可用的 HTTP/Mixed 监听端口，无法启用系统代理。SOCKS-only 端口不能用于系统 HTTP/HTTPS 代理；请在核心设置中启用 Mixed 或 HTTP 端口。".into(),
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
        .ok_or_else(|| "系统代理启用成功但没有返回所有权证据".to_owned())
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
            Ok(()) => format!("保存系统代理设置失败：{error}；系统代理已安全关闭"),
            Err(rollback) => {
                format!("保存系统代理设置失败：{error}；安全关闭也失败：{rollback}")
            }
        };
    }
    match apply_owned_system_proxy(operation, port, previous) {
        Ok(ownership) => {
            if expected.system_proxy_ownership.as_ref() == Some(&ownership) {
                return format!("保存系统代理设置失败：{error}；原状态已恢复");
            }
            match store.update(|preferences| {
                preferences.system_proxy_ownership = Some(ownership.clone());
            }) {
                Ok(_) => format!("保存系统代理设置失败：{error}；原状态已恢复"),
                Err(ownership_error) => {
                    let release = operation.release_if_owned(&ownership);
                    match release {
                        Ok(_) => format!(
                            "保存系统代理设置失败：{error}；恢复所有权记录也失败：{ownership_error}，系统代理已安全关闭"
                        ),
                        Err(release_error) => format!(
                            "保存系统代理设置失败：{error}；恢复所有权记录失败：{ownership_error}；安全关闭也失败：{release_error}"
                        ),
                    }
                }
            }
        }
        Err(rollback) => {
            format!("保存系统代理设置失败：{error}；恢复原状态也失败：{rollback}")
        }
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

        assert!(message.contains("Clash Party") && message.contains("7890"));
    }

    #[test]
    fn unavailable_message_rejects_socks_only_system_proxying() {
        let message = unavailable_message(None);

        assert!(message.contains("SOCKS-only") && message.contains("Mixed 或 HTTP"));
    }
}
