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
        let port = config.system_proxy_port().unwrap_or_default();
        let native_bypass = if status.bypass.is_empty() {
            zenclash_i18n::text("system_proxy.status.none")
        } else {
            status.bypass.join(" · ")
        };
        let mode = match self.preferences.system_proxy_mode {
            SystemProxyMode::Manual => zenclash_i18n::text("system_proxy.mode.manual_summary"),
            SystemProxyMode::Pac => zenclash_i18n::text("system_proxy.mode.pac"),
        };
        let current_pac = if status.auto_enabled {
            status.auto_url.clone()
        } else {
            zenclash_i18n::text("system_proxy.status.disabled")
        };
        let mut card = setting_card(zenclash_i18n::text("system_proxy.title"), theme)
            .child(setting_switch(
                zenclash_i18n::text("system_proxy.enable.title"),
                zenclash_i18n::text("system_proxy.enable.description"),
                active,
                "system-proxy-enable",
                theme,
                cx.listener(|this, checked, _, cx| this.toggle_system_proxy(*checked, cx)),
            ))
            .child(self.render_proxy_settings_summary(&mode, theme, cx))
            .child(info_row(
                zenclash_i18n::text("system_proxy.fields.service"),
                &status.service,
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("system_proxy.fields.http"),
                format_proxy(&status.server, status.port, status.enabled),
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("system_proxy.fields.https"),
                format_proxy(
                    &status.secure_server,
                    status.secure_port,
                    status.secure_enabled,
                ),
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("system_proxy.fields.pac"),
                current_pac,
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("system_proxy.fields.bypass"),
                &native_bypass,
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("system_proxy.fields.port"),
                format_port(port),
                theme,
            ));
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
                zenclash_i18n::text("system_proxy.status.no_bypass")
            }
            SystemProxyMode::Manual => self.preferences.system_proxy_bypass.join(" · "),
            SystemProxyMode::Pac => zenclash_i18n::text_with(
                "system_proxy.status.pac_summary",
                &[
                    (
                        "lines",
                        self.preferences
                            .system_proxy_pac_script
                            .lines()
                            .count()
                            .to_string(),
                    ),
                    ("host", self.preferences.system_proxy_host.clone()),
                ],
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
                    .label(zenclash_i18n::text("system_proxy.editor.edit"))
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
                                zenclash_i18n::text("system_proxy.fields.pac_host")
                            } else {
                                zenclash_i18n::text("system_proxy.fields.host")
                            }),
                    )
                    .child(Input::new(&editor.host).disabled(self.mutating)),
            )
            .when(editor.mode == SystemProxyMode::Manual, |this| {
                this.child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .child(zenclash_i18n::text("system_proxy.fields.bypass_rules")),
                        )
                        .child(Input::new(&editor.bypass).disabled(self.mutating)),
                )
            })
            .when(editor.mode == SystemProxyMode::Pac, |this| {
                this.child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .child(zenclash_i18n::text("system_proxy.fields.pac_script")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(zenclash_i18n::text("system_proxy.editor.pac_help")),
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
                            .label(zenclash_i18n::text("system_proxy.editor.reset"))
                            .small()
                            .outline()
                            .disabled(self.mutating)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reset_system_proxy_editor(window, cx);
                            })),
                    )
                    .child(
                        Button::new("cancel-system-proxy")
                            .label(zenclash_i18n::text("system_proxy.editor.cancel"))
                            .small()
                            .ghost()
                            .disabled(self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cancel_system_proxy_editor(cx);
                            })),
                    )
                    .child(
                        Button::new("save-system-proxy")
                            .label(zenclash_i18n::text("system_proxy.editor.save"))
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
            .child(
                div()
                    .text_sm()
                    .child(zenclash_i18n::text("system_proxy.mode.label")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("system-proxy-mode-manual")
                            .label(zenclash_i18n::text("system_proxy.mode.manual"))
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
                            .label(zenclash_i18n::text("system_proxy.mode.pac"))
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
