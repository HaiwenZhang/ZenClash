use super::{
    div, empty_state, h_flex, message_banner, v_flex, ActiveTheme, App, Button, ButtonVariants,
    Context, Disableable, FluentBuilder, Focusable, Icon, IconName, InteractiveElement,
    IntoElement, Page, ParentElement, Render, RuntimeData, RuntimePage, ScrollableElement, Sizable,
    Styled, Window,
};

impl RuntimePage {
    fn render_header(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let traffic = self.traffic_monitor.snapshot();
        let (status, status_color) = if traffic.connected {
            (
                zenclash_i18n::text("runtime.stream.connected"),
                theme.success,
            )
        } else {
            (
                zenclash_i18n::text("runtime.stream.reconnecting"),
                theme.warning,
            )
        };
        h_flex()
            .h_16()
            .px_5()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        Icon::new(self.page.icon())
                            .size_5()
                            .text_color(theme.primary),
                    )
                    .child(
                        v_flex()
                            .gap_0()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child(self.page.label()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(self.page.subtitle()),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(div().size_2().rounded_full().bg(status_color))
                            .child(status),
                    )
                    .child(
                        Button::new("refresh-runtime-page")
                            .icon(IconName::Redo2)
                            .label(zenclash_i18n::text(if self.loading {
                                "common.actions.loading"
                            } else {
                                "common.actions.refresh"
                            }))
                            .small()
                            .ghost()
                            .loading(self.loading)
                            .disabled(self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    ),
            )
    }

    fn render_status(&self, theme: &gpui_component::Theme) -> gpui::AnyElement {
        v_flex()
            .gap_2()
            .when_some(self.startup_error.clone(), |this, error| {
                this.child(message_banner(error, theme.danger, theme))
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(message_banner(error, theme.danger, theme))
            })
            .when_some(self.notice.clone(), |this, notice| {
                this.child(message_banner(notice, theme.success, theme))
            })
            .into_any_element()
    }

    fn render_body(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if matches!(self.data, RuntimeData::Empty) && self.page == Page::Settings {
            return self.render_offline_settings(theme, cx).into_any_element();
        }
        if matches!(self.data, RuntimeData::Empty)
            && !matches!(self.page, Page::Logs | Page::Mihomo)
        {
            return empty_state(
                if self.loading {
                    zenclash_i18n::text("runtime.empty.loading")
                } else {
                    zenclash_i18n::text("runtime.empty.unavailable")
                },
                theme,
            )
            .into_any_element();
        }
        match self.page {
            Page::Home => self.render_home(theme, cx),
            Page::Mihomo => self.render_core(theme, cx),
            Page::Profiles => self.render_profile(theme, cx),
            Page::Connections => self.render_connections(theme, cx),
            Page::Rules => self.render_rules(theme, cx),
            Page::Resources => self.render_resources(theme, cx),
            Page::Logs => self.render_logs(theme, cx),
            Page::Tun => self.render_tun(theme, cx),
            Page::Sniffer => self.render_sniffer(theme, cx),
            Page::Traffic => self.render_traffic(theme, cx),
            Page::Network => self.render_network(theme, cx),
            Page::Dns => self.render_dns(theme, cx),
            Page::SystemProxy => self.render_system_proxy(theme, cx),
            Page::Override => self.render_override(theme, cx),
            Page::Settings => self.render_settings(theme, cx),
            Page::Proxies => div().into_any_element(),
        }
    }
}

impl Focusable for RuntimePage {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RuntimePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh_config_inputs_if_needed(window, cx);
        let theme = cx.theme().clone();
        v_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .child(self.render_header(&theme, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .gap_4()
                    .px_5()
                    .py_4()
                    .child(self.render_status(&theme))
                    .child(self.render_body(&theme, cx)),
            )
    }
}
