use super::{
    div, h_flex, message_banner, px, v_flex, ActiveTheme, App, Button, Context, Disableable,
    FluentBuilder, Focusable, Icon, IconName, InteractiveElement, IntoElement, Page, ParentElement,
    Render, RuntimePage, ScrollableElement, Sizable, Styled, Window,
};

impl RuntimePage {
    fn render_header(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let loading = self.loading;
        h_flex()
            .h(px(86.))
            .px_6()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .size(px(38.))
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.secondary)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(self.page.icon())
                                    .size_5()
                                    .text_color(theme.primary),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_0()
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme.primary)
                                    .child(format!(
                                        "{} / {}",
                                        self.page.section_label(),
                                        self.page.route().to_ascii_uppercase()
                                    )),
                            )
                            .child(
                                div()
                                    .text_xl()
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
                Button::new("refresh-runtime-page")
                    .icon(IconName::Redo2)
                    .label(if loading { "读取中" } else { "刷新" })
                    .small()
                    .outline()
                    .loading(loading)
                    .disabled(self.mutating)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
    }

    fn render_status(&self, theme: &gpui_component::Theme) -> gpui::AnyElement {
        v_flex()
            .gap_2()
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
        match self.page {
            Page::Mihomo => self.render_core(theme, cx),
            Page::Profiles => self.render_profile(theme, cx),
            Page::Connections => self.render_connections(theme, cx),
            Page::Rules => self.render_rules(theme),
            Page::Resources => self.render_resources(theme, cx),
            Page::Logs => self.render_logs(theme),
            Page::Tun => self.render_tun(theme, cx),
            Page::Sniffer => self.render_sniffer(theme),
            Page::Traffic => self.render_traffic(theme),
            Page::Network => self.render_network(theme),
            Page::Dns => self.render_dns(theme),
            Page::SystemProxy => self.render_system_proxy(theme, cx),
            Page::Override => self.render_override(theme, cx),
            Page::SubStore => self.render_substore(theme, cx),
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                    .gap_5()
                    .px_6()
                    .py_5()
                    .child(self.render_status(&theme))
                    .child(self.render_body(&theme, cx)),
            )
    }
}
