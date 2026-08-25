use super::{
    format_port, format_proxy, info_row, load_page, setting_card, setting_switch, Context,
    IntoElement, Page, ParentElement, RuntimeConfig, RuntimeData, RuntimePage, SystemProxyManager,
    SystemProxyStatus,
};

impl RuntimePage {
    fn toggle_system_proxy(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let port = self
            .config()
            .map(|config| [config.mixed_port, config.port, config.socks_port])
            .and_then(|ports| ports.into_iter().find(|port| *port > 0))
            .unwrap_or_default();
        if enabled && port == 0 {
            self.error = Some("Mihomo 当前没有可用的 HTTP/Mixed 监听端口，无法启用系统代理".into());
            cx.notify();
            return;
        }

        let Some(token) = self.begin_mutation(Page::SystemProxy) else {
            return;
        };
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            tokio::task::spawn_blocking(move || {
                let manager = SystemProxyManager::detect().map_err(|error| error.to_string())?;
                manager
                    .set_enabled(enabled, "127.0.0.1", port)
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())??;
            load_page(client, Page::SystemProxy).await
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("系统代理任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data) {
                            this.notice = Some(if enabled {
                                "系统 HTTP/HTTPS 代理已启用".into()
                            } else {
                                "系统 HTTP/HTTPS 代理已停用".into()
                            });
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

    pub(super) fn render_system_proxy(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (config, status) = match &self.data {
            RuntimeData::SystemProxy { config, status } => (config.clone(), status.clone()),
            _ => (RuntimeConfig::default(), SystemProxyStatus::default()),
        };
        let active = status.enabled && status.secure_enabled;
        let port = [config.mixed_port, config.port, config.socks_port]
            .into_iter()
            .find(|port| *port > 0)
            .unwrap_or_default();
        setting_card("系统代理", theme)
            .child(setting_switch(
                "启用系统代理",
                "同步控制桌面系统的 HTTP 与 HTTPS 代理",
                active,
                "system-proxy-enable",
                theme,
                cx.listener(|this, checked, _, cx| this.toggle_system_proxy(*checked, cx)),
            ))
            .child(info_row("网络服务", &status.service, theme))
            .child(info_row(
                "当前 HTTP",
                &format_proxy(&status.server, status.port, status.enabled),
                theme,
            ))
            .child(info_row(
                "当前 HTTPS",
                &format_proxy(
                    &status.secure_server,
                    status.secure_port,
                    status.secure_enabled,
                ),
                theme,
            ))
            .child(info_row("Mihomo 代理端口", &format_port(port), theme))
            .into_any_element()
    }
}
