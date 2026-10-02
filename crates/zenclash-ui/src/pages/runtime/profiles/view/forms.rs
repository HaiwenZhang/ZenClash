use zenclash_core::RuntimeConfig;

use super::super::super::{
    Button, ButtonVariants, Context, Disableable, FluentBuilder, Icon, IconName, Input,
    ParentElement, RemoteProfileRoute, RuntimePage, Sizable, Styled, Switch, div, h_flex, info_row,
    message_banner, px, setting_card, v_flex,
};

impl RuntimePage {
    pub(super) fn render_subscription_form(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        setting_card(zenclash_i18n::text("profiles.form.title"), theme).child(
            v_flex()
                .p_4()
                .gap_3()
                .child(
                    h_flex()
                        .gap_3()
                        .child(subscription_input(
                            zenclash_i18n::text("profiles.form.name"),
                            Input::new(&self.profiles.forms.subscription_name)
                                .prefix(Icon::new(IconName::File))
                                .cleanable(true),
                            theme,
                        ))
                        .child(
                            subscription_input(
                                zenclash_i18n::text("profiles.form.user_agent"),
                                Input::new(&self.profiles.forms.subscription_user_agent)
                                    .prefix(Icon::new(IconName::Bot))
                                    .cleanable(true),
                                theme,
                            )
                            .w(px(220.)),
                        ),
                )
                .child(subscription_input(
                    zenclash_i18n::text("profiles.form.url"),
                    Input::new(&self.profiles.forms.subscription_url)
                        .prefix(Icon::new(IconName::Globe))
                        .cleanable(true),
                    theme,
                ))
                .child(
                    h_flex()
                        .gap_3()
                        .child(subscription_input(
                            zenclash_i18n::text("profiles.form.authorization"),
                            Input::new(&self.profiles.forms.subscription_authorization)
                                .prefix(Icon::new(IconName::Asterisk))
                                .mask_toggle()
                                .cleanable(true),
                            theme,
                        ))
                        .child(self.render_subscription_route_controls(theme, cx)),
                )
                .when_some(
                    self.profiles.forms.subscription_error.clone(),
                    |this, error| this.child(message_banner(error, theme.danger, theme)),
                )
                .child(
                    h_flex()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(zenclash_i18n::text("profiles.form.validation_note")),
                        )
                        .child(
                            Button::new("download-subscription")
                                .icon(IconName::Inbox)
                                .label(zenclash_i18n::text("profiles.actions.download_enable"))
                                .primary()
                                .loading(self.core_busy())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.add_remote_profile(cx);
                                })),
                        ),
                ),
        )
    }

    fn render_subscription_route_controls(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        v_flex()
            .w(px(220.))
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Switch::new("subscription-use-mihomo-proxy")
                            .checked(
                                self.profiles.forms.subscription_route
                                    == RemoteProfileRoute::Mihomo,
                            )
                            .disabled(self.core_busy())
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.profiles.forms.subscription_route = if *checked {
                                    RemoteProfileRoute::Mihomo
                                } else {
                                    RemoteProfileRoute::DirectWithMihomoFallback
                                };
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .child(zenclash_i18n::text("profiles.form.always_proxy")),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Switch::new("subscription-mihomo-fallback")
                            .checked(
                                self.profiles.forms.subscription_route
                                    == RemoteProfileRoute::DirectWithMihomoFallback,
                            )
                            .disabled(
                                self.core_busy()
                                    || self.profiles.forms.subscription_route
                                        == RemoteProfileRoute::Mihomo,
                            )
                            .on_click(cx.listener(|this, checked, _, cx| {
                                if this.profiles.forms.subscription_route
                                    != RemoteProfileRoute::Mihomo
                                {
                                    this.profiles.forms.subscription_route = if *checked {
                                        RemoteProfileRoute::DirectWithMihomoFallback
                                    } else {
                                        RemoteProfileRoute::Direct
                                    };
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .child(zenclash_i18n::text("profiles.form.fallback")),
                            )
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(theme.muted_foreground)
                                    .child(zenclash_i18n::text(
                                        "profiles.form.fallback_description",
                                    )),
                            ),
                    ),
            )
    }

    pub(super) fn render_current_profile(
        &self,
        config: &RuntimeConfig,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let path = self.profile_path.as_ref().map_or_else(
            || zenclash_i18n::text("profiles.current.unspecified"),
            |path| path.display().to_string(),
        );
        let remote_count = self.profiles.forms.catalog_view.remote_count;

        setting_card(zenclash_i18n::text("profiles.current.title"), theme)
            .child(runtime_config_preview(config, theme))
            .child(info_row(
                zenclash_i18n::text("profiles.current.path"),
                &path,
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("profiles.current.mode"),
                &config.mode,
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("profiles.current.log_level"),
                &config.log_level,
                theme,
            ))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .p_3()
                    .flex_wrap()
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        zenclash_i18n::text_with(
                            "profiles.current.counts",
                            &[
                                ("managed", self.profiles.catalog.profiles.len().to_string()),
                                ("remote", remote_count.to_string()),
                            ],
                        ),
                    ))
                    .child(configuration_edit_button(
                        "edit-current-profile-config",
                        true,
                    ))
                    .child(
                        Button::new("reload-profile")
                            .icon(crate::assets::AppIcon::RefreshCw)
                            .label(zenclash_i18n::text("profiles.actions.reload"))
                            .primary()
                            .loading(self.core_busy())
                            .on_click(cx.listener(|this, _, _, cx| this.reload_profile(cx))),
                    ),
            )
    }
}

