use super::*;

const HOME_NODE_LIMIT: usize = 4;

pub(super) struct HomeProxyProjection {
    pub(super) current: CurrentProxySummary,
    group: Option<String>,
    switchable: bool,
    nodes: Vec<HomeNodeRow>,
}

struct HomeNodeRow {
    name: String,
    delay: Option<u32>,
    current: bool,
}

pub(super) fn latency_presentation(delay: Option<u32>) -> (String, f32) {
    match delay {
        None => (zenclash_i18n::text("home.proxy.untested"), 0.),
        Some(0) => (zenclash_i18n::text("common.status.timeout"), 100.),
        Some(delay) => (
            format!("{delay} ms"),
            (delay as f32 / 1_000. * 100.).min(100.),
        ),
    }
}

impl HomeUiState {
    pub(super) fn prepare(&mut self, config: &RuntimeConfig, catalog: &ProxyCatalog) {
        let group = current_proxy_group(config, catalog);
        self.projection = Some(HomeProxyProjection {
            current: current_proxy_summary(config, catalog),
            group: group.map(|group| group.name.clone()),
            switchable: group.is_some_and(home_group_can_switch),
            nodes: group.map_or_else(Vec::new, |group| {
                group
                    .all
                    .iter()
                    .take(HOME_NODE_LIMIT)
                    .map(|id| HomeNodeRow {
                        name: id.controller_name().to_owned(),
                        delay: catalog
                            .node(id)
                            .and_then(zenclash_core::ProxyNode::latest_delay),
                        current: id.controller_name() == group.now,
                    })
                    .collect()
            }),
        });
    }
}

impl RuntimePage {
    pub(super) fn home_proxy_projection(&self) -> Option<&HomeProxyProjection> {
        (matches!(self.data, RuntimeData::Dashboard { .. })
            && self.data_runtime_version == self.home.generation)
            .then_some(self.home.projection.as_ref())
            .flatten()
    }

    pub(in crate::pages::runtime) fn reconcile_home_generation(&mut self) -> bool {
        let generation = self.core_session.generation();
        if self.home.generation == generation {
            return false;
        }
        self.release_home_presentation();
        self.home.generation = generation;
        true
    }

    pub(in crate::pages::runtime) fn prepare_home_projection(&mut self) {
        self.reconcile_home_generation();
        if self.data_runtime_version != self.home.generation {
            self.home.projection = None;
            return;
        }
        if let RuntimeData::Dashboard { config, proxies } = &self.data
            && let (Some(config), Some(proxies)) = (config.value(), proxies.value())
        {
            self.home.prepare(config, proxies);
            self.observe_home_traffic();
        } else {
            self.home.projection = None;
        }
    }

