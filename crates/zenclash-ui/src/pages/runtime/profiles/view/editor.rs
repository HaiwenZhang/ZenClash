use super::super::super::{
    Button, ButtonVariants, Context, Disableable, IconName, Input, ParentElement,
    RemoteProfileRoute, RuntimePage, Sizable, Styled, config_input_row, div, h_flex, px,
    setting_card, setting_switch, v_flex,
};

impl RuntimePage {
    pub(super) fn render_remote_profile_editor(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let name = self
            .profile_forms
            .editing_profile_id
            .as_deref()
            .and_then(|id| {
                self.profile_catalog
                    .profiles
                    .iter()
                    .find(|profile| profile.id == id)
            })
            .map_or_else(
                || zenclash_i18n::text("profiles.editor.generic_name"),
                |profile| profile.name.clone(),
            );
        setting_card(zenclash_i18n::text("profiles.editor.title"), theme)
            .child(
                h_flex()
                    .min_h(px(48.))
                    .px_4()
                    .justify_between()
                    .child(div().text_sm().child(name))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("profiles.editor.save_hint")),
                    ),
            )
            .child(config_input_row(
                zenclash_i18n::text("profiles.form.name"),
                zenclash_i18n::text("profiles.editor.name_description"),
                Input::new(&self.profile_forms.request_name),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("profiles.editor.url"),
                zenclash_i18n::text("profiles.editor.url_description"),
                Input::new(&self.profile_forms.request_url),
                theme,
            ))
            .child(config_input_row(
                "User-Agent",
                zenclash_i18n::text("profiles.editor.user_agent_description"),
                Input::new(&self.profile_forms.request_user_agent),
                theme,
            ))
            .child(config_input_row(
                "Authorization",
                zenclash_i18n::text("profiles.editor.authorization_description"),
                Input::new(&self.profile_forms.request_authorization).mask_toggle(),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("profiles.editor.timeout"),
                zenclash_i18n::text("profiles.editor.timeout_description"),
                Input::new(&self.profile_forms.request_timeout_seconds),
                theme,
            ))
            .child(self.render_remote_profile_route_settings(theme, cx))
            .child(config_input_row(
                zenclash_i18n::text("profiles.editor.cron"),
                zenclash_i18n::text("profiles.editor.cron_description"),
                Input::new(&self.profile_forms.update_cron),
                theme,
            ))
            .child(setting_switch(
                zenclash_i18n::text("profiles.editor.fixed_interval"),
                zenclash_i18n::text("profiles.editor.fixed_interval_description"),
                self.profile_forms.editing_fixed_update_interval,
                "edit-profile-fixed-update-interval",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.profile_forms.editing_fixed_update_interval = *checked;
                    cx.notify();
                }),
            ))
            .child(self.render_remote_profile_editor_actions(cx))
    }

    fn render_remote_profile_route_settings(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        v_flex()
            .child(setting_switch(
                zenclash_i18n::text("profiles.editor.proxy"),
                zenclash_i18n::text("profiles.editor.proxy_description"),
                self.profile_forms.editing_route == RemoteProfileRoute::Mihomo,
                "edit-profile-use-mihomo-proxy",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.profile_forms.editing_route = if *checked {
                        RemoteProfileRoute::Mihomo
                    } else {
                        RemoteProfileRoute::DirectWithMihomoFallback
                    };
                    cx.notify();
                }),
            ))
            .child(setting_switch(
                zenclash_i18n::text("profiles.editor.fallback"),
                zenclash_i18n::text("profiles.editor.fallback_description"),
                self.profile_forms.editing_route == RemoteProfileRoute::DirectWithMihomoFallback,
                "edit-profile-mihomo-fallback",
                theme,
                cx.listener(|this, checked, _, cx| {
                    if this.profile_forms.editing_route != RemoteProfileRoute::Mihomo {
                        this.profile_forms.editing_route = if *checked {
                            RemoteProfileRoute::DirectWithMihomoFallback
                        } else {
                            RemoteProfileRoute::Direct
                        };
                    }
                    cx.notify();
                }),
            ))
    }

    fn render_remote_profile_editor_actions(&self, cx: &mut Context<Self>) -> gpui::Div {
        h_flex()
            .px_4()
            .py_3()
            .gap_2()
            .justify_end()
            .child(
                Button::new("cancel-profile-request-edit")
                    .label(zenclash_i18n::text("profiles.actions.cancel"))
                    .small()
                    .ghost()
                    .disabled(self.mutating)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.cancel_edit_remote_profile(cx);
                    })),
            )
            .child(
                Button::new("save-profile-request-edit")
                    .icon(IconName::Check)
                    .label(zenclash_i18n::text("profiles.actions.save_request"))
                    .small()
                    .primary()
                    .loading(self.mutating)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.save_remote_profile_settings(cx);
                    })),
            )
    }
}
