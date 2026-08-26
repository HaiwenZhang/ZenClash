use std::{
    collections::VecDeque,
    time::{SystemTime, UNIX_EPOCH},
};

use gpui::{div, rems, IntoElement, ParentElement, SharedString, Styled};
use gpui_component::{
    button::{Button, ButtonVariants},
    chart::AreaChart,
    h_flex,
    progress::Progress,
    switch::Switch,
    v_flex, Disableable, Icon, IconName, Selectable, Sizable,
};
use zenclash_core::{
    format_speed, ProxyCatalog, RuntimeConfig, SubscriptionUsage, SystemProxyStatus,
};

use crate::{
    app::{
        NavigateProfiles, NavigateProxies, NavigateSystemProxy, NavigateTraffic, SetDirectMode,
        SetGlobalMode, SetRuleMode,
    },
    components::sidebar::OutboundMode,
};

use super::{
    format_bytes, format_profile_age, normalized_fraction, Context, FluentBuilder,
    LiveTrafficSample, RuntimeData, RuntimePage,
};

impl RuntimePage {
    pub(in crate::pages::runtime) fn render_home(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let fallback_config = RuntimeConfig::default();
        let fallback_proxies = ProxyCatalog::default();
        let fallback_connections = zenclash_core::ConnectionsSnapshot::default();
        let fallback_system_proxy = SystemProxyStatus::default();
        let (config, proxies, connections, system_proxy) = match &self.data {
            RuntimeData::Dashboard {
                config,
                proxies,
                connections,
                system_proxy,
            } => (config, proxies, connections, system_proxy),
            _ => (
                &fallback_config,
                &fallback_proxies,
                &fallback_connections,
                &fallback_system_proxy,
            ),
        };

        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .items_start()
                    .gap_4()
                    .flex_wrap()
                    .child(self.render_home_profile(theme))
                    .child(self.render_home_proxy(config, proxies, theme)),
            )
            .child(self.render_home_controls(config, system_proxy, theme, cx))
            .child(self.render_home_traffic(connections, theme))
            .into_any_element()
    }

    fn render_home_profile(&self, theme: &gpui_component::Theme) -> gpui::AnyElement {
        let active = self.profile_catalog.active_profile();
        let name = active.map_or("尚未选择订阅", |profile| profile.name.as_str());
        let source = active.map_or_else(
            || "添加订阅后即可在这里查看用量和更新时间".to_owned(),
            |profile| {
                format!(
                    "{} · 更新于 {}",
                    profile.source_label(),
                    format_profile_age(profile.updated_at)
                )
            },
        );
        let (usage, usage_percent) = active
            .and_then(|profile| profile.subscription.usage.as_ref())
            .map_or_else(|| ("未提供流量额度".to_owned(), 0.), subscription_usage);

        home_card("当前订阅", IconName::FolderOpen, theme)
            .min_h(rems(11.5))
            .child(
                v_flex()
                    .flex_1()
                    .p_4()
                    .gap_3()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(name.to_owned()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(source),
                    )
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .gap_3()
                                    .justify_between()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("订阅用量")
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_right()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .overflow_hidden()
                                            .child(usage),
                                    ),
                            )
                            .child(Progress::new().h_1().bg(theme.primary).value(usage_percent)),
                    )
                    .child(div().flex_1())
                    .child(
                        h_flex().justify_end().child(
                            Button::new("home-open-profiles")
                                .icon(IconName::ArrowRight)
                                .label("订阅详情")
                                .small()
                                .outline()
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(NavigateProfiles), cx);
                                }),
                        ),
                    ),
            )
            .into_any_element()
    }

    fn render_home_proxy(
        &self,
        config: &RuntimeConfig,
        proxies: &ProxyCatalog,
        theme: &gpui_component::Theme,
    ) -> gpui::AnyElement {
        let selection = current_proxy_summary(config, proxies);
        let latency_color = match selection.delay {
            Some(1..=499) => theme.success,
            Some(500..) => theme.warning,
            Some(0) => theme.danger,
            None => theme.muted_foreground,
        };
        let latency = selection
            .delay
            .map_or_else(|| "未测速".to_owned(), |delay| format!("{delay} ms"));

        home_card("当前节点", IconName::GalleryVerticalEnd, theme)
            .min_h(rems(11.5))
            .child(
                v_flex()
                    .flex_1()
                    .p_4()
                    .gap_3()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(selection.group),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .justify_between()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_lg()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .child(selection.node),
                            )
                            .child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .rounded_full()
                                    .bg(latency_color.opacity(0.12))
                                    .font_family(theme.mono_font_family.clone())
                                    .text_xs()
                                    .text_color(latency_color)
                                    .child(latency),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(selection.kind),
                    )
                    .child(div().flex_1())
                    .child(
                        h_flex().justify_end().child(
                            Button::new("home-open-proxies")
                                .icon(IconName::ArrowRight)
                                .label("代理详情")
                                .small()
                                .outline()
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(NavigateProxies), cx);
                                }),
                        ),
                    ),
            )
            .into_any_element()
    }

    fn render_home_controls(
        &self,
        config: &RuntimeConfig,
        status: &SystemProxyStatus,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let active = status.active();
        let mode = OutboundMode::from_api(&config.mode);
        let port = config.system_proxy_port().map_or_else(
            || "未检测到可用代理端口".to_owned(),
            |port| format!("本机代理 127.0.0.1:{port}"),
        );

        home_card("快速控制", IconName::Settings2, theme)
            .w_full()
            .child(
                h_flex()
                    .items_start()
                    .flex_wrap()
                    .gap_5()
                    .p_4()
                    .child(
                        v_flex()
                            .min_w(rems(18.))
                            .flex_1()
                            .gap_3()
                            .child(
                                h_flex()
                                    .justify_between()
                                    .gap_3()
                                    .child(
                                        v_flex()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .child("系统代理"),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(if active {
                                                        "系统流量正通过 ZenClash"
                                                    } else {
                                                        "系统流量尚未接入"
                                                    }),
                                            ),
                                    )
                                    .child(
                                        Switch::new("home-system-proxy")
                                            .checked(active)
                                            .disabled(self.mutating)
                                            .on_click(cx.listener(|this, checked, _, cx| {
                                                this.toggle_system_proxy(*checked, cx);
                                            })),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .justify_between()
                                    .gap_3()
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_family(theme.mono_font_family.clone())
                                            .text_color(theme.muted_foreground)
                                            .child(port),
                                    )
                                    .child(
                                        Button::new("home-open-system-proxy")
                                            .icon(IconName::ArrowRight)
                                            .label("代理设置")
                                            .small()
                                            .ghost()
                                            .on_click(|_, window, cx| {
                                                window.dispatch_action(
                                                    Box::new(NavigateSystemProxy),
                                                    cx,
                                                );
                                            }),
                                    ),
                            ),
                    )
                    .child(
                        v_flex()
                            .min_w(rems(18.))
                            .flex_1()
                            .gap_3()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child("路由模式"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(mode_description(mode)),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(mode_button(
                                        "home-mode-rule",
                                        OutboundMode::Rule,
                                        mode,
                                        SetRuleMode,
                                    ))
                                    .child(mode_button(
                                        "home-mode-global",
                                        OutboundMode::Global,
                                        mode,
                                        SetGlobalMode,
                                    ))
                                    .child(mode_button(
                                        "home-mode-direct",
                                        OutboundMode::Direct,
                                        mode,
                                        SetDirectMode,
                                    )),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_home_traffic(
        &self,
        connections: &zenclash_core::ConnectionsSnapshot,
        theme: &gpui_component::Theme,
    ) -> gpui::AnyElement {
        let traffic = self.traffic_monitor.snapshot();
        let status_color = if traffic.connected {
            theme.success
        } else {
            theme.warning
        };
        let points = traffic_chart_points(&self.traffic_samples);
        let chart = AreaChart::new(points)
            .x(|point| point.label.clone())
            .y(|point| point.download)
            .stroke(theme.chart_1)
            .fill(theme.chart_1.opacity(0.18))
            .natural()
            .y(|point| point.upload)
            .stroke(theme.chart_2)
            .fill(theme.chart_2.opacity(0.14))
            .natural()
            .tick_margin(12);

        home_card("实时流量", IconName::ChartPie, theme)
            .w_full()
            .child(
                v_flex()
                    .p_4()
                    .gap_4()
                    .child(
                        h_flex()
                            .flex_wrap()
                            .justify_between()
                            .gap_3()
                            .child(
                                h_flex()
                                    .gap_4()
                                    .child(status_label(
                                        if traffic.connected {
                                            "实时更新"
                                        } else {
                                            "正在重连"
                                        },
                                        status_color,
                                        theme,
                                    ))
                                    .child(series_label(
                                        "下载",
                                        format_speed(traffic.download),
                                        theme.chart_1,
                                        theme,
                                    ))
                                    .child(series_label(
                                        "上传",
                                        format_speed(traffic.upload),
                                        theme.chart_2,
                                        theme,
                                    )),
                            )
                            .child(
                                Button::new("home-open-traffic")
                                    .icon(IconName::ArrowRight)
                                    .label("用量详情")
                                    .small()
                                    .ghost()
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(NavigateTraffic), cx);
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .h(rems(9.5))
                            .w_full()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.background.opacity(0.36))
                            .p_3()
                            .child(chart),
                    )
                    .child(
                        h_flex()
                            .gap_5()
                            .flex_wrap()
                            .child(traffic_metric(
                                "当前下载",
                                format_speed(traffic.download),
                                theme.chart_1,
                                theme,
                            ))
                            .child(traffic_metric(
                                "当前上传",
                                format_speed(traffic.upload),
                                theme.chart_2,
                                theme,
                            ))
                            .child(traffic_metric(
                                "活动连接",
                                connections.connections.len().to_string(),
                                theme.foreground,
                                theme,
                            ))
                            .child(traffic_metric(
                                "内核内存",
                                format_bytes(connections.memory),
                                theme.foreground,
                                theme,
                            )),
                    ),
            )
            .into_any_element()
    }
}

#[derive(Clone, Debug, PartialEq)]
struct TrafficChartPoint {
    label: SharedString,
    upload: f64,
    download: f64,
}

fn traffic_chart_points(samples: &VecDeque<LiveTrafficSample>) -> Vec<TrafficChartPoint> {
    let last_ix = samples.len().saturating_sub(1);
    samples
        .iter()
        .enumerate()
        .map(|(ix, sample)| {
            let remaining_half_seconds = last_ix.saturating_sub(ix);
            let label = if remaining_half_seconds == 0 {
                "现在".into()
            } else {
                format!("−{}s", remaining_half_seconds.div_ceil(2)).into()
            };
            TrafficChartPoint {
                label,
                upload: chart_value(sample.upload),
                download: chart_value(sample.download),
            }
        })
        .collect()
}

fn chart_value(bytes_per_second: u64) -> f64 {
    f64::from(u32::try_from(bytes_per_second).unwrap_or(u32::MAX))
}

struct CurrentProxySummary {
    group: String,
    node: String,
    kind: String,
    delay: Option<u32>,
}

fn current_proxy_summary(config: &RuntimeConfig, proxies: &ProxyCatalog) -> CurrentProxySummary {
    let mode = OutboundMode::from_api(&config.mode);
    if mode == OutboundMode::Direct {
        return CurrentProxySummary {
            group: "直连模式".into(),
            node: "DIRECT".into(),
            kind: "所有连接绕过代理节点".into(),
            delay: None,
        };
    }
    let Some(group) = proxies.groups_for_mode(&config.mode).next() else {
        return CurrentProxySummary {
            group: "没有可用策略组".into(),
            node: "未选择节点".into(),
            kind: "请到代理详情检查当前配置".into(),
            delay: None,
        };
    };
    let node = group.all.iter().find(|node| node.name == group.now);
    CurrentProxySummary {
        group: group.name.clone(),
        node: group.now.clone(),
        kind: node.map_or_else(
            || group.kind.clone(),
            |node| {
                let capabilities = node.capabilities().collect::<Vec<_>>().join(" · ");
                if capabilities.is_empty() {
                    node.kind.clone()
                } else {
                    format!("{} · {capabilities}", node.kind)
                }
            },
        ),
        delay: node.and_then(zenclash_core::ProxyNode::latest_delay),
    }
}

fn subscription_usage(usage: &SubscriptionUsage) -> (String, f32) {
    let used = usage.used();
    let quota = if usage.total == 0 {
        format!("已用 {}", format_bytes(used))
    } else {
        format!("{} / {}", format_bytes(used), format_bytes(usage.total))
    };
    let expiry = remaining_days(usage.expire).map_or_else(
        || "到期时间未提供".to_owned(),
        |days| {
            if days == 0 {
                "已到期".to_owned()
            } else {
                format!("剩余 {days} 天")
            }
        },
    );
    let percent = if usage.total == 0 {
        0.
    } else {
        100. * normalized_fraction(used, usage.total)
    };
    (format!("{quota} · {expiry}"), percent)
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

fn home_card(title: &'static str, icon: IconName, theme: &gpui_component::Theme) -> gpui::Div {
    v_flex()
        .min_w(rems(20.))
        .flex_1()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .overflow_hidden()
        .when(theme.shadow, |this| this.shadow_sm())
        .child(
            h_flex()
                .h_12()
                .px_4()
                .gap_3()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.muted.opacity(0.24))
                .child(
                    div()
                        .size_7()
                        .rounded(theme.radius)
                        .bg(theme.primary.opacity(0.12))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::new(icon).size_4().text_color(theme.primary)),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title),
                ),
        )
}

