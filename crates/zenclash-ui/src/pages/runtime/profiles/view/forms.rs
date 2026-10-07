use zenclash_core::RuntimeConfig;

use super::super::super::{
    Button, ButtonVariants, Context, Disableable, FluentBuilder, Icon, IconName, Input,
    ParentElement, RemoteProfileRoute, RuntimePage, Sizable, Styled, div, h_flex, px, v_flex,
};

use crate::components::mint_switch::MintSwitch as Switch;
use gpui_kit::{InteractiveElement, TestSupportExt};

impl RuntimePage {
    pub(in crate::pages::runtime) fn render_subscription_form(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .child(subscription_input(
                        zenclash_i18n::text("settings_redesign.subscription_name_optional"),
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
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(zenclash_i18n::text("profiles.form.validation_note")),
            )
            .when_some(
                self.profiles.forms.subscription_error.as_ref(),
                |form, error| {
                    form.child(
                        div()
                            .text_sm()
                            .text_color(theme.danger)
                            .child(error.clone()),
                    )
                },
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-add-subscription")
                            .outline()
                            .label(zenclash_i18n::text("common.actions.cancel"))
                            .disabled(self.core_busy())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_subscription_form(cx);
                                gpui_kit::component::WindowExt::close_dialog(window, cx);
                            })),
                    )
                    .child(
                        Button::new("download-subscription")
                            .icon(IconName::Inbox)
                            .label(zenclash_i18n::text("profiles.actions.download_enable"))
                            .primary()
                            .loading(self.core_busy())
                            .disabled(self.core_busy())
                            .on_click(cx.listener(|this, _, _, cx| this.add_remote_profile(cx))),
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
        config: Option<&RuntimeConfig>,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let path = self.profile_path.as_ref().map_or_else(
            || zenclash_i18n::text("profiles.current.unspecified"),
            |path| path.display().to_string(),
        );
        v_flex()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .p_3()
                    .flex_wrap()
                    .child(
                        div()
                            .flex_1()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(zenclash_i18n::text("profiles.current.title")),
                    )
                    .child(
                        configuration_edit_button("edit-current-profile-config", true)
                            .label(zenclash_i18n::text("profiles.design.open_editor"))
                            .tooltip(path),
                    )
                    .child(
                        Button::new("reload-profile")
                            .icon(crate::assets::AppIcon::RefreshCw)
                            .label(zenclash_i18n::text("profiles.actions.reload"))
                            .small()
                            .h_10()
                            .outline()
                            .loading(self.core_busy())
                            .on_click(cx.listener(|this, _, _, cx| this.reload_profile(cx))),
                    ),
            )
            .child(self.render_profile_statistics(theme))
            .when_some(config, |view, config| {
                view.child(runtime_config_preview(config, theme))
            })
            .when(config.is_none(), |view| {
                view.child(
                    div()
                        .m_3()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(zenclash_i18n::text("runtime.empty.unavailable")),
                )
            })
    }
    fn render_profile_statistics(&self, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
        let (proxies, groups, rules) = match &self.data {
            super::super::super::RuntimeData::Profile {
                proxy_count,
                group_count,
                rule_count,
                ..
            } => (*proxy_count, *group_count, *rule_count),
            _ => (None, None, None),
        };
        h_flex().px_3().gap_3().children(
            [
                (
                    "profiles.metrics.proxies",
                    proxies,
                    gpui_kit::assets::IconName::Globe,
                ),
                (
                    "profiles.metrics.groups",
                    groups,
                    gpui_kit::assets::IconName::Layers,
                ),
                (
                    "profiles.metrics.rules",
                    rules,
                    gpui_kit::assets::IconName::File,
                ),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, (key, count, icon))| {
                h_flex()
                    .id(("profile-statistic", index))
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .p_3()
                    .gap_3()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.group_box)
                    .child(
                        div()
                            .size_11()
                            .flex_shrink_0()
                            .rounded(theme.radius)
                            .bg(crate::design::workspace_background(theme))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(icon).size_6()),
                    )
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(zenclash_i18n::text(key)),
                            )
                            .child(
                                div()
                                    .text_2xl()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .child(count.map_or_else(
                                        || "—".into(),
                                        |value| {
                                            let digits = value.to_string();
                                            digits.chars().enumerate().fold(
                                                String::new(),
                                                |mut text, (index, digit)| {
                                                    if index > 0
                                                        && (digits.len() - index).is_multiple_of(3)
                                                    {
                                                        text.push(',');
                                                    }
                                                    text.push(digit);
                                                    text
                                                },
                                            )
                                        },
                                    )),
                            ),
                    )
            }),
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
        .h_10()
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
        (
            zenclash_i18n::text("profiles.current.mixed_port"),
            config.mixed_port.to_string(),
        ),
        (
            zenclash_i18n::text("profiles.current.mode"),
            match config.mode.as_str() {
                "rule" => zenclash_i18n::text("home.controls.mode_rule"),
                "global" => zenclash_i18n::text("home.controls.mode_global"),
                "direct" => zenclash_i18n::text("home.controls.mode_direct"),
                _ => super::super::super::empty_dash(&config.mode),
            },
        ),
        (
            zenclash_i18n::text("profiles.current.log_level"),
            super::super::super::empty_dash(&config.log_level),
        ),
        ("IPv6".to_owned(), super::super::super::yes_no(config.ipv6)),
        (
            zenclash_i18n::text("home.controls.tun"),
            super::super::super::yes_no(config.tun.enable),
        ),
    ];
    v_flex()
        .m_3()
        .p_3()
        .gap_2()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .children(fields.into_iter().map(|(key, value)| {
            h_flex()
                .gap_3()
                .text_sm()
                .child(div().w_32().text_color(theme.muted_foreground).child(key))
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
