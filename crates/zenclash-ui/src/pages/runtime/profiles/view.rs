mod catalog;
mod editor;
mod forms;

use gpui_kit::Focusable;

use super::super::{
    Button, Disableable, FluentBuilder, IconName, IntoElement, ParentElement, RuntimeData,
    RuntimePage, Sizable, Styled, empty_dash, empty_state, h_flex, v_flex,
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
                    .h_10()
                    .outline()
                    .disabled(self.core_busy())
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.profiles.forms.adding_subscription {
                            this.close_subscription_form(cx);
                        } else {
                            this.profiles.forms.adding_subscription = true;
                            this.profiles
                                .forms
                                .subscription_name
                                .focus_handle(cx)
                                .focus(window, cx);
                            cx.notify();
                        }
                    })),
            )
            .child(
                Button::new("choose-profile")
                    .icon(IconName::FolderOpen)
                    .label(zenclash_i18n::text("profiles.actions.import_local"))
                    .small()
                    .h_10()
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
                h_flex()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.group_box)
                    .child(
                        h_flex()
                            .flex_wrap()
                            .flex_1()
                            .min_w_0()
                            .child(profile_summary_metric(
                                gpui_kit::component::Icon::new(IconName::File),
                                false,
                                zenclash_i18n::text("profiles.metrics.current"),
                                self.profiles
                                    .active_profile()
                                    .map_or_else(|| empty_dash(""), |profile| profile.name.clone()),
                                theme,
                            ))
                            .child(profile_summary_metric(
                                gpui_kit::component::Icon::new(IconName::Network),
                                true,
                                zenclash_i18n::text("profiles.metrics.proxies"),
                                proxy_count
                                    .map_or_else(|| empty_dash(""), |count| count.to_string()),
                                theme,
                            ))
                            .child(profile_summary_metric(
                                gpui_kit::component::Icon::default()
                                    .path(crate::assets::GROUP_ICON_PATH),
                                true,
                                zenclash_i18n::text("profiles.metrics.groups"),
                                group_count
                                    .map_or_else(|| empty_dash(""), |count| count.to_string()),
                                theme,
                            ))
                            .child(profile_summary_metric(
                                gpui_kit::component::Icon::default()
                                    .path(crate::assets::RULER_ICON_PATH),
                                true,
                                zenclash_i18n::text("profiles.metrics.rules"),
                                rule_count
                                    .map_or_else(|| empty_dash(""), |count| count.to_string()),
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
                            .flex_grow(1.5)
                            .flex_basis(gpui_kit::rems(36.))
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
                            .flex_grow_1()
                            .flex_basis(gpui_kit::rems(24.))
                            .min_w_0()
                            .max_w_full()
                            .gap_4()
                            .child(self.render_profile_inspector(theme, cx))
                            .child(
                                h_flex()
                                    .gap_3()
                                    .p_4()
                                    .border_1()
                                    .border_color(theme.border)
                                    .rounded(theme.radius_lg)
                                    .bg(theme.group_box)
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .gap_2()
                                            .child(
                                                gpui_kit::div()
                                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                    .child(zenclash_i18n::text(
                                                        "profiles.design.overrides",
                                                    )),
                                            )
                                            .child(
                                                gpui_kit::div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(zenclash_i18n::text(
                                                        "profiles.design.overrides_note",
                                                    )),
                                            ),
                                    )
                                    .child(
                                        Button::new("profile-manage-overrides")
                                            .label(zenclash_i18n::text(
                                                "profiles.design.manage_overrides",
                                            ))
                                            .small()
                                            .h_10()
                                            .outline()
                                            .on_click(|_, window, cx| {
                                                crate::components::sidebar::dispatch_navigate(
                                                    crate::pages::Page::Override,
                                                    window,
                                                    cx,
                                                )
                                            }),
                                    ),
                            ),
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

fn profile_summary_metric(
    icon: gpui_kit::component::Icon,
    divider: bool,
    label: String,
    value: String,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    h_flex()
        .flex_1()
        .min_w(gpui_kit::rems(12.))
        .gap_4()
        .px_5()
        .my_4()
        .when(divider, |row| row.border_l_1().border_color(theme.border))
        .child(icon.size_8())
        .child(
            v_flex()
                .min_w_0()
                .gap_1()
                .child(
                    gpui_kit::div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(
                    gpui_kit::div()
                        .truncate()
                        .text_2xl()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(value),
                ),
        )
}