fn mode_button<A>(
    id: &'static str,
    value: OutboundMode,
    selected: OutboundMode,
    action: A,
) -> Button
where
    A: gpui::Action + Clone + 'static,
{
    Button::new(id)
        .label(value.label())
        .small()
        .outline()
        .selected(value == selected)
        .on_click(move |_, window, cx| {
            window.dispatch_action(Box::new(action.clone()), cx);
        })
}

const fn mode_description(mode: OutboundMode) -> &'static str {
    match mode {
        OutboundMode::Rule => "按规则决定每条连接的去向，适合日常使用",
        OutboundMode::Global => "全部连接使用当前全局代理节点",
        OutboundMode::Direct => "全部连接直连，不经过代理节点",
    }
}

fn status_label(
    label: &'static str,
    color: gpui::Hsla,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    h_flex()
        .gap_2()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(div().size_2().rounded_full().bg(color))
        .child(label)
        .into_any_element()
}

fn series_label(
    label: &'static str,
    value: String,
    color: gpui::Hsla,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    h_flex()
        .gap_2()
        .text_xs()
        .child(div().w_4().h_0p5().rounded_full().bg(color))
        .child(div().text_color(theme.muted_foreground).child(label))
        .child(
            div()
                .font_family(theme.mono_font_family.clone())
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(value),
        )
        .into_any_element()
}

