use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::component::{Selectable, chart::PieChart, progress::Progress};
use zenclash_core::{ProfileRecord, ProfileSource, SubscriptionUsage};

use super::super::super::{
    Button, ButtonVariants, Context, Disableable, FluentBuilder, IconName, IntoElement,
    ParentElement, RemoteProfileRoute, RuntimePage, Sizable, Styled, Switch, compact_text, div,
    empty_state, format_bytes, format_profile_age, h_flex, setting_card, v_flex,
};

const UPDATE_INTERVALS: [u32; 4] = [60, 6 * 60, 12 * 60, 24 * 60];

impl RuntimePage {
    pub(super) fn render_managed_profiles(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let mut card = setting_card(zenclash_i18n::text("profiles.catalog.title"), theme);
        if !self
            .profiles
            .forms
            .catalog_view
            .is_current(&self.profiles.catalog)
        {
            return card.child(empty_state(
                zenclash_i18n::text("common.actions.loading"),
                theme,
            ));
        }
        if self.profiles.catalog.profiles.is_empty() {
            return card.child(empty_state(
                zenclash_i18n::text("profiles.catalog.empty"),
                theme,
            ));
        }

        use super::super::state::ProfileFilter;
        card = card.child(
            h_flex().px_3().py_2().justify_end().gap_2().children(
                [
                    (ProfileFilter::All, "profiles.design.filter_all"),
                    (ProfileFilter::Remote, "profiles.source.remote"),
                    (ProfileFilter::Local, "profiles.source.local"),
                ]
                .into_iter()
                .map(|(filter, key)| {
                    Button::new(format!("profile-filter:{filter:?}"))
                        .label(zenclash_i18n::text(key))
                        .small()
                        .outline()
                        .selected(self.profiles.forms.catalog_view.filter == filter)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.profiles
                                .forms
                                .catalog_view
                                .set_filter(filter, &this.profiles.catalog);
                            cx.notify();
                        }))
                }),
            ),
        );
        let grid = h_flex().gap_3().flex_wrap().p_3().children(
            self.profiles
                .forms
                .catalog_view
                .visible_indices()
                .iter()
                .filter_map(|&index| {
                    self.profiles
                        .catalog
                        .profiles
                        .get(index)
                        .map(|profile| self.render_managed_profile(profile, theme, cx))
                }),
        );
        card = card.child(grid).when(
            self.profiles
                .forms
                .catalog_view
                .visible_indices()
                .is_empty(),
            |this| {
                this.child(empty_state(
                    zenclash_i18n::text("profiles.design.filter_empty"),
                    theme,
                ))
            },
        );
        let page = self.profiles.forms.catalog_view.page;
        let pages = self.profiles.forms.catalog_view.page_count();
        card = card.when(pages > 1, |this| {
            this.child(
                h_flex()
                    .p_3()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("previous-profile-page")
                            .small()
                            .outline()
                            .label(zenclash_i18n::text("common.actions.previous_page"))
                            .disabled(page == 0)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.profiles
                                    .forms
                                    .catalog_view
                                    .set_page(page.saturating_sub(1));
                                cx.notify();
                            })),
                    )
                    .child(div().text_sm().child(format!("{} / {pages}", page + 1)))
                    .child(
                        Button::new("next-profile-page")
                            .small()
                            .outline()
                            .label(zenclash_i18n::text("common.actions.next_page"))
                            .disabled(page + 1 >= pages)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.profiles.forms.catalog_view.set_page(page + 1);
                                cx.notify();
                            })),
                    ),
            )
        });

        card
    }

    pub(super) fn render_profile_inspector(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        if !self
            .profiles
            .forms
            .catalog_view
            .is_current(&self.profiles.catalog)
        {
            return setting_card(zenclash_i18n::text("profiles.catalog.title"), theme).child(
                empty_state(zenclash_i18n::text("common.actions.loading"), theme),
            );
        }
        let profile = self
            .profiles
            .forms
            .catalog_view
            .selected_index()
            .and_then(|index| self.profiles.catalog.profiles.get(index));
        let Some(profile) = profile else {
            return setting_card(zenclash_i18n::text("profiles.catalog.title"), theme).child(
                empty_state(zenclash_i18n::text("profiles.catalog.empty"), theme),
            );
        };
        let active = self.profiles.catalog.active.as_deref() == Some(profile.id.as_str());
        setting_card(profile.name.clone(), theme).child(
            v_flex()
                .p_4()
                .gap_4()
                .child(profile_heading(profile, active, theme))
                .when_some(profile.subscription.usage.as_ref(), |this, usage| {
                    this.child(subscription_quota_chart(profile, usage, theme))
                })
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(profile.source_label()),
                )
                .child(div().text_xs().text_color(theme.muted_foreground).child(
                    zenclash_i18n::text_with(
                        "profiles.catalog.updated",
                        &[("age", format_profile_age(profile.updated_at))],
                    ),
                ))
                .when(profile.is_remote(), |this| {
                    this.child(self.render_profile_update_policy(profile, theme, cx))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(profile_source(&profile.source)),
                        )
                })
                .child(self.render_profile_actions(profile, active, "inspector-", cx)),
        )
    }

    pub(super) fn render_profile_recovery(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let mut card = setting_card(zenclash_i18n::text("profiles.recovery.title"), theme).child(
            div().px_4().py_3().text_sm().child(zenclash_i18n::text(
                if self.profiles.pending_finalization.is_some() {
                    "profiles.recovery.pending_description"
                } else {
                    "profiles.recovery.description"
                },
            )),
        );
        if let Some(version) = self.profiles.pending_finalization {
            return card.child(
                h_flex().px_4().pb_3().child(
                    Button::new("confirm-service-profile")
                        .label(zenclash_i18n::text("profiles.recovery.confirm"))
                        .outline()
                        .disabled(self.core_busy())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.confirm_service_profile(version, cx)
                        })),
                ),
            );
        }
        if let Some(recovery) = &self.profiles.recovery {
            let known = recovery.last_known_good.as_ref().and_then(|version| {
                self.profiles
                    .catalog
                    .profiles
                    .iter()
                    .find(|profile| profile.id == version.profile_id)
            });
            let target = known.or_else(|| {
                self.profiles
                    .catalog
                    .profiles
                    .iter()
                    .find(|profile| profile.id == recovery.attempted.profile_id)
            });
            if let Some(profile) = target {
                let id = profile.id.clone();
                card = card.child(
                    h_flex().px_4().pb_3().child(
                        Button::new("reapply-profile-recovery")
                            .label(zenclash_i18n::text_with(
                                "profiles.recovery.reapply",
                                &[("name", profile.name.clone())],
                            ))
                            .outline()
                            .disabled(self.core_busy())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.activate_managed_profile(id.clone(), cx)
                            })),
                    ),
                );
            }
        }
        card
    }

    fn render_managed_profile(
        &self,
        profile: &ProfileRecord,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let active = self.profiles.catalog.active.as_deref() == Some(profile.id.as_str());
        let selected = self.profiles.forms.catalog_view.is_selected(&profile.id);
        let show_id = profile.id.clone();
        let source = profile_source(&profile.source);

        v_flex()
            .flex_grow_1()
            .min_w_0()
            .w(gpui_kit::rems(20.))
            .max_w_full()
            .p_4()
            .gap_3()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(if selected {
                theme.primary
            } else {
                theme.border
            })
            .bg(if selected {
                theme.primary.opacity(0.06)
            } else {
                theme.background
            })
            .child(profile_heading(profile, active, theme))
            .child(
                Button::new(format!("inspect-profile:{}", profile.id))
                    .label(zenclash_i18n::text("profiles.design.details"))
                    .small()
                    .ghost()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.profiles.forms.catalog_view.select(&show_id);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(source),
            )
            .when_some(profile.subscription.usage.as_ref(), |this, usage| {
                this.child(render_subscription_usage(
                    usage,
                    format!("catalog-quota:{}", profile.id),
                    theme,
                ))
            })
            .when_some(
                profile.subscription.home_url.as_deref(),
                |this, home_url| {
                    this.child(div().text_xs().text_color(theme.muted_foreground).child(
                        zenclash_i18n::text_with(
                            "profiles.catalog.homepage",
                            &[("url", compact_text(home_url, 90))],
                        ),
                    ))
                },
            )
            .child(
                h_flex()
                    .justify_between()
                    .flex_wrap()
                    .gap_2()
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        zenclash_i18n::text_with(
                            "profiles.catalog.updated",
                            &[("age", format_profile_age(profile.updated_at))],
                        ),
                    ))
                    .child(self.render_profile_actions(profile, active, "", cx)),
            )
    }

    fn render_profile_actions(
        &self,
        profile: &ProfileRecord,
        active: bool,
        scope: &str,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let activate_id = profile.id.clone();
        let update_id = profile.id.clone();
        let edit_id = profile.id.clone();
        let delete_id = profile.id.clone();
        h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .flex_wrap()
            .when(!profile.is_remote(), |this| {
                this.child(super::forms::configuration_edit_button(
                    format!("{scope}edit-profile-config:{}", profile.id),
                    active,
                ))
            })
            .when(profile.is_remote(), |this| {
                this.child(
                    Button::new(format!("{scope}edit-profile-request:{}", profile.id))
                        .icon(IconName::Settings2)
                        .label(zenclash_i18n::text("profiles.actions.request_settings"))
                        .small()
                        .ghost()
                        .disabled(self.core_busy())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.profiles.forms.catalog_view.select(&edit_id);
                            this.begin_edit_remote_profile(edit_id.clone(), window, cx);
                        })),
                )
                .child(
                    Button::new(format!("{scope}update-profile:{}", profile.id))
                        .icon(crate::assets::AppIcon::RefreshCw)
                        .label(zenclash_i18n::text("profiles.actions.update"))
                        .small()
                        .outline()
                        .disabled(self.core_busy())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.update_managed_profile(update_id.clone(), cx);
                        })),
                )
            })
            .child(
                Button::new(format!("{scope}activate-profile:{}", profile.id))
                    .icon(IconName::ArrowRight)
                    .label(if active {
                        zenclash_i18n::text("profiles.actions.active")
                    } else {
                        zenclash_i18n::text("profiles.actions.activate")
                    })
                    .small()
                    .primary()
                    .disabled(active || self.core_busy())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.activate_managed_profile(activate_id.clone(), cx);
                    })),
            )
            .child(
                Button::new(format!("{scope}delete-profile:{}", profile.id))
                    .accessibility_label(zenclash_i18n::text_with(
                        "profiles.actions.delete",
                        &[("name", profile.name.clone())],
                    ))
                    .icon(IconName::Delete)
                    .small()
                    .ghost()
                    .danger()
                    .disabled(active || self.core_busy())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.delete_managed_profile(delete_id.clone(), cx);
                    })),
            )
    }

    fn render_profile_update_policy(
        &self,
        profile: &ProfileRecord,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let interval_minutes = profile.update_interval_minutes;
        let update_cron = profile.update_cron.clone();
        let auto_update = profile.auto_update;
        let policy_id = profile.id.clone();
        let interval_id = profile.id.clone();
        h_flex()
            .gap_3()
            .flex_wrap()
            .child(
                Switch::new(format!("auto-update-profile:{}", profile.id))
                    .accessibility_label(zenclash_i18n::text("profiles.catalog.auto_update"))
                    .checked(auto_update)
                    .disabled(self.core_busy())
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
                    .child(zenclash_i18n::text("profiles.catalog.auto_update")),
            )
            .child(
                Button::new(format!("profile-update-interval:{}", profile.id))
                    .label(update_cron.as_deref().map_or_else(
                        || format_update_interval(interval_minutes),
                        |expression| format!("Cron {expression}"),
                    ))
                    .xsmall()
                    .outline()
                    .disabled(!auto_update || self.core_busy())
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
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(zenclash_i18n::text("profiles.catalog.interval_hint")),
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
    if minutes.is_multiple_of(1_440) {
        zenclash_i18n::text_with(
            "profiles.catalog.every_days",
            &[("count", (minutes / 1_440).to_string())],
        )
    } else if minutes.is_multiple_of(60) {
        zenclash_i18n::text_with(
            "profiles.catalog.every_hours",
            &[("count", (minutes / 60).to_string())],
        )
    } else {
        zenclash_i18n::text_with(
            "profiles.catalog.every_minutes",
            &[("count", minutes.to_string())],
        )
    }
}

fn profile_source(source: &ProfileSource) -> String {
    match source {
        ProfileSource::Local { original_path } => compact_text(original_path, 76),
        ProfileSource::Remote {
            url: _,
            user_agent,
            options,
        } => {
            let route = match options.route() {
                RemoteProfileRoute::Direct => zenclash_i18n::text("profiles.route.direct"),
                RemoteProfileRoute::DirectWithMihomoFallback => {
                    zenclash_i18n::text("profiles.route.fallback")
                }
                RemoteProfileRoute::Mihomo => zenclash_i18n::text("profiles.route.proxy"),
            };
            let authorization = if options.authorization.is_some() {
                zenclash_i18n::text("profiles.route.authorization")
            } else {
                String::new()
            };
            format!("UA {user_agent} · {route}{authorization}")
        }
    }
}

fn render_subscription_usage(
    usage: &SubscriptionUsage,
    id: String,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::AnyElement {
    let quota = if usage.total == 0 {
        zenclash_i18n::text_with(
            "profiles.usage.no_total",
            &[("used", format_bytes(usage.used()))],
        )
    } else {
        let percent = u128::from(usage.used().min(usage.total)) * 100 / u128::from(usage.total);
        zenclash_i18n::text_with(
            "profiles.usage.quota",
            &[
                ("used", format_bytes(usage.used())),
                ("total", format_bytes(usage.total)),
                ("percent", percent.to_string()),
            ],
        )
    };
    let percent = quota_percent(usage);
    v_flex()
        .gap_2()
        .child(div().text_sm().child(quota))
        .when_some(percent, |this, percent| {
            this.child(Progress::new(id).h_2().color(theme.primary).value(percent))
        })
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format_subscription_expiry(usage.expire)),
        )
        .into_any_element()
}

