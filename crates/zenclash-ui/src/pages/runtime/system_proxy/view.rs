use super::SystemProxyEditorState;
use crate::pages::runtime::{
    div, format_port, format_proxy, h_flex, info_row, px, setting_card, setting_switch, v_flex,
    Button, ButtonVariants, Context, Disableable, FluentBuilder, Input, IntoElement, ParentElement,
    RuntimeConfig, RuntimeData, RuntimePage, Selectable, Sizable, Styled, SystemProxyMode,
    SystemProxyStatus,
};

impl RuntimePage {
    pub(in crate::pages::runtime) fn render_system_proxy(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (config, status) = match &self.data {
            RuntimeData::SystemProxy { config, status } => (config.clone(), status.clone()),
            _ => (RuntimeConfig::default(), SystemProxyStatus::default()),
        };
        let active = status.active();
        let port = [config.mixed_port, config.port, config.socks_port]
            .into_iter()
            .find(|port| *port > 0)
            .unwrap_or_default();
        let native_bypass = if status.bypass.is_empty() {
            "无".to_owned()
        } else {
            status.bypass.join(" · ")
        };
        let mode = match self.preferences.system_proxy_mode {
            SystemProxyMode::Manual => "手动 HTTP/HTTPS",
            SystemProxyMode::Pac => "PAC 自动配置",
        };
        let mut card = setting_card("系统代理", theme)
            .child(setting_switch(
                "启用系统代理",
                "控制原生手动代理或本地 PAC 自动配置",
                active,
                "system-proxy-enable",
                theme,
                cx.listener(|this, checked, _, cx| this.toggle_system_proxy(*checked, cx)),
            ))
            .child(self.render_proxy_settings_summary(mode, theme, cx))
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
            .child(info_row(
                "当前 PAC",
                if status.auto_enabled {
                    &status.auto_url
                } else {
                    "未启用"
                },
                theme,
            ))
            .child(info_row("系统回读绕过", &native_bypass, theme))
            .child(info_row("内核代理端口", &format_port(port), theme));
        if let Some(editor) = self.system_proxy_editor.as_ref() {
            card = card.child(self.render_system_proxy_editor(editor, theme, cx));
        }
        card.into_any_element()
    }

    fn render_proxy_settings_summary(
        &self,
        mode: &str,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let detail = match self.preferences.system_proxy_mode {
            SystemProxyMode::Manual if self.preferences.system_proxy_bypass.is_empty() => {
                "不绕过任何地址".to_owned()
            }
            SystemProxyMode::Manual => self.preferences.system_proxy_bypass.join(" · "),
            SystemProxyMode::Pac => format!(
                "{} 行脚本 · 监听 {}",
                self.preferences.system_proxy_pac_script.lines().count(),
                self.preferences.system_proxy_host
            ),
        };
        h_flex()
            .min_h(px(64.))
            .px_4()
            .gap_3()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child(mode.to_owned()))
                    .child(
                        div()
                            .max_w(px(680.))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(detail),
                    ),
            )
            .child(
                Button::new("edit-system-proxy")
                    .label("编辑")
                    .small()
                    .outline()
                    .disabled(self.mutating)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_system_proxy_editor(window, cx);
                    })),
            )
            .into_any_element()
    }

    fn render_system_proxy_editor(
        &self,
        editor: &SystemProxyEditorState,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        v_flex()
            .p_4()
            .gap_4()
            .border_t_1()
            .border_color(theme.border)
            .child(self.render_system_proxy_mode_selector(editor.mode, cx))
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .child(if editor.mode == SystemProxyMode::Pac {
                                "PAC 监听主机"
                            } else {
                                "代理主机"
                            }),
                    )
                    .child(Input::new(&editor.host).disabled(self.mutating)),
            )
            .when(editor.mode == SystemProxyMode::Manual, |this| {
                this.child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child("绕过规则（每行一条）"))
                        .child(Input::new(&editor.bypass).disabled(self.mutating)),
                )
            })
            .when(editor.mode == SystemProxyMode::Pac, |this| {
                this.child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child("PAC JavaScript"))
                        .child(
                            div().text_xs().text_color(theme.muted_foreground).child(
                                "必须定义 FindProxyForURL；%mixed-port% 会替换为当前内核端口",
                            ),
                        )
                        .child(Input::new(&editor.pac_script).disabled(self.mutating)),
                )
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("reset-system-proxy")
                            .label("恢复默认")
                            .small()
                            .outline()
                            .disabled(self.mutating)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reset_system_proxy_editor(window, cx);
                            })),
                    )
                    .child(
                        Button::new("cancel-system-proxy")
                            .label("取消")
                            .small()
                            .ghost()
                            .disabled(self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cancel_system_proxy_editor(cx);
                            })),
                    )
                    .child(
                        Button::new("save-system-proxy")
                            .label("保存并应用")
                            .small()
                            .primary()
                            .disabled(self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.save_system_proxy_editor(cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_system_proxy_mode_selector(
        &self,
        mode: SystemProxyMode,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        v_flex()
            .gap_2()
            .child(div().text_sm().child("代理方式"))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("system-proxy-mode-manual")
                            .label("手动代理")
                            .small()
                            .outline()
                            .selected(mode == SystemProxyMode::Manual)
                            .disabled(self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_system_proxy_editor_mode(SystemProxyMode::Manual, cx);
                            })),
                    )
                    .child(
                        Button::new("system-proxy-mode-pac")
                            .label("PAC 自动配置")
                            .small()
                            .outline()
                            .selected(mode == SystemProxyMode::Pac)
                            .disabled(self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_system_proxy_editor_mode(SystemProxyMode::Pac, cx);
                            })),
                    ),
            )
            .into_any_element()
    }
}