fn traffic_metric(
    label: &'static str,
    value: String,
    color: gpui::Hsla,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    v_flex()
        .min_w(rems(7.5))
        .flex_1()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(
            div()
                .font_family(theme.mono_font_family.clone())
                .text_sm()
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(color)
                .child(value),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use zenclash_core::{DelayHistory, ProxyGroup, ProxyNode};

    use super::*;

    #[test]
    fn direct_mode_summary_does_not_require_a_proxy_group() {
        let summary = current_proxy_summary(
            &RuntimeConfig {
                mode: "direct".into(),
                ..RuntimeConfig::default()
            },
            &ProxyCatalog::default(),
        );

        assert_eq!(summary.node, "DIRECT");
    }

    #[test]
    fn rule_mode_summary_uses_the_primary_group_current_node() {
        let catalog = ProxyCatalog {
            groups: vec![ProxyGroup {
                name: "Proxy".into(),
                now: "HK 01".into(),
                all: vec![ProxyNode {
                    name: "HK 01".into(),
                    kind: "Hysteria2".into(),
                    history: vec![DelayHistory {
                        delay: 42,
                        ..DelayHistory::default()
                    }],
                    ..ProxyNode::default()
                }],
                ..ProxyGroup::default()
            }],
            proxy_count: 1,
        };

        let summary = current_proxy_summary(
            &RuntimeConfig {
                mode: "rule".into(),
                ..RuntimeConfig::default()
            },
            &catalog,
        );

        assert_eq!(summary.delay, Some(42));
    }

    #[test]
    fn traffic_chart_points_keep_upload_and_download_separate() {
        let samples = VecDeque::from([
            LiveTrafficSample {
                upload: 10,
                download: 20,
            },
            LiveTrafficSample {
                upload: 30,
                download: 40,
            },
        ]);

        let points = traffic_chart_points(&samples);

        assert_eq!((points[1].upload, points[1].download), (30., 40.));
    }

    #[test]
    fn traffic_chart_last_point_is_labeled_as_now() {
        let samples = VecDeque::from([LiveTrafficSample::default(); 3]);

        let points = traffic_chart_points(&samples);

        assert_eq!(points[2].label, "现在");
    }
}
