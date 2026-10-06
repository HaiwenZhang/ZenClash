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
                    .find(|id| id.controller_name() == group.now)
                    .into_iter()
                    .chain(
                        group
                            .all
                            .iter()
                            .filter(|id| id.controller_name() != group.now),
                    )
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
        let traffic = self.home_traffic_snapshot();
        let generation = self.home.generation;
        let traffic_status = streams
            .traffic
            .value()
            .filter(|value| value.generation == generation);

        let metrics = [
            (
                1,
                "home.traffic.current_upload",
                traffic_status
                    .filter(|_| traffic.generation == generation)
                    .map_or_else(|| "—".into(), |_| format_speed(traffic.upload)),
                IconName::ArrowUp,
                theme.chart_2,
            ),
            (
                0,
                "home.traffic.current_download",
                traffic_status
                    .filter(|_| traffic.generation == generation)
                    .map_or_else(|| "—".into(), |_| format_speed(traffic.download)),
                IconName::ArrowDown,
                theme.chart_1,
            ),
        ];
        h_flex()
            .items_stretch()
            .flex_wrap()
            .gap_3()
            .children(
                metrics
                    .into_iter()
                    .map(|(index, label, value, icon, color)| {
                        let points = self.home.chart.sparkline(index, self.home.generation);
                        let peak = points.iter().map(|point| point.1).fold(0_f64, f64::max);
                        let peak_label = if points.is_empty() {
                            "—".into()
                        } else {
                            format_speed(peak as u64)
                        };
                        v_flex()
                            .id(("home-speed-card", index))
                            .test_support()
                            .flex_1()
                            .flex_basis(rems(13.))
                            .min_w_0()
                            .min_h(rems(11.5))
                            .gap_0()
                            .p_4()
                            .rounded(theme.radius_lg)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.secondary)
                            .child(
                                h_flex()
                                    .gap_2()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(
                                        h_flex()
                                            .size_6()
                                            .justify_center()
                                            .rounded(theme.radius)
                                            .border_1()
                                            .border_color(color.opacity(0.3))
                                            .child(Icon::new(icon).size_4().text_color(color)),
                                    )
                                    .child(zenclash_i18n::text(label))
                                    .child(div().flex_1())
                                    .when(
                                        traffic_status.is_some() && streams.traffic.is_fresh(),
                                        |row| {
                                            row.child(status_label(
                                                zenclash_i18n::text("home.traffic.live"),
                                                theme.chart_3,
                                                theme,
                                            ))
                                        },
                                    ),
                            )
                            .child(
                                h_flex().gap_3().child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_size(px(32.))
                                        .line_height(gpui_kit::relative(1.25))
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .truncate()
                                        .child(value),
                                ),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(zenclash_i18n::text("home.traffic.realtime_speed")),
                            )
                            .child(
                                div().w_full().h(rems(3.)).child(
                                    AreaChart::new(points)
                                        .id(("home-metric-sparkline", index))
                                        .x(|point| point.0.clone())
                                        .y(|point| point.1)
                                        .stroke(color)
                                        .fill(color.opacity(0.15))
                                        .y_domain(0., peak.max(1.))
                                        .natural()
                                        .x_axis(false)
                                        .y_axis(false)
                                        .grid(false)
                                        .interactive(false),
                                ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_right()
                                    .text_color(theme.muted_foreground)
                                    .child(zenclash_i18n::text_with(
                                        "home.traffic.peak",
                                        &[("value", peak_label)],
                                    )),
                            )
                    }),
            )
    }

    pub(super) fn render_home_node_delays(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let mut content = v_flex().px_4().pb_3().gap_0p5();
        if let Some(projection) = self.home_proxy_projection() {
            for (index, node) in projection.nodes.iter().enumerate() {
                let (delay, _) = latency_presentation(node.delay);
                let latency_color = if node.delay == Some(0) {
                    theme.danger
                } else if node.current {
                    theme.primary
                } else {
                    theme.info
                };
                let group = projection.group.clone();
                let name = node.name.clone();
                let switch = Button::new(("home-select-node", index))
                    .small()
                    .w(gpui_kit::rems(6.))
                    .outline()
                    .label(zenclash_i18n::text("home.proxy.switch"))
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
                        .px_2()
                        .py_0p5()
                        .rounded(theme.radius)
                        .when(node.current, |row| row.bg(theme.list_active))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .truncate()
                                .child(node.name.clone()),
                        )
                        .child(
                            div()
                                .w_16()
                                .text_sm()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(latency_color)
                                .child(delay),
                        )
                        .child(if node.current {
                            h_flex()
                                .w(gpui_kit::rems(6.))
                                .h_6()
                                .gap_2()
                                .justify_center()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.chart_3.opacity(0.12))
                                .text_color(theme.primary)
                                .text_xs()
                                .child(Icon::new(IconName::Check).size_4())
                                .child(zenclash_i18n::text("home.proxy.current"))
                                .into_any_element()
                        } else {
                            switch.into_any_element()
                        }),
                );
            }
        }
        content.min_w_0()
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
        assert_eq!(projection.nodes[0].name, "node-19");
        assert!(projection.nodes[0].current);
        assert_eq!(projection.nodes[0].delay, Some(39));
        assert_eq!(
            projection.nodes.iter().filter(|node| node.current).count(),
            1
        );
        assert_eq!(projection.nodes[1].name, "node-0");
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
