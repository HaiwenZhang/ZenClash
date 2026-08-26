use std::time::{SystemTime, UNIX_EPOCH};

use zenclash_core::{ProfileRecord, ProfileSource, SubscriptionUsage};

use super::super::super::{
    compact_text, div, empty_state, format_bytes, format_profile_age, h_flex, px, setting_card,
    v_flex, Button, ButtonVariants, Context, Disableable, FluentBuilder, IconName, IntoElement,
    ParentElement, RemoteProfileRoute, RuntimePage, Sizable, Styled, Switch,
};

const UPDATE_INTERVALS: [u32; 4] = [60, 6 * 60, 12 * 60, 24 * 60];

impl RuntimePage {
    pub(super) fn render_managed_profiles(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mut card = setting_card("配置仓库", theme);
        if self.profile_catalog.profiles.is_empty() {
            return card.child(empty_state(
                "还没有托管配置；可下载在线订阅或导入本地 YAML",
                theme,
            ));
        }

        for (index, profile) in self.profile_catalog.profiles.iter().enumerate() {
            card = card.child(self.render_managed_profile(index, profile, theme, cx));
        }
        card
    }

    fn render_managed_profile(
        &self,
        index: usize,
        profile: &ProfileRecord,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let active = self.profile_catalog.active.as_deref() == Some(profile.id.as_str());
        let source = profile_source(&profile.source);

        v_flex()
            .px_4()
            .py_3()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(profile_heading(profile, active, theme))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(source),
            )
            .when(profile.is_remote(), |this| {
                this.child(self.render_profile_update_policy(index, profile, theme, cx))
            })
            .when_some(profile.subscription.usage.as_ref(), |this, usage| {
                this.child(render_subscription_usage(usage, theme))
            })
            .when_some(
                profile.subscription.home_url.as_deref(),
                |this, home_url| {
                    this.child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme.muted_foreground)
                            .child(format!("订阅主页：{}", compact_text(home_url, 90))),
                    )
                },
            )
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme.muted_foreground)
                            .child(format!("更新于 {}", format_profile_age(profile.updated_at))),
                    )
                    .child(self.render_profile_actions(index, profile, active, cx)),
            )
    }

    fn render_profile_actions(
        &self,
        index: usize,
        profile: &ProfileRecord,
        active: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let activate_id = profile.id.clone();
        let update_id = profile.id.clone();
        let edit_id = profile.id.clone();
        let delete_id = profile.id.clone();
        h_flex()
            .gap_2()
            .when(profile.is_remote(), |this| {
                this.child(
                    Button::new(("edit-profile-request", index))
                        .icon(IconName::Settings2)
                        .label("请求设置")
                        .small()
                        .ghost()
                        .disabled(self.mutating)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.begin_edit_remote_profile(edit_id.clone(), window, cx);
                        })),
                )
                .child(
                    Button::new(("update-profile", index))
                        .icon(IconName::Redo2)
                        .label("更新")
                        .small()
                        .outline()
                        .disabled(self.mutating)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.update_managed_profile(update_id.clone(), cx);
                        })),
                )
            })
            .child(
                Button::new(("activate-profile", index))
                    .icon(IconName::ArrowRight)
                    .label(if active { "使用中" } else { "切换" })
                    .small()
                    .primary()
                    .disabled(active || self.mutating)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.activate_managed_profile(activate_id.clone(), cx);
                    })),
            )
            .child(
                Button::new(("delete-profile", index))
                    .icon(IconName::Delete)
                    .small()
                    .ghost()
                    .danger()
                    .disabled(active || self.mutating)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.delete_managed_profile(delete_id.clone(), cx);
                    })),
            )
    }

    fn render_profile_update_policy(
        &self,
        index: usize,
        profile: &ProfileRecord,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let interval_minutes = profile.update_interval_minutes;
        let update_cron = profile.update_cron.clone();
        let auto_update = profile.auto_update;
        let policy_id = profile.id.clone();
        let interval_id = profile.id.clone();
        h_flex()
            .min_h(px(30.))
            .gap_3()
            .child(
                Switch::new(("auto-update-profile", index))
                    .checked(auto_update)
                    .disabled(self.mutating)
                    .on_click(cx.listener(move |this, enabled, _, cx| {
                        this.set_profile_update_policy(
                            policy_id.clone(),
                            *enabled,
                            interval_minutes,
                            cx,
                        );
                    })),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("自动更新"),
            )
            .child(
                Button::new(("profile-update-interval", index))
                    .label(update_cron.as_deref().map_or_else(
                        || format_update_interval(interval_minutes),
                        |expression| format!("Cron {expression}"),
                    ))
                    .xsmall()
                    .outline()
                    .disabled(!auto_update || self.mutating)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_profile_update_policy(
                            interval_id.clone(),
                            true,
                            next_update_interval(interval_minutes),
                            cx,
                        );
                    })),
            )
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(theme.muted_foreground)
                    .child("点击间隔可在 1 / 6 / 12 / 24 小时之间切换"),
            )
    }
}