fn quota_percent(usage: &SubscriptionUsage) -> Option<f32> {
    (usage.total > 0).then(|| {
        let used = u128::from(usage.used().min(usage.total));
        let tenths = used * 1_000 / u128::from(usage.total);
        f32::from(u16::try_from(tenths).unwrap_or(1_000)) / 10.
    })
}

fn subscription_quota_chart(
    profile: &ProfileRecord,
    usage: &SubscriptionUsage,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    let Some(percent) = quota_percent(usage) else {
        return v_flex().child(render_subscription_usage(
            usage,
            format!("detail-quota:{}", profile.id),
            theme,
        ));
    };
    let used_color = theme.chart_1;
    let remaining_color = theme.chart_1.opacity(0.18);
    let chart = PieChart::new([(percent, true), (100. - percent, false)])
        .id(format!("profile-quota:{}", profile.id))
        .inner_radius(f32::from(theme.font_size) * 3.8)
        .value(|point| point.0)
        .color(move |point| if point.1 { used_color } else { remaining_color });
    v_flex()
        .gap_3()
        .child(
            div().relative().h_48().w_full().child(chart).child(
                v_flex()
                    .absolute()
                    .inset_0()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(format!("{percent:.1}%")),
                    ),
            ),
        )
        .children(
            [
                ("profiles.design.used", usage.used()),
                (
                    "profiles.design.remaining",
                    usage.total.saturating_sub(usage.used()),
                ),
                ("profiles.design.total", usage.total),
            ]
            .into_iter()
            .map(|(key, bytes)| {
                h_flex()
                    .justify_between()
                    .gap_3()
                    .text_sm()
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text(key)),
                    )
                    .child(
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .child(format_bytes(bytes)),
                    )
            }),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format_subscription_expiry(usage.expire)),
        )
}

