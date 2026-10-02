use super::{
    ActiveTheme, App, Button, ButtonVariants, Context, Disableable, FluentBuilder, Focusable, Icon,
    InteractiveElement, IntoElement, Page, ParentElement, Render, RuntimeData, RuntimePage,
    ScrollableElement, Sizable, Styled, Window, div, empty_state, h_flex, message_banner, v_flex,
};
use crate::assets::AppIcon;
use gpui_kit::StatefulInteractiveElement;

impl RuntimePage {
    fn render_header(
        &self,
        theme: &gpui_kit::component::Theme,
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
        let page_icon = if self.page == Page::Home {
            Icon::new(AppIcon::House)
        } else {
            Icon::new(self.page.icon())
        };
        h_flex()
            .min_h_16()
            .px_5()
            .py_4()
            .gap_3()
            .flex_wrap()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .min_w_0()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(page_icon.size_3())
                            .child(zenclash_i18n::text("runtime.design.workspace"))
                            .child("/")
                            .child(self.page.label()),
                    )
                    .child(
                        v_flex()
                            .gap_0()
                            .child(
                                div()
                                    .text_2xl()
                                    .font_weight(gpui_kit::FontWeight::BOLD)
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
                            .icon(AppIcon::RefreshCw)
                            .label(zenclash_i18n::text(if self.loading {
                                "common.actions.loading"
                            } else {
                                "common.actions.refresh"
                            }))
                            .small()
                            .ghost()
                            .loading(self.loading)
                            .disabled(self.core_busy())
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    ),
            )
    }

    fn render_status(&self, theme: &gpui_kit::component::Theme) -> gpui_kit::AnyElement {
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
        &mut self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        // Installation remains reachable when the current controller cannot provide
        // runtime forms. The card reads only the application owner's prepared state.
        if self.page == Page::Tun
            && self.profile_service.service_state().is_some()
            && (self.persistent_loading
                || !self
                    .config_inputs
                    .is_for_profile(self.profile_path.as_deref())
                || matches!(self.data, RuntimeData::Empty))
        {
            return v_flex()
                .gap_4()
                .children(self.render_service_status(theme, cx))
                .child(empty_state(
                    zenclash_i18n::text(if self.persistent_loading || self.loading {
                        "runtime.empty.loading"
                    } else {
                        "runtime.empty.unavailable"
                    }),
                    theme,
                ))
                .into_any_element();
        }
        if self.persistent_loading
            || (matches!(
                self.page,
                Page::Dns | Page::Sniffer | Page::Tun | Page::Mihomo
            ) && !self
                .config_inputs
                .is_for_profile(self.profile_path.as_deref()))
        {
            return empty_state(
                zenclash_i18n::text(if self.persistent_loading || self.config_inputs_loading {
                    "runtime.empty.loading"
                } else {
                    "runtime.empty.unavailable"
                }),
                theme,
            )
            .into_any_element();
        }
        if self.page == Page::Home {
            return self.render_home(theme, cx);
        }
        if matches!(self.data, RuntimeData::Empty)
            && !matches!(
                self.page,
                Page::Logs
                    | Page::Mihomo
                    | Page::Profiles
                    | Page::Override
                    | Page::Traffic
                    | Page::Settings
            )
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
            Page::Home => unreachable!("home is rendered before the empty-data fallback"),
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
    fn focus_handle(&self, _: &App) -> gpui_kit::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RuntimePage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        v_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .child(self.render_header(&theme, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .when(self.page == Page::Settings, |this| {
                        this.child(
                            v_flex()
                                .flex_shrink_0()
                                .min_h_0()
                                .h_full()
                                .px_3()
                                .py_4()
                                .child(self.render_settings_navigation(&theme, cx)),
                        )
                    })
                    .child(
                        v_flex()
                            .id("runtime-body-scroll")
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.settings_navigation.scroll)
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .gap_4()
                                    .px_5()
                                    .py_4()
                                    .child(self.render_status(&theme))
                                    .child(self.render_body(&theme, cx)),
                            )
                            .vertical_scrollbar(&self.settings_navigation.scroll),
                    ),
            )
    }
}
