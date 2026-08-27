use zenclash_core::RuntimeConfig;

use super::super::super::{
    div, h_flex, info_row, px, setting_card, v_flex, Button, ButtonVariants, Context, Disableable,
    Icon, IconName, Input, ParentElement, RemoteProfileRoute, RuntimePage, Styled, Switch,
};

impl RuntimePage {
    pub(super) fn render_subscription_form(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        setting_card(zenclash_i18n::text("profiles.form.title"), theme).child(
            v_flex()
                .p_4()
                .gap_3()
                .child(
                    h_flex()
                        .gap_3()
                        .child(subscription_input(
                            zenclash_i18n::text("profiles.form.name"),
                            Input::new(&self.profile_forms.subscription_name)
                                .prefix(Icon::new(IconName::File))
                                .cleanable(true),
                            theme,
                        ))
                        .child(
                            subscription_input(
                                zenclash_i18n::text("profiles.form.user_agent"),
                                Input::new(&self.profile_forms.subscription_user_agent)
                                    .prefix(Icon::new(IconName::Bot))
                                    .cleanable(true),
                                theme,
                            )
                            .w(px(220.)),
                        ),
                )
                .child(subscription_input(
                    zenclash_i18n::text("profiles.form.url"),
                    Input::new(&self.profile_forms.subscription_url)
                        .prefix(Icon::new(IconName::Globe))
                        .cleanable(true),
                    theme,
                ))
                .child(
                    h_flex()
                        .gap_3()
                        .child(subscription_input(
                            zenclash_i18n::text("profiles.form.authorization"),
                            Input::new(&self.profile_forms.subscription_authorization)
                                .prefix(Icon::new(IconName::Asterisk))
                                .mask_toggle()
                                .cleanable(true),
                            theme,
                        ))
                        .child(self.render_subscription_route_controls(theme, cx)),
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
                                .loading(self.mutating)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.add_remote_profile(cx);
                                })),
                        ),
                ),
        )
    }

    fn render_subscription_route_controls(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        v_flex()
            .w(px(220.))
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Switch::new("subscription-use-mihomo-proxy")
                            .checked(
                                self.profile_forms.subscription_route == RemoteProfileRoute::Mihomo,
                            )
                            .disabled(self.mutating)
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.profile_forms.subscription_route = if *checked {
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
                                self.profile_forms.subscription_route
                                    == RemoteProfileRoute::DirectWithMihomoFallback,
                            )
                            .disabled(
                                self.mutating
                                    || self.profile_forms.subscription_route
                                        == RemoteProfileRoute::Mihomo,
                            )
                            .on_click(cx.listener(|this, checked, _, cx| {
                                if this.profile_forms.subscription_route
                                    != RemoteProfileRoute::Mihomo
                                {
                                    this.profile_forms.subscription_route = if *checked {
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
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let path = self.profile_path.as_ref().map_or_else(
            || zenclash_i18n::text("profiles.current.unspecified"),
            |path| path.display().to_string(),
        );
        let remote_count = self
            .profile_catalog
            .profiles
            .iter()
            .filter(|profile| profile.is_remote())
            .count();

        setting_card(zenclash_i18n::text("profiles.current.title"), theme)
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
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        zenclash_i18n::text_with(
                            "profiles.current.counts",
                            &[
                                ("managed", self.profile_catalog.profiles.len().to_string()),
                                ("remote", remote_count.to_string()),
                            ],
                        ),
                    ))
                    .child(
                        Button::new("reload-profile")
                            .icon(IconName::Redo2)
                            .label(zenclash_i18n::text("profiles.actions.reload"))
                            .primary()
                            .loading(self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| this.reload_profile(cx))),
                    ),
            )
    }
}

fn subscription_input(label: String, input: Input, theme: &gpui_component::Theme) -> gpui::Div {
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
