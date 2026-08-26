use std::collections::HashSet;

mod actions;
mod model;

use model::{
    average_latency, format_asn, format_coordinates, format_proxy_flags, join_present,
    latency_color,
};

use super::{
    config_input_row, div, empty_dash, h_flex, info_row, json, message_banner, metric, px,
    setting_card, setting_switch, v_flex, Button, ButtonVariants, Context, Disableable,
    FluentBuilder, IconName, Input, IntoElement, NetworkLatencyTarget, NetworkProbeRoutePreference,
    NetworkProbeSnapshot, ParentElement, PublicIpProvider, RuntimeConfig, RuntimeData, RuntimePage,
    Selectable, Sizable, Styled, SystemNetworkSnapshot,
};

#[derive(Clone, Debug, Default)]
pub(super) struct NetworkProbeUiState {
    snapshot: Option<NetworkProbeSnapshot>,
    loading: bool,
    revision: u64,
}

#[derive(Clone, Debug)]
enum NetworkPreferenceChange {
    Provider(PublicIpProvider),
    ThroughMihomo(bool),
    AddTarget(NetworkLatencyTarget),
    RemoveTarget(String),
}

impl RuntimePage {
    pub(super) fn render_network(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (config, system) = match &self.data {
            RuntimeData::Network { config, system } => (config.clone(), system.clone()),
            _ => (RuntimeConfig::default(), SystemNetworkSnapshot::default()),
        };
        let snapshot = self.network_probe.snapshot.clone().unwrap_or_default();
        let average_latency = average_latency(&snapshot);
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(metric(
                        "公网出口",
                        snapshot
                            .public_ip
                            .as_ref()
                            .map_or_else(|| "等待探测".into(), |info| info.ip.clone()),
                        theme.primary,
                        theme,
                    ))
                    .child(metric(
                        "平均延迟",
                        average_latency.map_or_else(|| "—".into(), |value| format!("{value} ms")),
                        latency_color(average_latency, theme),
                        theme,
                    ))
                    .child(metric(
                        "探测路径",
                        empty_dash(&snapshot.route),
                        theme.warning,
                        theme,
                    )),
            )
            .child(self.render_public_ip_card(&snapshot, theme, cx))
            .child(self.render_latency_card(&snapshot, theme, cx))
            .child(self.render_system_network_card(&config, &system, theme, cx))
            .child(
                setting_card("网络能力", theme)
                    .child(info_row("IPv6", super::yes_no(config.ipv6), theme))
                    .child(info_row(
                        "允许局域网",
                        super::yes_no(config.allow_lan),
                        theme,
                    ))
                    .child(info_row(
                        "TCP 并发",
                        super::yes_no(config.tcp_concurrent),
                        theme,
                    ))
                    .child(info_row(
                        "统一延迟",
                        super::yes_no(config.unified_delay),
                        theme,
                    )),
            )
            .into_any_element()
    }

    fn render_public_ip_card(
        &self,
        snapshot: &NetworkProbeSnapshot,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let provider = self.preferences.network_ip_provider;
        let info = snapshot.public_ip.as_ref();
        setting_card("公网出口画像", theme)
            .child(
                h_flex()
                    .min_h(px(58.))
                    .px_4()
                    .gap_3()
                    .justify_between()
                    .child(
                        h_flex().gap_2().children(
                            PublicIpProvider::ALL.into_iter().enumerate().map(
                                |(index, candidate)| {
                                    Button::new(("network-provider", index))
                                        .label(candidate.label())
                                        .small()
                                        .outline()
                                        .selected(candidate == provider)
                                        .disabled(self.mutating || self.network_probe.loading)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.persist_network_preference(
                                                NetworkPreferenceChange::Provider(candidate),
                                                "公网 IP 数据源已保存",
                                                cx,
                                            );
                                        }))
                                },
                            ),
                        ),
                    )
                    .child(
                        Button::new("refresh-network-probe")
                            .icon(IconName::Redo2)
                            .label(if self.network_probe.loading {
                                "探测中"
                            } else {
                                "重新探测"
                            })
                            .small()
                            .primary()
                            .disabled(self.network_probe.loading || self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.refresh_network_probe(cx);
                            })),
                    ),
            )
            .when_some(snapshot.public_ip_error.clone(), |this, error| {
                this.child(message_banner(error, theme.danger, theme))
            })
            .when_some(info, |this, info| {
                this.child(info_row("公网 IP", &info.ip, theme))
                    .child(info_row(
                        "国家 / 地区",
                        &join_present(&[info.country.as_deref(), info.region.as_deref()]),
                        theme,
                    ))
                    .child(info_row("城市", info.city.as_deref().unwrap_or(""), theme))
                    .child(info_row(
                        "ASN / 组织",
                        &format_asn(info.asn, info.organization.as_deref()),
                        theme,
                    ))
                    .child(info_row("ISP", info.isp.as_deref().unwrap_or(""), theme))
                    .child(info_row(
                        "时区",
                        info.timezone.as_deref().unwrap_or(""),
                        theme,
                    ))
                    .child(info_row(
                        "坐标",
                        &format_coordinates(info.latitude, info.longitude),
                        theme,
                    ))
                    .child(info_row(
                        "代理识别",
                        &format_proxy_flags(info.is_proxy, info.is_vpn),
                        theme,
                    ))
            })
    }

    fn render_latency_card(
        &self,
        snapshot: &NetworkProbeSnapshot,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let custom_urls = self
            .preferences
            .network_latency_targets
            .iter()
            .map(|target| target.url.as_str())
            .collect::<HashSet<_>>();
        setting_card("独立延迟探测", theme)
            .child(setting_switch(
                "通过 Mihomo 探测",
                "使用当前 HTTP 或 Mixed 监听，结果反映真实代理出口；关闭后使用直连",
                self.preferences.network_probe_route == NetworkProbeRoutePreference::Mihomo,
                "network-probe-through-mihomo",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.persist_network_preference(
                        NetworkPreferenceChange::ThroughMihomo(*checked),
                        "网络探测路径已保存",
                        cx,
                    );
                }),
            ))
            .children(
                snapshot
                    .latencies
                    .iter()
                    .enumerate()
                    .map(|(index, result)| {
                        self.render_latency_result(
                            index,
                            result,
                            custom_urls.contains(result.target.url.as_str()),
                            theme,
                            cx,
                        )
                    }),
            )
            .child(config_input_row(
                "目标名称",
                "最多 64 个字符",
                Input::new(&self.network_latency_name),
                theme,
            ))
            .child(config_input_row(
                "探测 URL",
                "仅接受不含登录凭据的 HTTP(S) 地址",
                Input::new(&self.network_latency_url),
                theme,
            ))
            .child(
                h_flex().px_4().py_3().justify_end().child(
                    Button::new("add-network-latency-target")
                        .icon(IconName::Plus)
                        .label("添加探测目标")
                        .small()
                        .outline()
                        .disabled(
                            self.mutating || self.preferences.network_latency_targets.len() >= 13,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.add_network_latency_target(cx);
                        })),
                ),
            )
    }

    fn render_latency_result(
        &self,
        index: usize,
        result: &zenclash_core::NetworkLatencyResult,
        custom: bool,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let value = result.latency_ms.map_or_else(
            || result.error.as_deref().unwrap_or("请求失败").to_owned(),
            |latency| format!("{latency} ms"),
        );
        let color = result
            .latency_ms
            .map_or(theme.danger, |latency| latency_color(Some(latency), theme));
        let url = result.target.url.clone();
        h_flex()
            .min_h(px(54.))
            .px_4()
            .gap_4()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child(result.target.name.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(result.target.url.clone()),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(div().text_sm().text_color(color).child(value))
                    .when(custom, |this| {
                        this.child(
                            Button::new(("remove-network-target", index))
                                .icon(IconName::Delete)
                                .small()
                                .ghost()
                                .disabled(self.mutating)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.persist_network_preference(
                                        NetworkPreferenceChange::RemoveTarget(url.clone()),
                                        "自定义延迟目标已删除",
                                        cx,
                                    );
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_system_network_card(
        &self,
        config: &RuntimeConfig,
        system: &SystemNetworkSnapshot,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        setting_card("默认网络路径", theme)
            .child(info_row("接口", &system.interface, theme))
            .child(info_row("网关", &system.gateway, theme))
            .child(info_row("本地地址", &system.local_ipv4, theme))
            .child(info_row("DNS", &system.dns_servers.join(", "), theme))
            .when_some(system.error.clone(), |this, error| {
                this.child(message_banner(error, theme.warning, theme))
            })
            .child(
                h_flex()
                    .min_h(px(58.))
                    .px_4()
                    .gap_3()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "Mihomo 当前固定接口：{}",
                                empty_dash(&config.interface_name)
                            )),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("use-system-interface")
                                    .icon(IconName::Check)
                                    .label("固定为当前接口")
                                    .small()
                                    .primary()
                                    .disabled(system.interface.is_empty() || self.mutating)
                                    .on_click({
                                        let interface = system.interface.clone();
                                        cx.listener(move |this, _, _, cx| {
                                            this.apply_controlled_config(
                                                json!({"interface-name": interface}),
                                                "出口接口已固定并由 Mihomo 验证",
                                                cx,
                                            );
                                        })
                                    }),
                            )
                            .child(
                                Button::new("clear-system-interface")
                                    .icon(IconName::Redo2)
                                    .label("自动选择")
                                    .small()
                                    .outline()
                                    .disabled(config.interface_name.is_empty() || self.mutating)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.apply_controlled_config(
                                            json!({"interface-name": ""}),
                                            "出口接口已恢复自动选择",
                                            cx,
                                        );
                                    })),
                            ),
                    ),
            )
    }
}
