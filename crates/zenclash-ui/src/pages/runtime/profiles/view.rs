mod catalog;
mod editor;
mod forms;

use super::super::{
    h_flex, metric, v_flex, FluentBuilder, IntoElement, ParentElement, RuntimeConfig, RuntimeData,
    RuntimePage, Styled,
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
                    .gap_3()
                    .flex_wrap()
                    .child(metric(
                        "代理对象",
                        proxy_count.to_string(),
                        theme.primary,
                        theme,
                    ))
                    .child(metric(
                        "策略组",
                        group_count.to_string(),
                        theme.success,
                        theme,
                    ))
                    .child(metric("规则", rule_count.to_string(), theme.warning, theme)),
            )
            .child(self.render_subscription_form(theme, cx))
            .child(self.render_local_import(theme, cx))
            .child(self.render_managed_profiles(theme, cx))
            .when(self.profile_forms.editing_profile_id.is_some(), |this| {
                this.child(self.render_remote_profile_editor(theme, cx))
            })
            .child(self.render_current_profile(&config, theme, cx))
            .into_any_element()
    }
}
