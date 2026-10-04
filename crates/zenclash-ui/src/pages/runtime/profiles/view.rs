mod catalog;
mod editor;
mod forms;

use super::super::{
    Button, ButtonVariants, Disableable, FluentBuilder, IconName, IntoElement, ParentElement,
    RuntimeData, RuntimePage, Sizable, Styled, empty_dash, empty_state, h_flex, metric, v_flex,
};

impl RuntimePage {
    pub(in crate::pages::runtime) fn render_profile_commands(
        &self,
        cx: &mut gpui_kit::Context<Self>,
    ) -> gpui_kit::Div {
        h_flex()
            .gap_2()
            .child(
                Button::new("toggle-add-subscription")
                    .icon(if self.profiles.forms.adding_subscription {
                        IconName::Close
                    } else {
                        IconName::Plus
                    })
                    .label(if self.profiles.forms.adding_subscription {
                        zenclash_i18n::text("profiles.actions.collapse_form")
                    } else {
                        zenclash_i18n::text("profiles.actions.add_remote")
                    })
                    .small()
                    .h_8()
                    .primary()
                    .disabled(self.core_busy())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.profiles.forms.adding_subscription =
                            !this.profiles.forms.adding_subscription;
                        if !this.profiles.forms.adding_subscription {
                            this.restore_page_focus(super::super::Page::Profiles, cx);
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("choose-profile")
                    .icon(IconName::FolderOpen)
                    .label(zenclash_i18n::text("profiles.actions.import_local"))
                    .small()
                    .h_8()
                    .outline()
                    .disabled(self.core_busy())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.choose_profile(window, cx);
                    })),
            )
    }

    pub(in super::super) fn render_profile(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut gpui_kit::Context<Self>,
    ) -> gpui_kit::AnyElement {
        let (config, proxy_count, group_count, rule_count) = match &self.data {
            RuntimeData::Profile {
                config,
                proxy_count,
                group_count,
                rule_count,
            } => (config.as_ref(), *proxy_count, *group_count, *rule_count),
            _ => (None, None, None, None),
        };

        v_flex()
            .gap_4()
            .child(
                h_flex().justify_between().gap_3().flex_wrap().child(
                    h_flex()
                        .gap_3()
                        .flex_wrap()
                        .flex_1()
                        .min_w_0()
                        .child(metric(
                            zenclash_i18n::text("profiles.metrics.current"),
                            self.profiles
                                .active_profile()
                                .map_or_else(|| empty_dash(""), |profile| profile.name.clone()),
                            theme,
                        ))
                        .child(metric(
                            zenclash_i18n::text("profiles.metrics.proxies"),
                            proxy_count.map_or_else(|| empty_dash(""), |count| count.to_string()),
                            theme,
                        ))
                        .child(metric(
                            zenclash_i18n::text("profiles.metrics.groups"),
                            group_count.map_or_else(|| empty_dash(""), |count| count.to_string()),
                            theme,
                        ))
                        .child(metric(
                            zenclash_i18n::text("profiles.metrics.rules"),
                            rule_count.map_or_else(|| empty_dash(""), |count| count.to_string()),
                            theme,
                        )),
                ),
            )
            .when(self.profiles.forms.adding_subscription, |this| {
                this.child(self.render_subscription_form(theme, cx))
            })
            .when(
                self.profiles.recovery.is_some() || self.profiles.pending_finalization.is_some(),
                |this| this.child(self.render_profile_recovery(theme, cx)),
            )
            .child(
                h_flex()
                    .gap_4()
                    .items_start()
                    .flex_wrap()
                    .child(
                        v_flex()
                            .flex_1()
                            .flex_basis(gpui_kit::rems(30.))
                            .min_w_0()
                            .max_w_full()
                            .gap_4()
                            .child(self.render_managed_profiles(theme, cx))
                            .when_some(config, |this, config| {
                                this.child(self.render_current_profile(config, theme, cx))
                            }),
                    )
                    .child(
                        v_flex()
                            .w(gpui_kit::rems(24.))
                            .flex_shrink_0()
                            .max_w_full()
                            .child(self.render_profile_inspector(theme, cx)),
                    ),
            )
            .when(self.profiles.forms.editing_profile_id.is_some(), |this| {
                this.child(self.render_remote_profile_editor(theme, cx))
            })
            .when(config.is_none(), |this| {
                this.child(empty_state(
                    zenclash_i18n::text("runtime.empty.unavailable"),
                    theme,
                ))
            })
            .into_any_element()
    }
}