pub(super) fn configuration_edit_button(
    id: impl Into<gpui_kit::ElementId>,
    enabled: bool,
) -> Button {
    Button::new(id)
        .icon(IconName::Settings2)
        .label(zenclash_i18n::text("profiles.actions.edit_yaml"))
        .small()
        .outline()
        .disabled(!enabled)
        .when(!enabled, |this| {
            this.tooltip(zenclash_i18n::text("profiles.actions.activate_first"))
        })
        .on_click(|_, window, cx| {
            crate::components::sidebar::dispatch_navigate(crate::pages::Page::Override, window, cx);
        })
}

fn subscription_input(
    label: String,
    input: Input,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    v_flex()
        .flex_1()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(input)
}

fn runtime_config_preview(
    config: &RuntimeConfig,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    let fields = [
        ("mixed-port", config.mixed_port.to_string()),
        ("mode", config.mode.clone()),
        ("log-level", config.log_level.clone()),
        ("ipv6", config.ipv6.to_string()),
        ("tun.enable", config.tun.enable.to_string()),
    ];
    v_flex()
        .m_3()
        .p_3()
        .gap_2()
        .rounded(theme.radius)
        .bg(theme.muted.opacity(0.5))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(zenclash_i18n::text("profiles.design.runtime_preview")),
        )
        .children(fields.into_iter().enumerate().map(|(index, (key, value))| {
            h_flex()
                .gap_3()
                .text_sm()
                .font_family(theme.mono_font_family.clone())
                .child(
                    div()
                        .w_6()
                        .text_right()
                        .text_color(theme.muted_foreground)
                        .child((index + 1).to_string()),
                )
                .child(div().text_color(theme.chart_1).child(format!("{key}:")))
                .child(div().text_color(theme.foreground).child(value))
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AppContext, IntoElement, Render, TestAppContext, Window, size};
    use std::{cell::Cell, rc::Rc};

    struct ConfigurationEditorEntries;

    impl Render for ConfigurationEditorEntries {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            h_flex()
                .child(configuration_edit_button(
                    "edit-current-profile-config",
                    true,
                ))
                .child(configuration_edit_button("edit-active-local-config", true))
                .child(configuration_edit_button(
                    "edit-inactive-local-config",
                    false,
                ))
        }
    }

    #[gpui_kit::test]
    fn only_current_or_active_configuration_entries_dispatch_the_real_editing_route(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let requests = Rc::new(Cell::new(0));
        let received = requests.clone();
        cx.update(|cx| {
            cx.on_action(move |_: &crate::app::NavigateOverride, _| {
                received.set(received.get() + 1);
            });
        });
        let window: gpui_kit::AnyWindowHandle = cx
            .open_window(size(px(900.), px(700.)), |window, cx| {
                let entries = cx.new(|_| ConfigurationEditorEntries);
                Root::new(entries, window, cx)
            })
            .into();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("edit-inactive-local-config", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(
            requests.get(),
            0,
            "inactive profiles must not open another profile's editor"
        );
        for (id, expected) in [
            ("edit-active-local-config", 1),
            ("edit-current-profile-config", 2),
        ] {
            cx.update_window(window, |_, window, cx| window.click(id, cx))
                .unwrap();
            cx.run_until_parked();
            assert_eq!(requests.get(), expected);
        }
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
    }
}