fn next_update_interval(current: u32) -> u32 {
    UPDATE_INTERVALS
        .iter()
        .copied()
        .find(|interval| *interval > current)
        .unwrap_or(UPDATE_INTERVALS[0])
}

fn format_update_interval(minutes: u32) -> String {
    if minutes % 1_440 == 0 {
        format!("每 {} 天", minutes / 1_440)
    } else if minutes % 60 == 0 {
        format!("每 {} 小时", minutes / 60)
    } else {
        format!("每 {minutes} 分钟")
    }
}

fn profile_source(source: &ProfileSource) -> String {
    match source {
        ProfileSource::Local { original_path } => compact_text(original_path, 76),
        ProfileSource::Remote {
            url,
            user_agent,
            options,
        } => {
            let route = match options.route() {
                RemoteProfileRoute::Direct => "仅直连下载",
                RemoteProfileRoute::DirectWithMihomoFallback => "直连 · 失败经内核代理重试",
                RemoteProfileRoute::Mihomo => "内核代理下载",
            };
            let authorization = if options.authorization.is_some() {
                " · Authorization 已配置"
            } else {
                ""
            };
            format!(
                "{} · UA {user_agent} · {route}{authorization}",
                compact_text(url, 62)
            )
        }
    }
}

fn render_subscription_usage(
    usage: &SubscriptionUsage,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    let quota = if usage.total == 0 {
        format!("已用 {} · 总量未提供", format_bytes(usage.used()))
    } else {
        let percent = u128::from(usage.used().min(usage.total)) * 100 / u128::from(usage.total);
        format!(
            "已用 {} / {} · {percent}%",
            format_bytes(usage.used()),
            format_bytes(usage.total)
        )
    };
    h_flex()
        .gap_3()
        .text_size(px(10.))
        .text_color(theme.muted_foreground)
        .child(quota)
        .child(div().child(format_subscription_expiry(usage.expire)))
        .into_any_element()
}

fn format_subscription_expiry(expire: u64) -> String {
    if expire == 0 {
        return "到期时间未提供".into();
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    if expire <= now {
        "订阅已到期".into()
    } else {
        let days = expire.saturating_sub(now).saturating_add(86_399) / 86_400;
        format!("剩余 {days} 天 · Unix {expire}")
    }
}

fn profile_heading(
    profile: &ProfileRecord,
    active: bool,
    theme: &gpui_component::Theme,
) -> gpui::Div {
    h_flex()
        .gap_2()
        .child(
            div()
                .size_2()
                .rounded_full()
                .bg(if active { theme.success } else { theme.primary }),
        )
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(profile.name.clone()),
        )
        .child(
            div()
                .px_2()
                .py_0p5()
                .rounded_full()
                .bg(if active {
                    theme.success.opacity(0.14)
                } else {
                    theme.muted.opacity(0.5)
                })
                .text_size(px(10.))
                .text_color(if active {
                    theme.success
                } else {
                    theme.muted_foreground
                })
                .child(if active {
                    "当前使用"
                } else {
                    profile.source_label()
                }),
        )
        .child(div().flex_1())
        .child(
            div()
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format_bytes(profile.size_bytes)),
        )
}

#[cfg(test)]
mod tests {
    use super::{format_subscription_expiry, format_update_interval, next_update_interval};

    #[test]
    fn update_interval_cycle_wraps_after_one_day() {
        assert_eq!(next_update_interval(60), 360);
        assert_eq!(next_update_interval(1_440), 60);
    }

    #[test]
    fn update_interval_label_preserves_minutes_and_hours() {
        assert_eq!(format_update_interval(15), "每 15 分钟");
        assert_eq!(format_update_interval(360), "每 6 小时");
        assert_eq!(format_update_interval(1_440), "每 1 天");
    }

    #[test]
    fn subscription_expiry_distinguishes_missing_and_expired_values() {
        assert_eq!(format_subscription_expiry(0), "到期时间未提供");
        assert_eq!(format_subscription_expiry(1), "订阅已到期");
    }
}
