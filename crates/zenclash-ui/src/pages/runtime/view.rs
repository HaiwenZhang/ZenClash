use super::{
    ActiveTheme, App, Button, Context, Disableable, FluentBuilder, Focusable, InteractiveElement,
    IntoElement, Page, PageSnapshotData, ParentElement, Render, RuntimePage, ScrollableElement,
    Sizable, Styled, Window, div, empty_state, h_flex, v_flex,
};
use crate::{assets::AppIcon, components::workspace};
use gpui_kit::StatefulInteractiveElement;

impl RuntimePage {
    fn render_header(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let refresh = Button::new("refresh-runtime-page")
            .icon(AppIcon::RefreshCw)
            .label(zenclash_i18n::text(if self.loading {
                "common.actions.loading"
            } else if self.page == Page::Network {
                "redesign.recheck"
            } else {
                "common.actions.refresh"
            }))
            .small()
            .h_8()
            .when(matches!(self.page, Page::Rules | Page::Network), |button| {
                button.h_10()
            })
            .outline()
            .loading(
                self.loading
                    || (self.page == Page::Network && self.network_diagnostics.query.is_loading()),
            )
            .disabled(self.core_busy())
            .on_click(cx.listener(|this, _, _, cx| {
                if this.page == Page::Network {
                    this.refresh_network_diagnostics(cx);
                } else {
                    this.refresh(cx);
                }
            }));
        let commands = match self.page {
            Page::Home
            | Page::Settings
            | Page::SystemProxy
            | Page::Tun
            | Page::Dns
            | Page::Sniffer
            | Page::Resources
            | Page::Mihomo => div().into_any_element(),
            Page::Profiles => h_flex()
                .gap_2()
                .flex_wrap()
                .child(self.render_profile_commands(cx))
                .into_any_element(),
            Page::Logs => self.render_log_actions(cx).into_any_element(),
            Page::Connections => self.connections.update(cx, |view, cx| {
                h_flex()
                    .gap_2()
                    .child(view.render_connection_pause(cx))
                    .child(view.render_connection_close_all(cx))
                    .into_any_element()
            }),
            Page::Network => h_flex()
                .gap_2()
                .flex_wrap()
                .child(
                    Button::new("ai-network-diagnostics")
                        .label(zenclash_i18n::text("ai_network.open"))
                        .small()
                        .h_10()
                        .outline()
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_ai_network(window, cx)),
                        ),
                )
                .child(refresh)
                .child(
                    Button::new("export-network-report")
                        .icon(gpui_kit::component::IconName::Copy)
                        .label(zenclash_i18n::text("redesign.export_report"))
                        .small()
                        .h_10()
                        .outline()
                        .disabled(self.network_diagnostics.query.report().is_none())
                        .on_click(cx.listener(|this, _, _, cx| this.export_network_report(cx))),
                )
                .into_any_element(),
            Page::Rules => refresh.into_any_element(),
            _ => refresh.into_any_element(),
        };
        v_flex()
            .px_6()
            .when(!self.page.is_settings(), |view| view.px_8())
            .pt_3()
            .pb_2()
            .when(self.page == Page::Home, |view| view.pb_1())
            .when(self.page == Page::Logs, |view| view.pb_0())
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .items_end()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(workspace::title(
                                if self.page.is_settings() {
                                    Page::Settings
                                } else {
                                    self.page
                                },
                                cx,
                            ))
                            .when(self.page == Page::Logs, |view| {
                                view.child(self.log_stream_status(theme, cx))
                            }),
                    )
                    .child(commands),
            )
    }

    fn render_body(
        &mut self,
        theme: &gpui_kit::component::Theme,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        if self.page == Page::Network {
            return self.render_network(compact, theme, cx);
        }
        // Installation remains reachable when the current controller cannot provide
        // runtime forms. The card reads only the application owner's prepared state.
        if self.page == Page::Tun
            && self.application_commands.service_state().is_some()
            && (self.persistent_loading
                || !self
                    .config_inputs
                    .is_for_profile(self.profile_path.as_deref())
                || matches!(self.page_observation.data(), PageSnapshotData::Empty))
        {
            return v_flex()
                .gap_4()
                .children(self.render_service_details(theme, cx))
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
                Page::Dns | Page::Sniffer | Page::Tun | Page::Mihomo | Page::SystemProxy
            ) && !self
                .config_inputs
                .is_for_profile(self.profile_path.as_deref()))
        {
            return empty_state(
                zenclash_i18n::text(
                    if self.persistent_loading || self.config_inputs.is_loading() {
                        "runtime.empty.loading"
                    } else {
                        "runtime.empty.unavailable"
                    },
                ),
                theme,
            )
            .into_any_element();
        }
        if self.page == Page::Home {
            return self.render_home(theme, compact, cx);
        }
        if matches!(self.page_observation.data(), PageSnapshotData::Empty)
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
            Page::Mihomo => self.render_core(compact, theme, cx),
            Page::Profiles => self.render_profile(compact, theme, cx),
            Page::Connections => self.connections.clone().into_any_element(),
            Page::Rules => self.rules.clone().into_any_element(),
            Page::Resources => self.render_resources(theme, cx),
            Page::Logs => self.render_logs(),
            Page::Tun => self.render_tun(compact, theme, cx),
            Page::Sniffer => self.render_sniffer(compact, theme, cx),
            Page::Traffic => self.render_traffic(theme, cx),
            Page::Network => self.render_network(compact, theme, cx),
            Page::Dns => self.render_dns(compact, theme, cx),
            Page::SystemProxy => self.render_system_proxy(compact, theme, cx),
            Page::Override => self.render_override(theme, cx),
            Page::Settings => self.render_settings(compact, theme, cx),
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        v_flex()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape"
                    && this.page == Page::Profiles
                    && !this.core_busy()
                {
                    match this.profiles.forms.dismissible_form() {
                        Some(super::profiles::ProfileFormDismissal::RemoteEditor) => {
                            this.cancel_edit_remote_profile(cx);
                            cx.stop_propagation();
                        }
                        Some(super::profiles::ProfileFormDismissal::Subscription) => {
                            this.close_subscription_form(cx);
                            cx.stop_propagation();
                        }
                        None => {}
                    }
                }
            }))
            .size_full()
            .bg(crate::design::workspace_background(&theme))
            .child(self.render_header(&theme, cx))
            .when(self.page.is_settings(), |page| {
                page.child(
                    div()
                        .px_6()
                        .child(self.render_settings_navigation(&theme, cx)),
                )
            })
            .child(
                h_flex().flex_1().min_h_0().items_stretch().child(
                    v_flex()
                        .id("runtime-body-scroll")
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.body_scroll)
                        .child(
                            v_flex()
                                .min_w_0()
                                .when(
                                    matches!(
                                        self.page,
                                        Page::Connections | Page::Rules | Page::Logs
                                    ),
                                    |view| view.h_full(),
                                )
                                .gap_4()
                                .px_6()
                                .when(!self.page.is_settings(), |view| view.px_8())
                                .py_3()
                                .child(self.render_body(
                                    &theme,
                                    window.viewport_size().width
                                        < window.rem_size()
                                            * if self.page.is_settings() { 60. } else { 68. },
                                    cx,
                                )),
                        )
                        .vertical_scrollbar(&self.body_scroll),
                ),
            )
    }
}
