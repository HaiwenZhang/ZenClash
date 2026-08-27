mod catalog;
mod editor;
mod forms;

use super::super::{
    Button, ButtonVariants, Disableable, FluentBuilder, IconName, IntoElement, ParentElement,
    RuntimeConfig, RuntimeData, RuntimePage, Sizable, Styled, h_flex, metric, v_flex,
};

impl RuntimePage {
    pub(in super::super) fn render_profile(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut gpui::Context<Self>,
    ) -> gpui::AnyElement {
        let (config, proxy_count, group_count, rule_count) = match &self.data {
            RuntimeData::Profile {
                config,
                proxy_count,
                group_count,
                rule_count,
            } => (config.clone(), *proxy_count, *group_count, *rule_count),
            _ => (RuntimeConfig::default(), 0, 0, 0),
        };

        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .justify_between()
                    .gap_3()
                    .flex_wrap()
                    .child(
                        h_flex()
                            .gap_3()
                            .flex_wrap()
                            .child(metric(
                                zenclash_i18n::text("profiles.metrics.proxies"),
                                proxy_count.to_string(),
                                theme.primary,
                                theme,
                            ))
                            .child(metric(
                                zenclash_i18n::text("profiles.metrics.groups"),
                                group_count.to_string(),
                                theme.success,
                                theme,
                            ))
                            .child(metric(
                                zenclash_i18n::text("profiles.metrics.rules"),
                                rule_count.to_string(),
                                theme.warning,
                                theme,
                            )),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("toggle-add-subscription")
                                    .icon(if self.profile_forms.adding_subscription {
                                        IconName::Close
                                    } else {
                                        IconName::Plus
                                    })
                                    .label(if self.profile_forms.adding_subscription {
                                        zenclash_i18n::text("profiles.actions.collapse_form")
                                    } else {
                                        zenclash_i18n::text("profiles.actions.add_remote")
                                    })
                                    .small()
                                    .outline()
                                    .disabled(self.mutating)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.profile_forms.adding_subscription =
                                            !this.profile_forms.adding_subscription;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("choose-profile")
                                    .icon(IconName::FolderOpen)
                                    .label(zenclash_i18n::text("profiles.actions.import_local"))
                                    .small()
                                    .primary()
                                    .disabled(self.mutating)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.choose_profile(window, cx);
                                    })),
                            ),
                    ),
            )
            .when(self.profile_forms.adding_subscription, |this| {
                this.child(self.render_subscription_form(theme, cx))
            })
            .child(self.render_managed_profiles(theme, cx))
            .when(self.profile_forms.editing_profile_id.is_some(), |this| {
                this.child(self.render_remote_profile_editor(theme, cx))
            })
            .child(self.render_current_profile(&config, theme, cx))
            .into_any_element()
    }
}