fn format_subscription_expiry(expire: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    format_subscription_expiry_at(expire, now)
}

fn format_subscription_expiry_at(expire: u64, now: u64) -> String {
    let date = i64::try_from(expire)
        .ok()
        .and_then(|timestamp| chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0));
    let Some(date) = date.filter(|_| expire != 0) else {
        return zenclash_i18n::text("profiles.usage.expiry_unavailable");
    };
    if expire <= now {
        zenclash_i18n::text("profiles.usage.expired")
    } else {
        let days = expire.saturating_sub(now).saturating_add(86_399) / 86_400;
        zenclash_i18n::text_with(
            "profiles.usage.remaining_date",
            &[
                ("days", days.to_string()),
                ("date", date.format("%Y-%m-%d").to_string()),
            ],
        )
    }
}

fn profile_heading(
    profile: &ProfileRecord,
    active: bool,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    h_flex()
        .gap_2()
        .flex_wrap()
        .child(
            div()
                .size_2()
                .rounded_full()
                .bg(if active { theme.success } else { theme.primary }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_ellipsis()
                .whitespace_nowrap()
                .overflow_hidden()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
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
                .text_xs()
                .text_color(if active {
                    theme.success
                } else {
                    theme.muted_foreground
                })
                .child(if active {
                    zenclash_i18n::text("profiles.catalog.current")
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
    use super::{
        format_subscription_expiry, format_subscription_expiry_at, format_update_interval,
        next_update_interval, quota_percent,
    };
    use zenclash_core::SubscriptionUsage;

    #[test]
    fn quota_visualization_preserves_provider_accounting_and_unknown_totals() {
        let usage = SubscriptionUsage {
            upload: 84,
            download: 600,
            total: 2_000,
            expire: 0,
        };
        assert_eq!(quota_percent(&usage), Some(34.2));
        assert_eq!(
            quota_percent(&SubscriptionUsage {
                total: 0,
                ..usage.clone()
            }),
            None
        );
        assert_eq!(
            quota_percent(&SubscriptionUsage {
                upload: u64::MAX,
                download: u64::MAX,
                total: 10,
                expire: 0
            }),
            Some(100.)
        );
    }

    #[test]
    fn update_interval_cycle_wraps_after_one_day() {
        assert_eq!(next_update_interval(60), 360);
        assert_eq!(next_update_interval(1_440), 60);
    }

    #[test]
    fn update_interval_label_preserves_minutes_and_hours() {
        assert!(format_update_interval(15).contains("15"));
        assert!(format_update_interval(360).contains('6'));
        assert!(format_update_interval(1_440).contains('1'));
    }

    #[test]
    fn subscription_expiry_uses_calendar_dates_and_rounds_partial_days() {
        let expire = 1_735_689_600;
        assert_eq!(
            format_subscription_expiry_at(expire, expire - 86_401),
            zenclash_i18n::text_with(
                "profiles.usage.remaining_date",
                &[("days", "2".into()), ("date", "2025-01-01".into())],
            ),
        );
        assert_eq!(
            format_subscription_expiry_at(u64::MAX, 0),
            zenclash_i18n::text("profiles.usage.expiry_unavailable"),
        );
        assert_eq!(
            format_subscription_expiry_at(expire, expire),
            zenclash_i18n::text("profiles.usage.expired"),
        );
    }

    #[test]
    fn subscription_expiry_distinguishes_missing_and_expired_values() {
        let missing = format_subscription_expiry(0);
        let expired = format_subscription_expiry(1);
        assert!(!missing.is_empty());
        assert!(!expired.is_empty());
        assert_ne!(missing, expired);
    }
}
