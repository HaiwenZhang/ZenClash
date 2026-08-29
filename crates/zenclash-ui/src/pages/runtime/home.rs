use std::{
    collections::VecDeque,
    time::{SystemTime, UNIX_EPOCH},
};

use gpui::{IntoElement, ParentElement, SharedString, Styled, div, px, rems};
use gpui_component::{
    Disableable, Icon, IconName, Selectable, Sizable,
    button::{Button, ButtonVariants},
    chart::AreaChart,
    h_flex,
    menu::{DropdownMenu, PopupMenuItem},
    progress::Progress,
    switch::Switch,
    v_flex,
};
use zenclash_core::{
    CapabilityState, CaptureOutcome, CapturePlan, CaptureStatus, ConnectionPolicy, Observation,
    OperationalSnapshot, ProcessRecoveryStatus, ProcessStatus, ProxyCatalog, ProxyGroup,
    ProxyGroupBehavior, ProxyOperations, RuntimeConfig, StreamStatus, StreamStatuses,
    SubscriptionUsage, SystemProxyOwnershipState, TrafficSample, format_speed,
};

use crate::{
    app::{
        NavigateProfiles, NavigateProxies, NavigateSystemProxy, NavigateTraffic, SetDirectMode,
        SetGlobalMode, SetRuleMode,
    },
    components::sidebar::OutboundMode,
};

use super::{
    Context, FluentBuilder, Page, ProxySelectionChanged, RuntimeData, RuntimePage, format_bytes,
    format_profile_age, load_page, message_banner, normalized_fraction,
};

const LIVE_TRAFFIC_TICK_MARGIN: usize = 6;
const MIN_TRAFFIC_CHART_CEILING: u64 = 1_024;

