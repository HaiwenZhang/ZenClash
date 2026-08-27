use std::{
    collections::VecDeque,
    time::{SystemTime, UNIX_EPOCH},
};

use gpui::{div, px, rems, IntoElement, ParentElement, SharedString, Styled};
use gpui_component::{
    button::{Button, ButtonVariants},
    chart::AreaChart,
    h_flex,
    menu::{DropdownMenu, PopupMenuItem},
    progress::Progress,
    switch::Switch,
    v_flex, Disableable, Icon, IconName, Selectable, Sizable,
};
use zenclash_core::{
    format_speed, ProxyCatalog, ProxyGroup, RuntimeConfig, SubscriptionUsage, SystemProxyStatus,
};

use crate::{
    app::{
        NavigateProfiles, NavigateProxies, NavigateSystemProxy, NavigateTraffic, SetDirectMode,
        SetGlobalMode, SetRuleMode,
    },
    components::sidebar::OutboundMode,
};

use super::{
    format_bytes, format_profile_age, load_page, normalized_fraction, Context, FluentBuilder,
    LiveTrafficSample, Page, ProxySelectionChanged, RuntimeData, RuntimePage,
};

const LIVE_TRAFFIC_TICK_MARGIN: usize = 6;
const MIN_TRAFFIC_CHART_CEILING: u64 = 1_024;

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
                    .child(self.render_home_profile(theme, cx))
                    .child(self.render_home_proxy(config, proxies, theme, cx)),
            )
            .child(self.render_home_controls(config, system_proxy, theme, cx))
            .child(self.render_home_traffic(connections, theme))
            .into_any_element()
    }

    fn render_home_profile(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let active = self.profile_catalog.active_profile();
        let name = active.map_or_else(
            || zenclash_i18n::text("home.profile.none"),
            |profile| profile.name.clone(),
        );
        let source = active.map_or_else(
            || zenclash_i18n::text("home.profile.none_description"),
            |profile| {
                zenclash_i18n::text_with(
                    "home.profile.source_updated",
                    &[
                        ("source", profile.source_label().to_owned()),
                        ("age", format_profile_age(profile.updated_at)),
                    ],
                )
            },
        );
        let (usage, usage_percent) = active
            .and_then(|profile| profile.subscription.usage.as_ref())
            .map_or_else(
                || (zenclash_i18n::text("home.profile.usage_unavailable"), 0.),
                subscription_usage,
            );
        let active_id = self.profile_catalog.active.clone();
        let profiles = self
            .profile_catalog
            .profiles
            .iter()
            .map(|profile| (profile.id.clone(), profile.name.clone()))
            .collect::<Vec<_>>();
        let can_switch = self.profile_store.is_some() && !profiles.is_empty();
        let runtime_page = cx.entity().downgrade();
        let profile_switch_tooltip =
            zenclash_i18n::text_with("home.profile.switch_current", &[("name", name.clone())]);
        let profile_picker = Button::new("home-profile-picker")
            .label(name)
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .outline()
            .dropdown_caret(true)
            .tooltip(profile_switch_tooltip)
            .loading(self.home_profile_switching.is_some())
            .disabled(self.mutating || !can_switch)
            .dropdown_menu(move |mut menu, _, _| {
                menu = menu
                    .min_w(px(240.))
                    .max_w(px(420.))
                    .max_h(px(360.))
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
                menu
            });

        home_card(
            zenclash_i18n::text("home.profile.title"),
            IconName::FolderOpen,
            theme,
        )
        .min_h(rems(11.5))
        .child(
            v_flex()
                .flex_1()
                .p_4()
                .gap_3()
                .child(profile_picker)
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
                                .child(zenclash_i18n::text("home.profile.usage"))
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
                            .label(zenclash_i18n::text("home.profile.details"))
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
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let selection = current_proxy_summary(config, proxies);
        let latency_color = match selection.delay {
            Some(1..=499) => theme.success,
            Some(500..) => theme.warning,
            Some(0) => theme.danger,
            None => theme.muted_foreground,
        };
        let latency = selection.delay.map_or_else(
            || zenclash_i18n::text("home.proxy.untested"),
            |delay| format!("{delay} ms"),
        );
        let group = current_proxy_group(config, proxies).cloned();
        let group_name = group.as_ref().map(|group| group.name.clone());
        let active_node = group.as_ref().map(|group| group.now.clone());
        let nodes = group.map_or_else(Vec::new, |group| {
            group
                .all
                .into_iter()
                .map(|node| node.name)
                .collect::<Vec<_>>()
        });
        let can_switch = group_name.is_some() && !nodes.is_empty();
        let runtime_page = cx.entity().downgrade();
        let node_switch_tooltip = zenclash_i18n::text_with(
            "home.proxy.switch_current",
            &[("name", selection.node.clone())],
        );
        let node_picker = Button::new("home-proxy-picker")
            .label(selection.node)
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .outline()
            .dropdown_caret(true)
            .tooltip(node_switch_tooltip)
            .loading(self.home_proxy_switching.is_some())
            .disabled(self.mutating || !can_switch)
            .dropdown_menu(move |mut menu, _, _| {
                menu = menu
                    .min_w(px(240.))
                    .max_w(px(480.))
                    .max_h(px(360.))
                    .scrollable(true);
                let Some(group) = group_name.as_ref() else {
                    return menu;
                };
                for node in &nodes {
                    let is_active = active_node.as_deref() == Some(node.as_str());
                    let runtime_page = runtime_page.clone();
                    let group = group.clone();
                    let node = node.clone();
                    menu = menu.item(
                        PopupMenuItem::new(node.clone())
                            .checked(is_active)
                            .disabled(is_active)
                            .on_click(move |_, _, cx| {
                                let group = group.clone();
                                let node = node.clone();
                                let _ = runtime_page.update(cx, |page, cx| {
                                    page.change_home_proxy(group, node, cx);
                                });
                            }),
                    );
                }
                menu
            });

        home_card(
            zenclash_i18n::text("home.proxy.title"),
            IconName::GalleryVerticalEnd,
            theme,
        )
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
                    h_flex().gap_3().justify_between().child(node_picker).child(
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
                            .label(zenclash_i18n::text("home.proxy.details"))
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
            || zenclash_i18n::text("home.controls.no_proxy_port"),
            |port| {
                zenclash_i18n::text_with("home.controls.local_proxy", &[("port", port.to_string())])
            },
        );

        home_card(
            zenclash_i18n::text("home.controls.title"),
            IconName::Settings2,
            theme,
        )
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
                                                .child(zenclash_i18n::text("tray.system_proxy")),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .child(if active {
                                                    zenclash_i18n::text(
                                                        "home.controls.system_proxy_on",
                                                    )
                                                } else {
                                                    zenclash_i18n::text(
                                                        "home.controls.system_proxy_off",
                                                    )
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
                                        .label(zenclash_i18n::text("home.controls.proxy_settings"))
                                        .small()
                                        .ghost()
                                        .on_click(|_, window, cx| {
                                            window
                                                .dispatch_action(Box::new(NavigateSystemProxy), cx);
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
                                        .child(zenclash_i18n::text("home.controls.routing_mode")),
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
        let points = traffic_chart_points(self.live_traffic.samples());
        let chart = AreaChart::new(points)
            .x(|point| point.label.clone())
            .y(|point| point.download)
            .stroke(theme.chart_1)
            .fill(theme.chart_1.opacity(0.18))
            .linear()
            .y(|point| point.upload)
            .stroke(theme.chart_2)
            .fill(theme.chart_2.opacity(0.14))
            .linear()
            .y(|point| point.ceiling)
            .stroke(theme.background.opacity(0.0))
            .fill(theme.background.opacity(0.0))
            .linear()
            .tick_margin(LIVE_TRAFFIC_TICK_MARGIN);

        home_card(
            zenclash_i18n::text("home.traffic.title"),
            IconName::ChartPie,
            theme,
        )
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
                                        zenclash_i18n::text("home.traffic.live")
                                    } else {
                                        zenclash_i18n::text("home.traffic.reconnecting")
                                    },
                                    status_color,
                                    theme,
                                ))
                                .child(series_label(
                                    zenclash_i18n::text("home.traffic.download"),
                                    format_speed(traffic.download),
                                    theme.chart_1,
                                    theme,
                                ))
                                .child(series_label(
                                    zenclash_i18n::text("home.traffic.upload"),
                                    format_speed(traffic.upload),
                                    theme.chart_2,
                                    theme,
                                )),
                        )
                        .child(
                            Button::new("home-open-traffic")
                                .icon(IconName::ArrowRight)
                                .label(zenclash_i18n::text("home.traffic.details"))
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
                            zenclash_i18n::text("home.traffic.current_download"),
                            format_speed(traffic.download),
                            theme.chart_1,
                            theme,
                        ))
                        .child(traffic_metric(
                            zenclash_i18n::text("home.traffic.current_upload"),
                            format_speed(traffic.upload),
                            theme.chart_2,
                            theme,
                        ))
                        .child(traffic_metric(
                            zenclash_i18n::text("home.traffic.active_connections"),
                            connections.connections.len().to_string(),
                            theme.foreground,
                            theme,
                        ))
                        .child(traffic_metric(
                            zenclash_i18n::text("home.traffic.core_memory"),
                            format_bytes(connections.memory),
                            theme.foreground,
                            theme,
                        )),
                ),
        )
        .into_any_element()
    }

    fn change_home_proxy(&mut self, group: String, proxy: String, cx: &mut Context<Self>) {
        if self.page != Page::Home {
            return;
        }
        let Some(token) = self.begin_mutation(Page::Home) else {
            return;
        };
        self.home_proxy_switching = Some((group.clone(), proxy.clone()));
        let client = self.client.clone();
        let selected_proxy = proxy.clone();
        let task = self.runtime.spawn(async move {
            client
                .change_proxy(&group, &proxy)
                .await
                .map_err(|error| error.to_string())?;
            client
                .close_all_connections()
                .await
                .map_err(|error| error.to_string())?;
            load_page(client, Page::Home).await
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "proxies.errors.switch_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                this.home_proxy_switching = None;
                match result {
                    Ok(data) => {
                        cx.emit(ProxySelectionChanged);
                        if this.replace_page_data(token, data) {
                            this.notice = Some(zenclash_i18n::text_with(
                                "home.proxy.switched",
                                &[("name", selected_proxy)],
                            ));
                        }
                    }
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

#[derive(Clone, Debug, PartialEq)]
struct TrafficChartPoint {
    label: SharedString,
    upload: f64,
    download: f64,
    ceiling: f64,
}

fn traffic_chart_points(samples: &VecDeque<LiveTrafficSample>) -> Vec<TrafficChartPoint> {
    let last_ix = samples.len().saturating_sub(1);
    let ceiling = traffic_chart_ceiling(samples);
    samples
        .iter()
        .enumerate()
        .map(|(ix, sample)| {
            let remaining_seconds = last_ix.saturating_sub(ix);
            let label = if remaining_seconds == 0 {
                zenclash_i18n::text("home.traffic.now").into()
            } else {
                format!("−{remaining_seconds}s").into()
            };
            TrafficChartPoint {
                label,
                upload: chart_value(sample.upload),
                download: chart_value(sample.download),
                ceiling,
            }
        })
        .collect()
}

fn traffic_chart_ceiling(samples: &VecDeque<LiveTrafficSample>) -> f64 {
    let peak = samples
        .iter()
        .map(|sample| sample.upload.max(sample.download))
        .max()
        .unwrap_or_default()
        .min(u64::from(u32::MAX));
    let peak_with_headroom = peak.saturating_add(peak / 10);
    let ceiling = peak_with_headroom
        .max(MIN_TRAFFIC_CHART_CEILING)
        .next_power_of_two();
    ceiling as f64
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
            group: zenclash_i18n::text("outbound_mode.direct_mode"),
            node: "DIRECT".into(),
            kind: zenclash_i18n::text("home.proxy.direct_description"),
            delay: None,
        };
    }
    let Some(group) = current_proxy_group(config, proxies) else {
        return CurrentProxySummary {
            group: zenclash_i18n::text("home.proxy.no_group"),
            node: zenclash_i18n::text("home.proxy.no_node"),
            kind: zenclash_i18n::text("home.proxy.check_configuration"),
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

fn current_proxy_group<'a>(
    config: &'a RuntimeConfig,
    proxies: &'a ProxyCatalog,
) -> Option<&'a ProxyGroup> {
    (OutboundMode::from_api(&config.mode) != OutboundMode::Direct)
        .then(|| proxies.groups_for_mode(&config.mode).next())
        .flatten()
}

fn subscription_usage(usage: &SubscriptionUsage) -> (String, f32) {
    let used = usage.used();
    let quota = if usage.total == 0 {
        zenclash_i18n::text_with("home.profile.used", &[("used", format_bytes(used))])
    } else {
        format!("{} / {}", format_bytes(used), format_bytes(usage.total))
    };
    let expiry = remaining_days(usage.expire).map_or_else(
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

fn home_card(
    title: impl Into<SharedString>,
    icon: IconName,
    theme: &gpui_component::Theme,
) -> gpui::Div {
    let title = title.into();
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

fn mode_description(mode: OutboundMode) -> String {
    zenclash_i18n::text(match mode {
        OutboundMode::Rule => "home.controls.mode_rule",
        OutboundMode::Global => "home.controls.mode_global",
        OutboundMode::Direct => "home.controls.mode_direct",
    })
}

fn status_label(
    label: impl Into<SharedString>,
    color: gpui::Hsla,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    let label = label.into();
    h_flex()
        .gap_2()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(div().size_2().rounded_full().bg(color))
        .child(label)
        .into_any_element()
}

fn series_label(
    label: impl Into<SharedString>,
    value: String,
    color: gpui::Hsla,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    let label = label.into();
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
    label: impl Into<SharedString>,
    value: String,
    color: gpui::Hsla,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    let label = label.into();
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
    use crate::pages::runtime::LiveTrafficSeries;

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
    fn traffic_chart_points_have_unique_x_axis_labels() {
        let samples = VecDeque::from([LiveTrafficSample::default(); 24]);

        let points = traffic_chart_points(&samples);

        assert!(points.windows(2).all(|pair| pair[0].label != pair[1].label));
    }

    #[test]
    fn traffic_chart_ceiling_is_stable_within_one_power_of_two_band() {
        let lower = VecDeque::from([LiveTrafficSample {
            download: 700 * 1_024,
            ..LiveTrafficSample::default()
        }]);
        let higher = VecDeque::from([LiveTrafficSample {
            download: 800 * 1_024,
            ..LiveTrafficSample::default()
        }]);

        assert_eq!(
            traffic_chart_ceiling(&lower),
            traffic_chart_ceiling(&higher)
        );
    }

    #[test]
    fn traffic_chart_last_point_is_labeled_as_now() {
        let samples = VecDeque::from([LiveTrafficSample::default(); 3]);

        let points = traffic_chart_points(&samples);

        assert!(matches!(points[2].label.as_ref(), "现在" | "Now"));
    }

    #[test]
    fn live_traffic_series_ignores_duplicate_monitor_reads() {
        let mut series = LiveTrafficSeries::default();
        let snapshot = zenclash_core::TrafficSnapshot {
            download: 1_024,
            connected: true,
            updated_at_ms: 1,
            ..zenclash_core::TrafficSnapshot::default()
        };
        series.observe(&snapshot);
        let samples_after_first_frame = series.samples.clone();

        series.observe(&snapshot);

        assert_eq!(series.samples, samples_after_first_frame);
    }

    #[test]
    fn live_traffic_series_does_not_insert_disconnect_zeroes() {
        let mut series = LiveTrafficSeries::default();
        series.observe(&zenclash_core::TrafficSnapshot {
            download: 1_024,
            connected: true,
            updated_at_ms: 1,
            ..zenclash_core::TrafficSnapshot::default()
        });
        let samples_before_disconnect = series.samples.clone();

        series.observe(&zenclash_core::TrafficSnapshot {
            updated_at_ms: 2,
            ..zenclash_core::TrafficSnapshot::default()
        });

        assert_eq!(series.samples, samples_before_disconnect);
    }
}
