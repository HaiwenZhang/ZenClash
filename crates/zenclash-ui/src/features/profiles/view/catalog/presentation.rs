use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::component::{h_flex, progress::Progress, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{IntoElement, ParentElement, Styled, div};
use zenclash_core::{ProfileRecord, SubscriptionUsage};

use crate::components::page_primitives::format_bytes;

pub(super) fn format_update_interval(minutes: u32) -> String {
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

pub(super) fn render_subscription_usage(
    usage: &SubscriptionUsage,
    id: String,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::AnyElement {
    let quota = quota_percent(usage).map_or_else(
        || {
            zenclash_i18n::text_with(
                "profiles.usage.no_total",
                &[("used", format_bytes(usage.used()))],
            )
        },
        |percent| {
            zenclash_i18n::text_with(
                "profiles.design.usage_percent",
                &[("percent", format!("{percent:.1}"))],
            )
        },
    );
    let percent = quota_percent(usage);
    v_flex()
        .gap_2()
        .child(div().text_sm().child(quota))
        .when_some(percent, |this, percent| {
            this.child(
                h_flex()
                    .gap_3()
                    .child(
                        div().flex_1().child(
                            Progress::new(id)
                                .h_2()
                                .color(crate::foundation::design::brand_accent(theme))
                                .value(percent),
                        ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{percent:.1}%")),
                    ),
            )
        })
        .into_any_element()
}

fn quota_percent(usage: &SubscriptionUsage) -> Option<f32> {
    (usage.total > 0).then(|| {
        let used = u128::from(usage.used().min(usage.total));
        let tenths = used * 1_000 / u128::from(usage.total);
        f32::from(u16::try_from(tenths).unwrap_or(1_000)) / 10.
    })
}

pub(super) fn subscription_quota_chart(
    _profile: &ProfileRecord,
    usage: &SubscriptionUsage,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    v_flex()
        .gap_2()
        .child(profile_detail_row(
            "profiles.design.used",
            quota_percent(usage).map_or_else(|| "—".into(), |value| format!("{value:.1}%")),
            theme,
        ))
        .child(profile_detail_row(
            "profiles.design.total",
            format!(
                "{} / {}",
                format_bytes(usage.used()),
                format_bytes(usage.total)
            ),
            theme,
        ))
}

pub(super) fn profile_detail_row(
    key: &str,
    value: String,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    h_flex()
        .gap_3()
        .text_sm()
        .child(
            div()
                .w_24()
                .flex_shrink_0()
                .text_color(theme.muted_foreground)
                .child(zenclash_i18n::text(key)),
        )
        .child(div().flex_1().min_w_0().child(value))
}

pub(super) fn format_subscription_remaining(expire: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let valid = i64::try_from(expire)
        .ok()
        .and_then(|timestamp| chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0))
        .is_some();
    if expire > now && valid {
        let days = expire.saturating_sub(now).saturating_add(86_399) / 86_400;
        zenclash_i18n::text_with(
            "profiles.design.remaining_days",
            &[("days", days.to_string())],
        )
    } else {
        format_subscription_expiry_at(expire, now)
    }
}

pub(super) fn format_subscription_expiry(expire: u64) -> String {
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

pub(super) fn profile_heading(
    profile: &ProfileRecord,
    active: bool,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    h_flex().gap_2().flex_wrap().child(
        div()
            .px_2()
            .py_0p5()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(if active || profile.is_remote() {
                theme.list_active
            } else {
                theme.chart_1.opacity(0.08)
            })
            .text_sm()
            .font_weight(gpui_kit::FontWeight::NORMAL)
            .text_color(if active || profile.is_remote() {
                theme.primary
            } else {
                theme.muted_foreground
            })
            .child(if active {
                zenclash_i18n::text("profiles.catalog.current")
            } else {
                profile.source_label()
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        format_subscription_expiry, format_subscription_expiry_at, format_update_interval,
        quota_percent,
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
            quota_percent(&SubscriptionUsage { total: 0, ..usage }),
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