#[derive(Default)]
pub(super) struct HomeUiState {
    pub(super) profile_switching: Option<String>,
    pub(super) proxy_switching: Option<(String, String)>,
    pub(super) proxy_error: Option<String>,
    pub(super) capture_pending: Option<CapturePlan>,
    pub(super) action_error: Option<String>,
    mode_transition: Option<ModeTransition>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ModeTransition {
    displayed: OutboundMode,
    pending: bool,
}

impl RuntimePage {
    pub(in crate::pages::runtime) fn render_home(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let fallback_config = RuntimeConfig::default();
        let fallback_proxies = ProxyCatalog::default();
        let (config, proxies) = match &self.data {
            RuntimeData::Dashboard { config, proxies } => (
                config.value().unwrap_or(&fallback_config),
                proxies.value().unwrap_or(&fallback_proxies),
            ),
            _ => (&fallback_config, &fallback_proxies),
        };
        let operational = self.operational_status.snapshot();

        v_flex()
            .gap_4()
            .child(self.render_home_controls(config, &operational.capture, theme, cx))
            .when_some(
                self.app_update
                    .status
                    .as_ref()
                    .and_then(|status| match status {
                        zenclash_core::AppUpdateStatus::Available { release, .. } => {
                            Some(release.tag.clone())
                        }
                        zenclash_core::AppUpdateStatus::NoPublishedRelease { .. }
                        | zenclash_core::AppUpdateStatus::UpToDate { .. } => None,
                    }),
                |this, version| {
                    this.child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .px_4()
                            .py_3()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.success.opacity(0.35))
                            .bg(theme.success.opacity(0.08))
                            .child(div().text_sm().child(zenclash_i18n::text_with(
                                "home.app_update.available",
                                &[("version", version)],
                            )))
                            .child(
                                Button::new("home-view-app-update")
                                    .icon(IconName::ExternalLink)
                                    .label(zenclash_i18n::text("home.app_update.action"))
                                    .small()
                                    .outline()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.switch_to(Page::Settings, cx);
                                    })),
                            ),
                    )
                },
            )
            .child(self.render_home_evidence(&operational, theme))
            .child(
                h_flex()
                    .items_start()
                    .gap_4()
                    .flex_wrap()
                    .child(self.render_home_profile(theme, cx))
                    .child(self.render_home_proxy(config, proxies, theme, cx)),
            )
            .child(self.render_home_traffic(&operational.streams, theme))
            .into_any_element()
    }

    fn render_home_evidence(
        &self,
        operational: &OperationalSnapshot,
        theme: &gpui_component::Theme,
    ) -> gpui::AnyElement {
        let process = operational.process.value();
        let controller = operational.controller.value();
        let controller_ready = controller.is_some_and(|controller| controller.authenticated);
        let path_ready = operational.path.value().is_some();
        let (process_text, process_color) = process_evidence(process, theme);
        v_flex()
            .gap_2()
            .px_4()
            .py_3()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                h_flex()
                    .gap_4()
                    .flex_wrap()
                    .child(status_label(process_text, process_color, theme))
                    .child(status_label(
                        zenclash_i18n::text(if controller_ready {
                            "home.evidence.controller_verified"
                        } else {
                            "home.evidence.controller_unverified"
                        }),
                        if controller_ready {
                            theme.success
                        } else {
                            theme.warning
                        },
                        theme,
                    ))
                    .child(status_label(
                        capture_status_text(&operational.capture),
                        if operational.capture.is_active() {
                            theme.success
                        } else {
                            theme.warning
                        },
                        theme,
                    ))
                    .child(status_label(
                        zenclash_i18n::text(if path_ready {
                            "home.evidence.path_observed"
                        } else {
                            "home.evidence.path_unknown"
                        }),
                        if path_ready {
                            theme.success
                        } else {
                            theme.muted_foreground
                        },
                        theme,
                    )),
            )
            .when_some(
                process.and_then(|process| process.exit_reason.as_ref()),
                |this, reason| {
                    this.child(div().text_xs().text_color(theme.muted_foreground).child(
                        zenclash_i18n::text_with(
                            "home.evidence.core_exit_reason",
                            &[("reason", reason.clone())],
                        ),
                    ))
                },
            )
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
                        ("source", profile.source_label()),
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
            .loading(self.home.profile_switching.is_some())
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
        let can_switch = group.as_ref().is_some_and(home_group_can_switch);
        let nodes = group.as_ref().map_or_else(Vec::new, |group| {
            group
                .all
                .iter()
                .map(|node| node.name.clone())
                .collect::<Vec<_>>()
        });
        let can_switch = can_switch && !nodes.is_empty();
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
            .loading(self.home.proxy_switching.is_some())
            .disabled(
                self.home.profile_switching.is_some()
                    || self.home.proxy_switching.is_some()
                    || !can_switch,
            )
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
                .when_some(self.home.proxy_error.clone(), |this, error| {
                    this.child(message_banner(error, theme.danger, theme))
                })
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
        capture: &CaptureStatus,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let proxy = capture.system_proxy.value();
        let intent_enabled = proxy.map_or(self.preferences.system_proxy_enabled, |snapshot| {
            snapshot.intent_enabled
        });
        let proxy_status = system_proxy_status_text(capture);
        let tun_enabled = capture
            .tun
            .value()
            .map_or(config.tun.enable, |tun| tun.requested || tun.configured);
        let tun_supported = capture
            .tun
            .value()
            .is_none_or(|tun| tun.observed != CapabilityState::Unsupported);
        let tun_status = tun_status_text(capture);
        let capture_status = capture_status_text(capture);
        let presentation = mode_presentation(
            OutboundMode::from_api(&config.mode),
            self.home.mode_transition,
        );
        let mode = presentation.displayed;
        let mode_pending = presentation.pending;
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
                .when_some(self.home.action_error.clone(), |this, error| {
                    this.child(
                        div()
                            .w_full()
                            .child(message_banner(error, theme.danger, theme)),
                    )
                })
                .child(
                    v_flex()
                        .min_w(rems(18.))
                        .flex_1()
                        .gap_3()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(zenclash_i18n::text("home.controls.capture_description")),
                        )
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
                                                .child(proxy_status),
                                        ),
                                )
                                .child(
                                    Switch::new("home-system-proxy")
                                        .checked(intent_enabled)
                                        .disabled(self.home.capture_pending.is_some())
                                        .on_click(cx.listener(|this, checked, _, cx| {
                                            this.apply_home_capture_plan(
                                                if *checked {
                                                    CapturePlan::SystemProxy
                                                } else {
                                                    CapturePlan::Off
                                                },
                                                cx,
                                            );
                                        })),
                                ),
                        )
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
                                                .child(zenclash_i18n::text("home.controls.tun")),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .child(tun_status),
                                        ),
                                )
                                .child(
                                    Switch::new("home-tun")
                                        .checked(tun_enabled)
                                        .disabled(
                                            self.home.capture_pending.is_some() || !tun_supported,
                                        )
                                        .on_click(cx.listener(|this, checked, _, cx| {
                                            this.apply_home_capture_plan(
                                                if *checked {
                                                    CapturePlan::Tun
                                                } else {
                                                    CapturePlan::Off
                                                },
                                                cx,
                                            );
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
                                        .child(format!("{port} · {capture_status}")),
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
                                .child(div().text_xs().text_color(theme.muted_foreground).child(
                                    if mode_pending {
                                        zenclash_i18n::text_with(
                                            "home.controls.mode_switching",
                                            &[("mode", mode.label())],
                                        )
                                    } else {
                                        mode_description(mode)
                                    },
                                )),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(mode_button(
                                    "home-mode-rule",
                                    OutboundMode::Rule,
                                    mode,
                                    mode_pending,
                                    SetRuleMode,
                                ))
                                .child(mode_button(
                                    "home-mode-global",
                                    OutboundMode::Global,
                                    mode,
                                    mode_pending,
                                    SetGlobalMode,
                                ))
                                .child(mode_button(
                                    "home-mode-direct",
                                    OutboundMode::Direct,
                                    mode,
                                    mode_pending,
                                    SetDirectMode,
                                )),
                        ),
                ),
        )
        .into_any_element()
    }

    fn render_home_traffic(
        &self,
        streams: &StreamStatuses,
        theme: &gpui_component::Theme,
    ) -> gpui::AnyElement {
        let traffic = self.traffic_monitor.snapshot();
        let (traffic_status, status_color) = stream_status_text(&streams.traffic, theme);
        let (connections_status, connections_color) =
            stream_status_text(&streams.connections, theme);
        let connections = streams.connections.value();
        let samples = self.traffic_monitor.samples();
        let points = traffic_chart_points(&samples);
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
                                .child(status_label(traffic_status, status_color, theme))
                                .child(status_label(
                                    zenclash_i18n::text_with(
                                        "home.traffic.connections_status",
                                        &[("status", connections_status)],
                                    ),
                                    connections_color,
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
                            connections
                                .map_or(0, |status| status.item_count)
                                .to_string(),
                            theme.foreground,
                            theme,
                        ))
                        .child(traffic_metric(
                            zenclash_i18n::text("home.traffic.core_memory"),
                            format_bytes(connections.map_or(0, |status| status.memory)),
                            theme.foreground,
                            theme,
                        )),
                ),
        )
        .into_any_element()
    }

    fn change_home_proxy(&mut self, group: String, proxy: String, cx: &mut Context<Self>) {
        if self.page != Page::Home
            || self.home.profile_switching.is_some()
            || self.home.proxy_switching.is_some()
        {
            return;
        }
        self.invalidate_page_load();
        let token = self.page_task_token_for(Page::Home);
        self.home.proxy_switching = Some((group.clone(), proxy.clone()));
        self.home.proxy_error = None;
        let client = self.client.clone();
        let task_group = group.clone();
        let task_proxy = proxy.clone();
        let task = self.runtime.spawn(async move {
            ProxyOperations::new(client)
                .apply_selection(&task_group, &task_proxy, ConnectionPolicy::KeepExisting)
                .await
                .map_err(|error| error.to_string())
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
                if this.home.proxy_switching.as_ref() != Some(&(group.clone(), proxy.clone())) {
                    return;
                }
                this.home.proxy_switching = None;
                match result {
                    Ok(receipt) => {
                        for warning in receipt.warnings {
                            tracing::warn!(%warning, "home proxy selection completed with a warning");
                        }
                        cx.emit(ProxySelectionChanged);
                        if this.is_page_task_current(token) {
                            apply_home_proxy_selection(&mut this.data, &group, &proxy);
                            this.notice = Some(zenclash_i18n::text_with(
                                "home.proxy.switched",
                                &[("name", proxy.clone())],
                            ));
                            this.refresh(cx);
                        }
                    }
                    Err(error) => {
                        if this.is_page_task_current(token) {
                            this.home.proxy_error = Some(error);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn apply_home_capture_plan(&mut self, plan: CapturePlan, cx: &mut Context<Self>) {
        if self.page != Page::Home || self.home.capture_pending.is_some() {
            return;
        }
        self.home.capture_pending = Some(plan);
        self.home.action_error = None;
        let token = self.page_task_token_for(Page::Home);
        let capture = self.traffic_capture.clone();
        let client = self.client.clone();
        let preferences_store = self.preferences_store.clone();
        let task = self.runtime.spawn(async move {
            let outcome = capture
                .apply(plan)
                .await
                .map_err(|error| error.to_string())?;
            let preferences = if let Some(store) = preferences_store {
                Some(
                    tokio::task::spawn_blocking(move || store.load())
                        .await
                        .map_err(|error| error.to_string())?
                        .map_err(|error| error.to_string())?,
                )
            } else {
                None
            };
            let data = load_page(client, Page::Home).await;
            Ok::<_, String>((outcome, preferences, data))
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.home.capture_pending = None;
                match result {
                    Ok((CaptureOutcome::RolledBack { failure, .. }, _, _))
                    | Ok((CaptureOutcome::ReconcileNeeded { failure, .. }, _, _)) => {
                        this.home.action_error = Some(failure);
                    }
                    Ok((_, preferences, data)) => {
                        if let Some(preferences) = preferences {
                            this.preferences = preferences.clone();
                            cx.emit(super::PreferencesRestored { preferences });
                        }
                        match data {
                            Ok(data) => {
                                this.replace_page_data(token, data);
                            }
                            Err(error) => this.home.action_error = Some(error),
                        }
                    }
                    Err(error) => this.home.action_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn begin_home_mode_transition(
        &mut self,
        displayed: OutboundMode,
        pending: bool,
        cx: &mut Context<Self>,
    ) {
        self.home.mode_transition = Some(ModeTransition { displayed, pending });
        cx.notify();
    }

    pub(crate) fn update_home_mode_transition_if_active(
        &mut self,
        displayed: OutboundMode,
        pending: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(current) = self.home.mode_transition else {
            return;
        };
        let confirmed = self
            .config()
            .map(|config| OutboundMode::from_api(&config.mode));
        let next = if !pending && confirmed == Some(displayed) {
            None
        } else {
            Some(ModeTransition { displayed, pending })
        };
        if next != Some(current) {
            self.home.mode_transition = next;
            cx.notify();
        }
    }
}

fn apply_home_proxy_selection(data: &mut RuntimeData, group_name: &str, proxy_name: &str) -> bool {
    let RuntimeData::Dashboard { proxies, .. } = data else {
        return false;
    };
    let catalog = match proxies {
        Observation::Fresh { value, .. } | Observation::Stale { value, .. } => value,
        Observation::Loading | Observation::Failed { .. } => return false,
    };
    let Some(group) = catalog
        .groups
        .iter_mut()
        .find(|group| group.name == group_name)
    else {
        return false;
    };
    group.now = proxy_name.to_owned();
    if matches!(group.behavior, ProxyGroupBehavior::Automatic { .. }) {
        group.behavior = ProxyGroupBehavior::Automatic { fixed: true };
    }
    true
}

#[derive(Clone, Debug, PartialEq)]
struct TrafficChartPoint {
    label: SharedString,
    upload: f64,
    download: f64,
    ceiling: f64,
}

fn traffic_chart_points(samples: &VecDeque<TrafficSample>) -> Vec<TrafficChartPoint> {
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

fn traffic_chart_ceiling(samples: &VecDeque<TrafficSample>) -> f64 {
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
    if matches!(group.behavior, ProxyGroupBehavior::LoadBalance) {
        return CurrentProxySummary {
            group: group.name.clone(),
            node: zenclash_i18n::text("home.proxy.load_balance"),
            kind: zenclash_i18n::text("home.proxy.load_balance_description"),
            delay: None,
        };
    }
    let node = group.all.iter().find(|node| node.name == group.now);
    let behavior = match group.behavior {
        ProxyGroupBehavior::Automatic { fixed: true } => {
            Some(zenclash_i18n::text("home.proxy.fixed"))
        }
        ProxyGroupBehavior::Automatic { fixed: false } => {
            Some(zenclash_i18n::text("home.proxy.automatic"))
        }
        _ => None,
    };
    CurrentProxySummary {
        group: group.name.clone(),
        node: group.now.clone(),
        kind: behavior
            .into_iter()
            .chain(node.map(|node| {
                let capabilities = node.capabilities().collect::<Vec<_>>().join(" · ");
                if capabilities.is_empty() {
                    node.kind.clone()
                } else {
                    format!("{} · {capabilities}", node.kind)
                }
            }))
            .collect::<Vec<_>>()
            .join(" · "),
        delay: node.and_then(zenclash_core::ProxyNode::latest_delay),
    }
}

fn home_group_can_switch(group: &ProxyGroup) -> bool {
    matches!(
        group.behavior,
        ProxyGroupBehavior::Selector | ProxyGroupBehavior::Automatic { .. }
    )
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

fn system_proxy_status_text(capture: &CaptureStatus) -> String {
    match &capture.system_proxy {
        Observation::Loading => zenclash_i18n::text("home.controls.system_proxy_loading"),
        Observation::Failed { .. } => zenclash_i18n::text("home.controls.system_proxy_unavailable"),
        Observation::Fresh { value, .. } => describe_system_proxy(value),
        Observation::Stale {
            value,
            observed_at_ms,
            ..
        } => zenclash_i18n::text_with(
            "home.controls.system_proxy_stale",
            &[
                ("status", describe_system_proxy(value)),
                ("age", format_profile_age(observed_at_ms / 1_000)),
            ],
        ),
    }
}

fn describe_system_proxy(snapshot: &zenclash_core::SystemProxySessionSnapshot) -> String {
    if snapshot.ownership == SystemProxyOwnershipState::Lost {
        return zenclash_i18n::text("home.controls.system_proxy_ownership_lost");
    }
    if snapshot.intent_enabled
        && snapshot.actual.active()
        && snapshot.ownership == SystemProxyOwnershipState::Unowned
    {
        return zenclash_i18n::text("home.controls.system_proxy_ownership_unknown");
    }
    match (snapshot.intent_enabled, snapshot.actual.active()) {
        (true, true) => zenclash_i18n::text("home.controls.system_proxy_on"),
        (true, false) => zenclash_i18n::text("home.controls.system_proxy_intent_only"),
        (false, true) => zenclash_i18n::text("home.controls.system_proxy_external"),
        (false, false) => zenclash_i18n::text("home.controls.system_proxy_off"),
    }
}

fn tun_status_text(capture: &CaptureStatus) -> String {
    match &capture.tun {
        Observation::Loading => zenclash_i18n::text("home.controls.tun_loading"),
        Observation::Failed { .. } => zenclash_i18n::text("home.controls.tun_unavailable"),
        Observation::Fresh { value, .. } | Observation::Stale { value, .. } => describe_tun(value),
    }
}

fn describe_tun(snapshot: &zenclash_core::TunCaptureStatus) -> String {
    match snapshot.observed {
        CapabilityState::Active => zenclash_i18n::text("home.controls.tun_on"),
        CapabilityState::Unsupported => zenclash_i18n::text("home.controls.tun_unsupported"),
        CapabilityState::Unknown if snapshot.requested || snapshot.configured => {
            zenclash_i18n::text("home.controls.tun_unverified")
        }
        CapabilityState::Inactive if snapshot.requested || snapshot.configured => {
            zenclash_i18n::text("home.controls.tun_inactive")
        }
        CapabilityState::Inactive | CapabilityState::Unknown => {
            zenclash_i18n::text("home.controls.tun_off")
        }
    }
}

fn capture_status_text(capture: &CaptureStatus) -> String {
    if capture.is_active() {
        return zenclash_i18n::text("home.controls.capture_active");
    }
    if capture
        .tun
        .value()
        .is_some_and(|tun| tun.configured && matches!(tun.observed, CapabilityState::Unknown))
    {
        return zenclash_i18n::text("home.controls.capture_unverified");
    }
    if matches!(
        &capture.system_proxy,
        Observation::Failed { .. } | Observation::Loading
    ) || matches!(
        &capture.tun,
        Observation::Failed { .. } | Observation::Loading
    ) {
        return zenclash_i18n::text("home.controls.capture_unknown");
    }
    zenclash_i18n::text("home.controls.capture_off")
}

fn process_evidence(
    process: Option<&ProcessStatus>,
    theme: &gpui_component::Theme,
) -> (String, gpui::Hsla) {
    let (key, attempts) = process_evidence_copy(process);
    let text = attempts.map_or_else(
        || zenclash_i18n::text(key),
        |attempt| zenclash_i18n::text_with(key, &[("attempt", attempt.to_string())]),
    );
    let color = match process.map(|process| process.recovery) {
        Some(ProcessRecoveryStatus::Stable | ProcessRecoveryStatus::External)
            if process.is_some_and(|process| process.running) =>
        {
            theme.success
        }
        Some(ProcessRecoveryStatus::Recovering) => theme.warning,
        Some(
            ProcessRecoveryStatus::Stable
            | ProcessRecoveryStatus::Failed
            | ProcessRecoveryStatus::Stopped
            | ProcessRecoveryStatus::External,
        )
        | None => theme.danger,
    };
    (text, color)
}

fn process_evidence_copy(process: Option<&ProcessStatus>) -> (&'static str, Option<u32>) {
    let Some(process) = process else {
        return ("home.evidence.core_unavailable", None);
    };
    match process.recovery {
        ProcessRecoveryStatus::Recovering => (
            "home.evidence.core_recovering",
            Some(process.recovery_attempts),
        ),
        ProcessRecoveryStatus::Failed => (
            "home.evidence.core_recovery_failed",
            Some(process.recovery_attempts),
        ),
        ProcessRecoveryStatus::Stopped => ("home.evidence.core_stopped", None),
        ProcessRecoveryStatus::Stable if process.running && process.recovery_attempts > 0 => (
            "home.evidence.core_recovered",
            Some(process.recovery_attempts),
        ),
        ProcessRecoveryStatus::Stable | ProcessRecoveryStatus::External if process.running => {
            ("home.evidence.core_running", None)
        }
        ProcessRecoveryStatus::Stable | ProcessRecoveryStatus::External => {
            ("home.evidence.core_unavailable", None)
        }
    }
}

fn stream_status_text(
    observation: &Observation<StreamStatus>,
    theme: &gpui_component::Theme,
) -> (String, gpui::Hsla) {
    match observation {
        Observation::Fresh { .. } => (zenclash_i18n::text("home.traffic.live"), theme.success),
        Observation::Stale { observed_at_ms, .. } => (
            zenclash_i18n::text_with(
                "home.traffic.stale",
                &[("age", format_profile_age(observed_at_ms / 1_000))],
            ),
            theme.warning,
        ),
        Observation::Failed { .. } => (
            zenclash_i18n::text("home.traffic.unavailable"),
            theme.danger,
        ),
        Observation::Loading => (
            zenclash_i18n::text("home.traffic.reconnecting"),
            theme.warning,
        ),
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
    pending: bool,
    action: A,
) -> Button
where
    A: gpui::Action + Clone + 'static,
{
    let is_selected = value == selected;
    Button::new(id)
        .label(value.label())
        .small()
        .when(is_selected, |this| this.icon(IconName::Check).primary())
        .when(!is_selected, |this| this.outline())
        .selected(is_selected)
        .loading(is_selected && pending)
        .disabled(pending)
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

fn mode_presentation(
    confirmed: OutboundMode,
    transition: Option<ModeTransition>,
) -> ModeTransition {
    transition.unwrap_or(ModeTransition {
        displayed: confirmed,
        pending: false,
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
    use zenclash_core::{
        DelayHistory, ProxyGroup, ProxyGroupBehavior, ProxyNode, SystemProxySessionSnapshot,
        SystemProxyStatus, TunCaptureStatus, TunRuntimeObservation,
    };

    use super::*;

    #[test]
    fn process_recovery_attempts_have_a_visible_non_color_label() {
        let process = ProcessStatus {
            kind: zenclash_core::CoreKind::Mihomo,
            managed: true,
            running: false,
            generation: 3,
            exit_reason: Some("exit status: 23".into()),
            recovery_attempts: 2,
            recovery: ProcessRecoveryStatus::Recovering,
        };

        assert_eq!(
            process_evidence_copy(Some(&process)),
            ("home.evidence.core_recovering", Some(2))
        );
        assert_eq!(
            process_evidence_copy(Some(&ProcessStatus {
                recovery: ProcessRecoveryStatus::Failed,
                ..process.clone()
            })),
            ("home.evidence.core_recovery_failed", Some(2))
        );
        assert_eq!(
            process_evidence_copy(Some(&ProcessStatus {
                running: true,
                recovery: ProcessRecoveryStatus::Stable,
                ..process
            })),
            ("home.evidence.core_recovered", Some(2))
        );
    }

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
    fn acknowledged_home_proxy_selection_updates_the_local_group_immediately() {
        let mut data = RuntimeData::Dashboard {
            config: Observation::Loading,
            proxies: Observation::Fresh {
                value: ProxyCatalog {
                    groups: vec![ProxyGroup {
                        name: "Proxy".into(),
                        now: "HK 01".into(),
                        behavior: ProxyGroupBehavior::Automatic { fixed: false },
                        ..ProxyGroup::default()
                    }],
                    proxy_count: 1,
                },
                observed_at_ms: 10,
            },
        };

        assert!(apply_home_proxy_selection(&mut data, "Proxy", "US 02"));
        let RuntimeData::Dashboard { proxies, .. } = data else {
            panic!("expected dashboard data");
        };
        let group = &proxies.value().expect("fresh proxy catalog").groups[0];
        assert_eq!(group.now, "US 02");
        assert_eq!(
            group.behavior,
            ProxyGroupBehavior::Automatic { fixed: true }
        );
    }

    #[test]
    fn load_balance_summary_does_not_offer_a_fake_manual_selection() {
        let group = ProxyGroup {
            name: "Balance".into(),
            behavior: ProxyGroupBehavior::LoadBalance,
            now: "HK 01".into(),
            all: vec![ProxyNode {
                name: "HK 01".into(),
                ..ProxyNode::default()
            }],
            ..ProxyGroup::default()
        };
        let catalog = ProxyCatalog {
            groups: vec![group.clone()],
            proxy_count: 2,
        };

        let summary = current_proxy_summary(
            &RuntimeConfig {
                mode: "rule".into(),
                ..RuntimeConfig::default()
            },
            &catalog,
        );

        assert!(!home_group_can_switch(&group));
        assert_eq!(summary.node, zenclash_i18n::text("home.proxy.load_balance"));
        assert_eq!(summary.delay, None);
    }

    #[test]
    fn system_proxy_intent_actual_and_lost_ownership_have_distinct_copy() {
        let intent_only = SystemProxySessionSnapshot {
            intent_enabled: true,
            actual: SystemProxyStatus::default(),
            ownership: SystemProxyOwnershipState::Unowned,
        };
        let external = SystemProxySessionSnapshot {
            intent_enabled: false,
            actual: SystemProxyStatus {
                enabled: true,
                ..SystemProxyStatus::default()
            },
            ownership: SystemProxyOwnershipState::Unowned,
        };
        let lost = SystemProxySessionSnapshot {
            intent_enabled: true,
            actual: external.actual.clone(),
            ownership: SystemProxyOwnershipState::Lost,
        };

        assert_eq!(
            describe_system_proxy(&intent_only),
            zenclash_i18n::text("home.controls.system_proxy_intent_only")
        );
        assert_eq!(
            describe_system_proxy(&external),
            zenclash_i18n::text("home.controls.system_proxy_external")
        );
        assert_eq!(
            describe_system_proxy(&lost),
            zenclash_i18n::text("home.controls.system_proxy_ownership_lost")
        );
    }

    #[test]
    fn tun_status_copy_distinguishes_off_unverified_active_and_unsupported() {
        let mut status = TunCaptureStatus {
            requested: false,
            configured: false,
            permission: CapabilityState::Inactive,
            runtime: TunRuntimeObservation {
                device_name: None,
                device: CapabilityState::Inactive,
                route: CapabilityState::Inactive,
                detail: String::new(),
            },
            observed: CapabilityState::Inactive,
        };

        assert_eq!(
            describe_tun(&status),
            zenclash_i18n::text("home.controls.tun_off")
        );

        status.requested = true;
        status.configured = true;
        status.observed = CapabilityState::Unknown;
        assert_eq!(
            describe_tun(&status),
            zenclash_i18n::text("home.controls.tun_unverified")
        );

        status.observed = CapabilityState::Active;
        assert_eq!(
            describe_tun(&status),
            zenclash_i18n::text("home.controls.tun_on")
        );

        status.observed = CapabilityState::Unsupported;
        assert_eq!(
            describe_tun(&status),
            zenclash_i18n::text("home.controls.tun_unsupported")
        );
    }

    #[test]
    fn traffic_chart_points_keep_upload_and_download_separate() {
        let samples = VecDeque::from([
            TrafficSample {
                upload: 10,
                download: 20,
            },
            TrafficSample {
                upload: 30,
                download: 40,
            },
        ]);

        let points = traffic_chart_points(&samples);

        assert_eq!((points[1].upload, points[1].download), (30., 40.));
    }

    #[test]
    fn traffic_chart_points_have_unique_x_axis_labels() {
        let samples = VecDeque::from([TrafficSample::default(); 24]);

        let points = traffic_chart_points(&samples);

        assert!(points.windows(2).all(|pair| pair[0].label != pair[1].label));
    }

    #[test]
    fn traffic_chart_ceiling_is_stable_within_one_power_of_two_band() {
        let lower = VecDeque::from([TrafficSample {
            download: 700 * 1_024,
            ..TrafficSample::default()
        }]);
        let higher = VecDeque::from([TrafficSample {
            download: 800 * 1_024,
            ..TrafficSample::default()
        }]);

        assert_eq!(
            traffic_chart_ceiling(&lower),
            traffic_chart_ceiling(&higher)
        );
    }

    #[test]
    fn traffic_chart_last_point_is_labeled_as_now() {
        let samples = VecDeque::from([TrafficSample::default(); 3]);

        let points = traffic_chart_points(&samples);

        assert!(matches!(points[2].label.as_ref(), "现在" | "Now"));
    }

    #[test]
    fn pending_mode_transition_is_presented_before_controller_readback() {
        let presentation = mode_presentation(
            OutboundMode::Rule,
            Some(ModeTransition {
                displayed: OutboundMode::Global,
                pending: true,
            }),
        );

        assert_eq!(
            (presentation.displayed, presentation.pending),
            (OutboundMode::Global, true)
        );
    }
}
