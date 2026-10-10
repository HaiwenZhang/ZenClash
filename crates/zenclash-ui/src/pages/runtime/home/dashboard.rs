use super::selector::HomeProxyProjection;
use super::*;

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

impl RuntimePage {
    pub(super) fn home_proxy_projection(&self) -> Option<&HomeProxyProjection> {
        (matches!(self.data, RuntimeData::Dashboard { .. })
            && self.data_runtime_version == self.home.generation)
            .then_some(self.home.projection.as_ref())
            .flatten()
    }

    pub(in crate::pages::runtime) fn reconcile_home_generation(&mut self) -> bool {
        let generation = self.core_session.generation();
        if self
            .home
            .capture_transition
            .as_ref()
            .is_some_and(|transition| !transition.pending && transition.generation != generation)
        {
            self.home.capture_transition = None;
        }
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
    ) -> gpui_kit::AnyElement {
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
            .id("home-speed-row")
            .test_support()
            .px_4()
            .pb_4()
            .gap_6()
            .flex_wrap()
            .children(
                metrics
                    .into_iter()
                    .map(|(index, label, value, icon, color)| {
                        let points = self.home.chart.sparkline(index, generation);
                        let peak = points.iter().map(|point| point.1).fold(0_f64, f64::max);
                        h_flex()
                            .id(("home-speed-card", index))
                            .test_support()
                            .gap_2()
                            .when(index == 0, |row| {
                                row.pl_6().border_l_1().border_color(theme.border)
                            })
                            .child(Icon::new(icon).size_5().text_color(color))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(zenclash_i18n::text(label)),
                            )
                            .child(
                                div()
                                    .text_base()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .child(value),
                            )
                            .child(
                                div().w(rems(8.)).h(rems(1.5)).child(
                                    AreaChart::new(points)
                                        .id(("home-metric-sparkline", index))
                                        .x(|point| point.0.clone())
                                        .y(|point| point.1)
                                        .stroke(color)
                                        .fill(color.opacity(0.10))
                                        .y_domain(0., peak.max(1.))
                                        .natural()
                                        .x_axis(false)
                                        .y_axis(false)
                                        .grid(false)
                                        .interactive(false),
                                ),
                            )
                    }),
            )
            .when(
                traffic_status.is_some() && streams.traffic.is_fresh(),
                |row| {
                    row.child(status_label(
                        zenclash_i18n::text("home.traffic.live"),
                        theme.chart_3,
                        theme,
                    ))
                },
            )
            .into_any_element()
    }
}