    pub(super) fn render_home_metrics(
        &self,
        streams: &StreamStatuses,
        theme: &gpui_kit::component::Theme,
    ) -> gpui_kit::Div {
        let traffic = self.traffic_monitor.snapshot();
        let generation = self.home.generation;
        let connections = streams
            .connections
            .value()
            .filter(|value| value.generation == generation);
        let traffic_status = streams
            .traffic
            .value()
            .filter(|value| value.generation == generation);

        let metrics = [
            (
                "home.traffic.current_download",
                traffic_status
                    .filter(|_| traffic.generation == generation)
                    .map_or_else(|| "—".into(), |_| format_speed(traffic.download)),
                IconName::ArrowDown,
            ),
            (
                "home.traffic.current_upload",
                traffic_status
                    .filter(|_| traffic.generation == generation)
                    .map_or_else(|| "—".into(), |_| format_speed(traffic.upload)),
                IconName::ArrowUp,
            ),
            (
                "home.traffic.active_connections",
                connections.map_or_else(|| "—".into(), |value| value.item_count.to_string()),
                IconName::Network,
            ),
            (
                "home.traffic.core_memory",
                connections.map_or_else(|| "—".into(), |value| format_bytes(value.memory)),
                IconName::Cpu,
            ),
        ];
        h_flex()
            .flex_wrap()
            .gap_3()
            .children(
                metrics
                    .into_iter()
                    .enumerate()
                    .map(|(index, (label, value, icon))| {
                        let points = self.home.chart.sparkline(index, self.home.generation);
                        v_flex()
                            .flex_1()
                            .flex_basis(rems(13.))
                            .min_w_0()
                            .gap_2()
                            .p_4()
                            .rounded(theme.radius_lg)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.secondary)
                            .child(
                                h_flex()
                                    .gap_2()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(Icon::new(icon).size_4())
                                    .child(zenclash_i18n::text(label)),
                            )
                            .child(
                                h_flex()
                                    .gap_3()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_lg()
                                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                            .truncate()
                                            .child(value),
                                    )
                                    .child(
                                        div().w_16().h_8().child(
                                            AreaChart::new(points)
                                                .id(("home-metric-sparkline", index))
                                                .x(|point| point.0.clone())
                                                .y(|point| point.1)
                                                .stroke(theme.info)
                                                .fill(theme.info.opacity(0.15))
                                                .natural()
                                                .x_axis(false)
                                                .y_axis(false)
                                                .grid(false)
                                                .interactive(false),
                                        ),
                                    ),
                            )
                    }),
            )
    }

    pub(super) fn render_home_node_delays(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let mut content = v_flex().p_4().gap_3();
        if let Some(projection) = self.home_proxy_projection() {
            for (index, node) in projection.nodes.iter().enumerate() {
                let (delay, latency_fraction) = latency_presentation(node.delay);
                let latency_color = if node.delay == Some(0) {
                    theme.danger
                } else {
                    theme.info
                };
                let group = projection.group.clone();
                let name = node.name.clone();
                let switch = Button::new(("home-select-node", index))
                    .small()
                    .outline()
                    .label(zenclash_i18n::text(if node.current {
                        "proxies.actions.current"
                    } else {
                        "proxies.actions.select"
                    }))
                    .tooltip(zenclash_i18n::text_with(
                        "home.proxy.switch_current",
                        &[("name", name.clone())],
                    ))
                    .disabled(
                        self.core_busy()
                            || node.current
                            || !projection.switchable
                            || self.home.proxy_switching.is_some()
                            || self.home.profile_switching.is_some(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(group) = &group {
                            this.change_home_proxy(group.clone(), name.clone(), cx);
                        }
                    }));
                content = content.child(
                    h_flex()
                        .gap_3()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .truncate()
                                .child(node.name.clone()),
                        )
                        .child(
                            div().w_24().child(
                                Progress::new(("home-node-delay", index))
                                    .h_2()
                                    .color(latency_color)
                                    .value(latency_fraction),
                            ),
                        )
                        .child(
                            div()
                                .w_16()
                                .text_xs()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(latency_color)
                                .child(delay),
                        )
                        .child(switch),
                );
            }
            if projection.nodes.is_empty() {
                content = content.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(zenclash_i18n::text("home.proxy.no_node")),
                );
            }
        } else {
            content = content.child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(zenclash_i18n::text("home.proxy.no_node")),
            );
        }
        content = content.child(
            h_flex().justify_end().child(
                Button::new("home-more-nodes")
                    .small()
                    .ghost()
                    .icon(IconName::ArrowRight)
                    .label(zenclash_i18n::text("home.proxy.details"))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(NavigateProxies), cx)
                    }),
            ),
        );
        home_card(
            zenclash_i18n::text("proxies.design.latency_comparison"),
            theme,
        )
        .flex_basis(rems(19.))
        .min_w_0()
        .child(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenclash_core::{DelayHistory, ProxyNode};

    #[test]
    fn latency_timeout_is_distinct_from_unknown_and_success() {
        let timeout = latency_presentation(Some(0));
        assert_eq!(timeout.0, zenclash_i18n::text("common.status.timeout"));
        assert_eq!(timeout.1, 100.);
        assert_ne!(timeout, latency_presentation(None));
        assert_eq!(latency_presentation(Some(28)).0, "28 ms");
    }

    #[test]
    fn home_projection_keeps_late_current_node_and_limits_visible_candidates() {
        let nodes = (0..20)
            .map(|index| ProxyNode {
                name: format!("node-{index}"),
                history: vec![DelayHistory {
                    time: String::new(),
                    delay: 20 + index,
                    mean_delay: 0,
                }],
                ..ProxyNode::default()
            })
            .collect();
        let catalog = ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup {
                    name: "primary".into(),
                    now: "node-19".into(),
                    behavior: ProxyGroupBehavior::Selector,
                    ..ProxyGroup::default()
                },
                nodes,
            )],
            20,
        );
        let mut home = HomeUiState::default();
        home.prepare(
            &RuntimeConfig {
                mode: "rule".into(),
                ..RuntimeConfig::default()
            },
            &catalog,
        );
        let projection = home.projection.unwrap();
        assert_eq!(projection.current.node, "node-19");
        assert_eq!(projection.current.delay, Some(39));
        assert_eq!(projection.nodes.len(), 4);
        assert!(projection.switchable);
        assert!(projection.nodes.iter().all(|node| !node.current));
    }

    #[test]
    fn direct_and_load_balance_do_not_offer_manual_switches() {
        let catalog = ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup {
                    name: "primary".into(),
                    behavior: ProxyGroupBehavior::LoadBalance,
                    ..ProxyGroup::default()
                },
                vec![ProxyNode {
                    name: "node".into(),
                    ..ProxyNode::default()
                }],
            )],
            1,
        );
        let mut home = HomeUiState::default();
        home.prepare(
            &RuntimeConfig {
                mode: "rule".into(),
                ..RuntimeConfig::default()
            },
            &catalog,
        );
        assert!(!home.projection.as_ref().unwrap().switchable);
        home.prepare(
            &RuntimeConfig {
                mode: "direct".into(),
                ..RuntimeConfig::default()
            },
            &catalog,
        );
        let projection = home.projection.unwrap();
        assert!(projection.nodes.is_empty());
        assert!(!projection.switchable);
        assert_eq!(projection.current.node, "DIRECT");
    }
}
