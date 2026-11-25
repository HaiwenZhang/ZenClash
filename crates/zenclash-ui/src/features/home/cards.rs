use super::status::stream_status_text;
use super::*;

const HOME_PROFILE_PICKER_LIMIT: usize = 20;

impl RuntimePage {
    pub(super) fn render_home_profile(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let active = self.profiles.active_profile();
        let name = active.map_or_else(
            || zenclash_i18n::text("home.profile.none"),
            |profile| profile.name.clone(),
        );
        let source = active.map_or_else(
            || zenclash_i18n::text("home.profile.none_description"),
            |profile| {
                zenclash_i18n::text_with(
                    "profiles.catalog.updated",
                    &[("age", format_profile_age(profile.updated_at))],
                )
            },
        );
        let quota = active.and_then(|profile| profile.subscription.usage.as_ref());
        let expiry = active
            .and_then(|profile| profile.subscription.usage.as_ref())
            .map_or_else(
                || zenclash_i18n::text("home.profile.expiry_unavailable"),
                subscription_expiry,
            );
        let active_id = self.profiles.catalog().active.clone();
        let mut profiles = self
            .profiles
            .catalog()
            .profiles
            .iter()
            .take(HOME_PROFILE_PICKER_LIMIT)
            .map(|profile| (profile.id.clone(), profile.name.clone()))
            .collect::<Vec<_>>();
        if let Some(active) = active
            && !profiles.iter().any(|(id, _)| id == &active.id)
        {
            profiles.push((active.id.clone(), active.name.clone()));
        }
        let can_switch = self.profiles.store().is_some() && !profiles.is_empty();
        let runtime_page = cx.entity().downgrade();
        let profile_switch_tooltip =
            zenclash_i18n::text_with("home.profile.switch_current", &[("name", name.clone())]);
        let profile_picker = Button::new("home-profile-picker")
            .label(name)
            .icon(Icon::new(IconName::File).size_6())
            .justify_start()
            .min_w_0()
            .overflow_hidden()
            .ghost()
            .small()
            .h_8()
            .text_lg()
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .dropdown_caret(true)
            .tooltip(profile_switch_tooltip)
            .loading(self.home.profile_switching.is_some())
            .disabled(self.core_busy() || !can_switch)
            .dropdown_menu(move |menu, window, cx| {
                let mut menu = HomeMenuSize::PROFILE_PICKER
                    .apply_to(menu, window, cx)
                    .scrollable(true);
                for (id, name) in &profiles {
                    let is_active = active_id.as_deref() == Some(id.as_str());
                    let runtime_page = runtime_page.clone();
                    let id = id.clone();
                    menu = menu.item(
                        PopupMenuItem::new(name.clone())
                            .checked(is_active)
                            .disabled(is_active)
                            .on_click(move |_, _, cx| {
                                let id = id.clone();
                                let _ = runtime_page.update(cx, |page, cx| {
                                    page.activate_home_profile(id, cx);
                                });
                            }),
                    );
                }
                menu.item(
                    PopupMenuItem::new(zenclash_i18n::text("home.profile.all")).on_click(
                        |_, window, cx| window.dispatch_action(Box::new(NavigateProfiles), cx),
                    ),
                )
            });

        v_flex()
            .flex_1()
            .line_height(gpui_kit::relative(1.25))
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .min_w_0()
            .child(
                h_flex().px_4().pt_4().justify_between().gap_2().child(
                    div()
                        .text_lg()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(zenclash_i18n::text("home.profile.title")),
                ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .px_4()
                    .py_2()
                    .gap_2()
                    .child(h_flex().gap_2().child(profile_picker).when_some(
                        active,
                        |row, profile| {
                            row.child(
                                div()
                                    .text_xs()
                                    .px_2()
                                    .py_1()
                                    .rounded(theme.radius)
                                    .bg(crate::foundation::design::brand_accent(theme)
                                        .opacity(0.12))
                                    .text_color(theme.primary)
                                    .child(profile.source_label()),
                            )
                        },
                    ))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(source),
                    )
                    .child(
                        h_flex()
                            .gap_4()
                            .items_center()
                            .flex_wrap()
                            .child(
                                WavePercentage::new(
                                    "home-subscription-quota",
                                    zenclash_i18n::text("profiles.design.used"),
                                )
                                .diameter(9.)
                                .animated(self.live_updates_enabled())
                                .value(
                                    quota.filter(|value| value.total > 0).map(|value| {
                                        100. * normalized_fraction(value.used(), value.total)
                                    }),
                                ),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .pl_4()
                                    .border_l_1()
                                    .border_color(theme.border)
                                    .gap_2()
                                    .children(
                                        [
                                            (
                                                "profiles.design.used",
                                                quota.map(SubscriptionUsage::used),
                                            ),
                                            (
                                                "profiles.design.remaining",
                                                quota.filter(|value| value.total > 0).map(
                                                    |value| {
                                                        value.total.saturating_sub(value.used())
                                                    },
                                                ),
                                            ),
                                        ]
                                        .into_iter()
                                        .map(
                                            |(key, bytes)| {
                                                v_flex()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .text_color(theme.muted_foreground)
                                                            .child(zenclash_i18n::text(key)),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(gpui_kit::rems(1.625))
                                                            .font_bold()
                                                            .child(bytes.map_or_else(
                                                                || "—".to_owned(),
                                                                format_bytes,
                                                            )),
                                                    )
                                            },
                                        ),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .text_sm()
                                            .text_color(theme.muted_foreground)
                                            .child(zenclash_i18n::text("profiles.design.total"))
                                            .child(
                                                quota.filter(|value| value.total > 0).map_or_else(
                                                    || "—".into(),
                                                    |value| format_bytes(value.total),
                                                ),
                                            ),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .gap_2()
                            .pt_2()
                            .border_t_1()
                            .border_color(theme.border)
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(expiry)
                            .when_some(quota.and_then(subscription_expiry_date), |row, date| {
                                row.child(date)
                            }),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_home_traffic(
        &self,
        streams: &StreamStatuses,
        theme: &gpui_kit::component::Theme,
        _cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let traffic = self.home_traffic_snapshot();
        let unknown_stream = Observation::<StreamStatus>::Loading;
        let traffic_stream = if streams
            .traffic
            .value()
            .is_some_and(|status| status.generation != self.home.generation)
        {
            &unknown_stream
        } else {
            &streams.traffic
        };
        let (points, observed_seconds) = self.home.chart.points(self.home.generation);
        let ceiling = points
            .first()
            .map_or(MIN_TRAFFIC_CHART_CEILING as f64, |point| point.ceiling);
        let (axis_unit, axis_divisor) = if ceiling >= 1_048_576. {
            ("MiB/s", 1_048_576.)
        } else if ceiling >= 1_024. {
            ("KiB/s", 1_024.)
        } else {
            ("B/s", 1.)
        };
        let chart = AreaChart::new(points)
            .x(|point| point.label.clone())
            .y(|point| point.download)
            .name(zenclash_i18n::text("home.traffic.download"))
            .stroke(theme.chart_1)
            .fill(theme.chart_1.opacity(0.18))
            .natural()
            .y(|point| point.upload)
            .name(zenclash_i18n::text("home.traffic.upload"))
            .stroke(theme.chart_2)
            .fill(theme.chart_2.opacity(0.14))
            .natural()
            .y_domain(0., ceiling)
            .x_axis(false)
            .y_axis(false)
            .grid(false)
            .y_padding(0., 0.)
            .tooltip_value(|_, _, value| format_speed(value.max(0.) as u64).into());
        // A fixed two-minute frame is separate from the observed series. Unknown
        // history stays blank; stretching a few seconds across the frame would
        // misrepresent both the time axis and the rates.
        let plot = h_flex()
            .items_stretch()
            .h(rems(10.))
            .gap_2()
            .child(
                v_flex()
                    .w(rems(2.))
                    .justify_between()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .children((0..=3).rev().map(|tick| {
                        div().text_right().child(format!(
                            "{:.1}",
                            ceiling / axis_divisor * f64::from(tick) / 3.
                        ))
                    })),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        AreaChart::new(vec![(0., 0.), (1., 0.)])
                            .id("home-traffic-grid")
                            .x(|point| point.0.to_string())
                            .y(|point| point.1)
                            .y_domain(0., ceiling)
                            .y_padding(0., 0.)
                            .stroke(theme.transparent)
                            .fill(theme.transparent)
                            .x_axis(false)
                            .y_axis(false)
                            .y_tick_count(4)
                            .grid_columns(5)
                            .grid_dashed(false)
                            .interactive(false),
                    )
                    .child(
                        div()
                            .absolute()
                            .right_0()
                            .top_0()
                            .h_full()
                            .w(gpui_kit::relative(traffic::observed_span_fraction(
                                observed_seconds,
                            )))
                            .child(chart),
                    ),
            );
        let time_axis = h_flex()
            .pl(gpui_kit::rems(2.5))
            .justify_between()
            .text_xs()
            .text_color(theme.muted_foreground)
            .children(["−2m", "−1m 30s", "−1m", "−30s"].map(|label| div().child(label)))
            .child(zenclash_i18n::text("home.traffic.now"));

        v_flex()
            .flex_1()
            .min_w_0()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .gap_4()
                    .items_center()
                    .child(
                        div()
                            .text_lg()
                            .font_bold()
                            .child(zenclash_i18n::text("home.traffic.title")),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("home.traffic.range_120")),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .gap_3()
                    .text_xs()
                    .child(div().text_color(theme.muted_foreground).child(axis_unit))
                    .child(
                        h_flex()
                            .gap_4()
                            .child(series_label(
                                "home-upload-legend",
                                IconName::ArrowUp,
                                zenclash_i18n::text("home.traffic.upload"),
                                format_speed(traffic.upload),
                                theme.chart_2,
                                theme,
                            ))
                            .child(series_label(
                                "home-download-legend",
                                IconName::ArrowDown,
                                zenclash_i18n::text("home.traffic.download"),
                                format_speed(traffic.download),
                                theme.chart_1,
                                theme,
                            )),
                    ),
            )
            .when(!traffic_stream.is_fresh(), |view| {
                let (status, color) = stream_status_text(traffic_stream, theme);
                view.child(status_label(status, color, theme))
            })
            .child(v_flex().gap_2().w_full().child(plot).child(time_axis))
            .child(div().text_xs().text_color(theme.muted_foreground).child(
                zenclash_i18n::text_with(
                    "home.traffic.observed_duration",
                    &[("seconds", observed_seconds.to_string())],
                ),
            ))
            .into_any_element()
    }
}

fn subscription_expiry_date(usage: &SubscriptionUsage) -> Option<String> {
    let timestamp = i64::try_from(usage.expire)
        .ok()
        .filter(|value| *value > 0)?;
    let date = chrono::DateTime::from_timestamp(timestamp, 0)?;
    Some(zenclash_i18n::text_with(
        "home.profile.expiry_date",
        &[("date", date.format("%Y-%m-%d").to_string())],
    ))
}

fn subscription_expiry(usage: &SubscriptionUsage) -> String {
    remaining_days(usage.expire).map_or_else(
        || zenclash_i18n::text("home.profile.expiry_unavailable"),
        |days| {
            if days == 0 {
                zenclash_i18n::text("home.profile.expired")
            } else {
                zenclash_i18n::text_with(
                    "home.profile.remaining_days",
                    &[("days", days.to_string())],
                )
            }
        },
    )
}

fn remaining_days(expire: u64) -> Option<u64> {
    if expire == 0 {
        return None;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if expire <= now {
        Some(0)
    } else {
        Some(expire.saturating_sub(now).saturating_add(86_399) / 86_400)
    }
}

fn series_label(
    id: &'static str,
    icon: IconName,
    label: impl Into<SharedString>,
    value: String,
    color: gpui_kit::Hsla,
    _theme: &gpui_kit::component::Theme,
) -> gpui_kit::AnyElement {
    h_flex()
        .id(id)
        .gap_1()
        .text_sm()
        .text_color(color)
        .child(Icon::new(icon).size_4())
        .child(label.into())
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(value.clone()).build(window, cx)
        })
        .into_any_element()
}
