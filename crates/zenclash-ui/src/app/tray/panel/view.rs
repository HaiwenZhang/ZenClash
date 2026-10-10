use super::*;
use gpui_kit::StatefulInteractiveElement;
use gpui_kit::component::scroll::ScrollableElement;

impl StatusPanel {
    fn render_traffic(&self, theme: &gpui_kit::component::Theme) -> gpui_kit::AnyElement {
        let points = self.snapshot.points.clone();
        let end = points.last().map_or(0, |point| point.at_ms);
        let start = points.first().map_or(end, |point| point.at_ms);
        let span = (end.saturating_sub(start) as f32 / 120_000.).clamp(1. / 120., 1.);
        let peak = points
            .iter()
            .map(|point| point.upload.max(point.download))
            .fold(1024_f64, f64::max);
        let ceiling = (peak.min(u64::MAX as f64 / 4.) as u64)
            .saturating_add((peak * 0.1) as u64)
            .checked_next_power_of_two()
            .unwrap_or(u64::MAX) as f64;
        let chart = AreaChart::new(points)
            .id("panel-traffic-series")
            .x(move |point| point.label(end))
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
            .y_padding(0., 0.)
            .x_axis(false)
            .y_axis(false)
            .grid(false)
            .interactive(false);
        let traffic = &self.snapshot.traffic;
        let current = traffic.connected && traffic.generation == self.snapshot.generation;
        v_flex()
            .id("panel-traffic")
            .test_support()
            .gap_2()
            .p_3()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(zenclash_i18n::text("home.traffic.title")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("home.traffic.range_120")),
                    ),
            )
            .child(
                h_flex().gap_4().children(
                    [
                        (
                            "home.traffic.download",
                            "↓",
                            traffic.download,
                            theme.chart_1,
                        ),
                        ("home.traffic.upload", "↑", traffic.upload, theme.chart_2),
                    ]
                    .map(|(label, arrow, rate, color)| {
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(div().text_color(color).child(arrow))
                                    .child(zenclash_i18n::text(label)),
                            )
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .child(if current {
                                        format_speed(rate)
                                    } else {
                                        "—".to_owned()
                                    }),
                            )
                    }),
                ),
            )
            .child(
                div()
                    .id("panel-traffic-plot")
                    .relative()
                    .w_full()
                    .h(rems(5.))
                    .child(
                        AreaChart::new(vec![(0., 0.), (1., 0.)])
                            .id("panel-traffic-grid")
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
                            .top_0()
                            .right_0()
                            .h_full()
                            .w(gpui_kit::relative(span))
                            .child(chart),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("−2m")
                    .child("−1m")
                    .child(zenclash_i18n::text("home.traffic.now")),
            )
            .into_any_element()
    }

    fn render_controls(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let theme = cx.theme();
        let mode = self.snapshot.mode;
        let pending = self.snapshot.mode_pending;
        let unavailable = self.snapshot.unavailable;
        let mut controls = v_flex()
            .gap_3()
            .p_3()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                h_flex()
                    .id("panel-mode")
                    .gap_0()
                    .p_0p5()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.muted)
                    .children(
                        [
                            (
                                "panel-mode-rule",
                                OutboundMode::Rule,
                                TrayCommand::SetRuleMode,
                            ),
                            (
                                "panel-mode-global",
                                OutboundMode::Global,
                                TrayCommand::SetGlobalMode,
                            ),
                            (
                                "panel-mode-direct",
                                OutboundMode::Direct,
                                TrayCommand::SetDirectMode,
                            ),
                        ]
                        .map(|(id, value, command)| {
                            let selected = mode == value;
                            Button::new(id)
                                .small()
                                .flex_1()
                                .label(value.label())
                                .custom(
                                    ButtonCustomVariant::new(cx)
                                        .color(if selected {
                                            theme.list_active
                                        } else {
                                            theme.transparent
                                        })
                                        .foreground(if selected {
                                            theme.primary
                                        } else {
                                            theme.muted_foreground
                                        })
                                        .hover(theme.list_active)
                                        .active(theme.list_active),
                                )
                                .selected(selected)
                                .loading(selected && pending)
                                .disabled(unavailable || pending)
                                .on_click(self.command(command))
                        }),
                    ),
            );
        for (id, label, checked, command, cannot_enable) in [
            (
                "panel-system-proxy",
                zenclash_i18n::text("tray.system_proxy"),
                self.snapshot.state.system_proxy,
                TrayCommand::SetSystemProxy {
                    enabled: !self.snapshot.state.system_proxy,
                    port: self.snapshot.state.mixed_port,
                },
                unavailable || self.snapshot.state.mixed_port == 0,
            ),
            (
                "panel-tun",
                "TUN".to_owned(),
                self.snapshot.state.tun,
                TrayCommand::SetTun(!self.snapshot.state.tun),
                unavailable,
            ),
        ] {
            let owner = self.owner.clone();
            controls = controls.child(
                h_flex()
                    .justify_between()
                    .gap_3()
                    .child(label.clone())
                    .child(
                        MintSwitch::new(id)
                            .accessibility_label(label)
                            .checked(checked)
                            .disabled(self.snapshot.capture_pending || (cannot_enable && !checked))
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |app, cx| {
                                    app.handle_tray_command(command.clone(), cx)
                                });
                            }),
                    ),
            );
        }
        if self.snapshot.capture_pending {
            controls = controls.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(zenclash_i18n::text("home.controls.capture_switching")),
            );
        }
        controls.into_any_element()
    }

    fn render_current_node(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let theme = cx.theme();
        let node = if self.snapshot.unavailable {
            "—".to_owned()
        } else {
            self.snapshot
                .current_node
                .clone()
                .unwrap_or_else(|| "—".to_owned())
        };
        v_flex()
            .id("panel-current-node")
            .test_support()
            .min_w_0()
            .p_3()
            .gap_2()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(zenclash_i18n::text("home.proxy.title")),
            )
            .child(
                div()
                    .id("panel-current-node-name")
                    .test_support()
                    .role(gpui_kit::Role::Status)
                    .aria_label(node.clone())
                    .text_lg()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(node),
            )
            .into_any_element()
    }
}

impl Render for StatusPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mut panel = v_flex()
            .id("status-panel")
            .track_focus(&self.focus)
            .key_context("ZenClashStatusPanel")
            .w_full()
            .min_h(gpui_kit::relative(1.))
            .flex_shrink_0()
            .p_3()
            .gap_3()
            .bg(theme.background)
            .text_color(theme.foreground)
            .text_sm()
            .on_action(|_: &crate::app::CloseStatusPanel, window, cx| {
                window.remove_window();
                cx.stop_propagation();
            })
            .child(self.render_traffic(theme))
            .child(self.render_controls(cx));
        if let Some(error) = self.snapshot.error.as_ref() {
            panel = panel.child(
                div()
                    .id("panel-error")
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        } else if !self.snapshot.traffic.connected {
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(zenclash_i18n::text("tray.core_offline")),
            );
        }
        div()
            .id("status-panel-content-scroll")
            .size_full()
            .overflow_y_scrollbar()
            .child(panel.child(self.render_current_node(cx)))
            .into_any_element()
    }
}
