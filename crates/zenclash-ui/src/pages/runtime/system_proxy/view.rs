use super::SystemProxyEditorState;
use crate::pages::runtime::settings::forms::settings_status;
use crate::pages::runtime::{
    Button, ButtonVariants, Context, Disableable, FluentBuilder, Input, IntoElement, ParentElement,
    RuntimeConfig, RuntimeData, RuntimePage, Selectable, Sizable, Styled, SystemProxyMode,
    SystemProxyStatus, div, format_proxy, h_flex, info_row, setting_card, v_flex,
};
use gpui_kit::component::WindowExt;
use gpui_kit::component::input::Textarea;

impl RuntimePage {
    pub(in crate::pages::runtime) fn render_system_proxy(
        &self,
        compact: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let (config, status) = match &self.data {
            RuntimeData::SystemProxy { config, status } => (config.clone(), status.clone()),
            _ => (RuntimeConfig::default(), SystemProxyStatus::default()),
        };
        let active = status.active();
        let port = config.system_proxy_port().unwrap_or_default();
        let mode = self.preferences.system_proxy_mode;
        let endpoint = format_proxy(&status.server, status.port, status.enabled);
        let bypass = self.preferences.system_proxy_bypass.clone();
        v_flex()
            .gap_4()
            .child(settings_status(
                zenclash_i18n::text("system_proxy.enable.title"),
                format!("{}:{}", self.preferences.system_proxy_host, port),
                active,
                "system-proxy-enable",
                theme,
                cx.listener(|this, checked, _, cx| this.toggle_system_proxy(*checked, cx)),
            ))
            .child(
                h_flex()
                    .items_start()
                    .gap_4()
                    .when(compact, |view| view.flex_col())
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .when(compact, |view| view.w_full())
                            .child(self.render_core_inputs(theme, cx)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .when(compact, |view| view.w_full())
                            .child(
                                setting_card(zenclash_i18n::text("system_proxy.mode.label"), theme)
                                    .child(
                                        h_flex().p_4().gap_2().children(
                                            [
                                                (
                                                    "manual",
                                                    SystemProxyMode::Manual,
                                                    "system_proxy.mode.manual",
                                                ),
                                                (
                                                    "pac",
                                                    SystemProxyMode::Pac,
                                                    "system_proxy.mode.pac",
                                                ),
                                            ]
                                            .into_iter()
                                            .map(
                                                |(id, value, key)| {
                                                    Button::new(id)
                                                        .outline()
                                                        .selected(mode == value)
                                                        .label(zenclash_i18n::text(key))
                                                        .disabled(self.core_busy())
                                                        .on_click(cx.listener(
                                                            move |this, _, window, cx| {
                                                                this.open_system_proxy_editor(
                                                                    window, cx,
                                                                );
                                                                this.set_system_proxy_editor_mode(
                                                                    value, cx,
                                                                );
                                                            },
                                                        ))
                                                },
                                            ),
                                        ),
                                    )
                                    .child(info_row(
                                        zenclash_i18n::text("system_proxy.fields.http"),
                                        endpoint,
                                        theme,
                                    ))
                                    .child(info_row(
                                        zenclash_i18n::text("system_proxy.fields.pac"),
                                        if status.auto_enabled {
                                            status.auto_url
                                        } else {
                                            "—".into()
                                        },
                                        theme,
                                    ))
                                    .child(
                                        div().p_4().child(
                                            Button::new("edit-system-proxy")
                                                .label(zenclash_i18n::text(
                                                    "system_proxy.editor.edit",
                                                ))
                                                .outline()
                                                .disabled(self.core_busy())
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.open_system_proxy_editor(window, cx)
                                                })),
                                        ),
                                    ),
                            ),
                    ),
            )
            .child(
                setting_card(
                    zenclash_i18n::text("system_proxy.fields.bypass_rules"),
                    theme,
                )
                .child(
                    h_flex()
                        .p_4()
                        .gap_2()
                        .flex_wrap()
                        .children(bypass.into_iter().map(|entry| {
                            div()
                                .px_3()
                                .py_2()
                                .text_sm()
                                .rounded(theme.radius)
                                .bg(theme.background)
                                .child(entry)
                        })),
                )
                .child(
                    div().p_4().child(
                        Button::new("edit-proxy-bypass")
                            .label(zenclash_i18n::text("settings_redesign.edit"))
                            .outline()
                            .disabled(self.core_busy())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_system_proxy_editor(window, cx)
                            })),
                    ),
                ),
            )
            .into_any_element()
    }

    pub(super) fn render_system_proxy_editor(
        &self,
        editor: &SystemProxyEditorState,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
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
                    .child(Input::new(&editor.host).disabled(self.core_busy())),
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
                        .child(Textarea::new(&editor.bypass).disabled(self.core_busy())),
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
                        .child(Textarea::new(&editor.pac_script).disabled(self.core_busy())),
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
                            .disabled(self.core_busy())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reset_system_proxy_editor(window, cx);
                            })),
                    )
                    .child(
                        Button::new("cancel-system-proxy")
                            .label(zenclash_i18n::text("system_proxy.editor.cancel"))
                            .small()
                            .ghost()
                            .disabled(self.core_busy())
                            .on_click(cx.listener(|this, _, window, cx| {
                                window.close_dialog(cx);
                                this.cancel_system_proxy_editor(cx);
                            })),
                    )
                    .child(
                        Button::new("save-system-proxy")
                            .label(zenclash_i18n::text("system_proxy.editor.save"))
                            .small()
                            .primary()
                            .disabled(self.core_busy())
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
    ) -> gpui_kit::AnyElement {
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
                            .disabled(self.core_busy())
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
                            .disabled(self.core_busy())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_system_proxy_editor_mode(SystemProxyMode::Pac, cx);
                            })),
                    ),
            )
            .into_any_element()
    }
}
