use zenclash_core::{TrafficDimension, TrafficTrendPoint};

use super::{dimension_label, TrafficRange};
use crate::pages::runtime::{
    div, empty_state, format_bytes, format_speed, h_flex, metric, normalized_fraction, px, v_flex,
    Button, ButtonVariants, ConnectionsSnapshot, Context, Disableable, FluentBuilder, IconName,
    InteractiveElement, IntoElement, ParentElement, RuntimeData, RuntimePage, Selectable, Sizable,
    StatefulInteractiveElement, Styled,
};

impl RuntimePage {
    pub(in crate::pages::runtime) fn render_traffic(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let realtime = self.traffic_monitor.snapshot();
        let connections = match &self.data {
            RuntimeData::Connections(data) => data.clone(),
            _ => ConnectionsSnapshot::default(),
        };
        let history = &self.traffic_history;
        let maximum = history
            .overview
            .trend
            .iter()
            .map(|point| point.upload.saturating_add(point.download))
            .max()
            .unwrap_or(1)
            .max(1);

        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(metric(
                        "实时上传",
                        format_speed(realtime.upload),
                        theme.success,
                        theme,
                    ))
                    .child(metric(
                        "实时下载",
                        format_speed(realtime.download),
                        theme.primary,
                        theme,
                    ))
                    .child(metric(
                        "区间用量",
                        format_bytes(history.overview.totals.total),
                        theme.warning,
                        theme,
                    ))
                    .child(metric(
                        "活动连接",
                        connections.connections.len().to_string(),
                        theme.foreground,
                        theme,
                    )),
            )
            .child(self.render_traffic_controls(theme, cx))
            .child(self.render_traffic_signal(maximum, theme))
            .child(
                h_flex()
                    .items_start()
                    .gap_4()
                    .flex_wrap()
                    .child(self.render_traffic_rankings(theme, cx))
                    .child(self.render_traffic_inspector(theme, cx)),
            )
            .into_any_element()
    }

    fn render_traffic_signal(
        &self,
        maximum: u64,
        theme: &gpui_component::Theme,
    ) -> gpui::AnyElement {
        let history = &self.traffic_history;
        v_flex()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                h_flex()
                    .px_4()
                    .pt_4()
                    .justify_between()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("流量信号带"),
                            )
                            .child(div().text_xs().text_color(theme.muted_foreground).child(
                                format!(
                                    "{} · 上传 {} · 下载 {}",
                                    history.range.label(),
                                    format_bytes(history.overview.totals.upload),
                                    format_bytes(history.overview.totals.download)
                                ),
                            )),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .text_xs()
                            .child(h_flex().gap_1().text_color(theme.success).child("■ 上传"))
                            .child(h_flex().gap_1().text_color(theme.primary).child("■ 下载")),
                    ),
            )
            .child(render_trend(&history.overview.trend, maximum, theme))
            .into_any_element()
    }

    fn render_traffic_controls(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let history = &self.traffic_history;
        h_flex()
            .gap_3()
            .flex_wrap()
            .justify_between()
            .p_3()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                h_flex()
                    .gap_2()
                    .children(
                        TrafficRange::ALL
                            .into_iter()
                            .enumerate()
                            .map(|(index, range)| {
                                Button::new(("traffic-range", index))
                                    .label(range.label())
                                    .small()
                                    .outline()
                                    .selected(history.range == range)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_traffic_range(range, cx);
                                    }))
                            }),
                    ),
            )
            .child(
                h_flex().gap_2().children(
                    [
                        (TrafficDimension::Host, "域名"),
                        (TrafficDimension::SourceIp, "设备"),
                        (TrafficDimension::Outbound, "出口"),
                        (TrafficDimension::Process, "进程"),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(index, (dimension, label))| {
                        Button::new(("traffic-dimension", index))
                            .label(label)
                            .small()
                            .outline()
                            .selected(history.dimension == dimension)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_traffic_dimension(dimension, cx);
                            }))
                    }),
                ),
            )
            .child(self.render_clear_history_control(cx))
    }

    fn render_clear_history_control(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        h_flex()
            .gap_2()
            .when(self.traffic_history.clear_confirmation, |this| {
                this.child(
                    Button::new("cancel-clear-traffic")
                        .label("取消")
                        .small()
                        .ghost()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.cancel_clear_traffic_history(cx);
                        })),
                )
                .child(
                    Button::new("confirm-clear-traffic")
                        .icon(IconName::Delete)
                        .label("确认清空")
                        .small()
                        .danger()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.clear_traffic_history(cx);
                        })),
                )
            })
            .when(!self.traffic_history.clear_confirmation, |this| {
                this.child(
                    Button::new("request-clear-traffic")
                        .icon(IconName::Delete)
                        .label("清空历史")
                        .small()
                        .ghost()
                        .disabled(self.mutating)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.request_clear_traffic_history(cx);
                        })),
                )
            })
            .into_any_element()
    }

    fn render_traffic_rankings(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let history = &self.traffic_history;
        v_flex()
            .min_w(px(360.))
            .flex_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                h_flex()
                    .px_4()
                    .py_3()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(format!("{}排名", dimension_label(history.dimension))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{} 条样本", history.overview.totals.samples)),
                    ),
            )
            .when(history.overview.rankings.is_empty(), |this| {
                this.child(empty_state(
                    if self.preferences.traffic_history_enabled {
                        "尚无历史用量；保持内核运行后会自动记录"
                    } else {
                        "流量历史记录已在应用设置中关闭"
                    },
                    theme,
                ))
            })
            .children(
                history
                    .overview
                    .rankings
                    .iter()
                    .take(20)
                    .enumerate()
                    .map(|(index, item)| {
                        let label = item.label.clone();
                        let selected = history.selected_parent.as_deref() == Some(&item.label);
                        h_flex()
                            .id(("traffic-ranking", index))
                            .min_h(px(44.))
                            .px_4()
                            .gap_3()
                            .border_b_1()
                            .border_color(theme.border)
                            .when(selected, |this| this.bg(theme.primary.opacity(0.1)))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_traffic_parent(label.clone(), cx);
                            }))
                            .child(
                                div()
                                    .w(px(24.))
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{:02}", index + 1)),
                            )
                            .child(div().flex_1().min_w_0().text_sm().child(item.label.clone()))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(format_bytes(item.total)),
                            )
                            .child(div().text_color(theme.muted_foreground).child("›"))
                    }),
            )
    }

    fn render_traffic_inspector(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let history = &self.traffic_history;
        v_flex()
            .min_w(px(320.))
            .flex_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                v_flex()
                    .px_4()
                    .py_3()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("流向检查器"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("选择排名项，再查看关联目标与实际出口"),
                    ),
            )
            .when(history.selected_parent.is_none(), |this| {
                this.child(empty_state("从左侧排名选择一项开始下钻", theme))
            })
            .when_some(history.selected_parent.clone(), |this, parent| {
                this.child(
                    div()
                        .px_4()
                        .py_3()
                        .text_sm()
                        .text_color(theme.primary)
                        .child(parent),
                )
                .children(self.render_traffic_details(theme, cx))
                .when_some(history.selected_detail.clone(), |this, detail| {
                    this.child(
                        v_flex()
                            .mt_2()
                            .px_4()
                            .py_3()
                            .gap_2()
                            .border_t_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{detail} · 实际出口")),
                            )
                            .children(history.proxy_stats.iter().map(|item| {
                                h_flex()
                                    .justify_between()
                                    .child(div().text_sm().child(item.label.clone()))
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(format_bytes(item.total)),
                                    )
                            })),
                    )
                })
            })
    }

    fn render_traffic_details(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        self.traffic_history
            .details
            .iter()
            .take(12)
            .enumerate()
            .map(|(index, item)| {
                let label = item.label.clone();
                let selected = self.traffic_history.selected_detail.as_deref() == Some(&item.label);
                h_flex()
                    .id(("traffic-detail", index))
                    .min_h(px(40.))
                    .px_4()
                    .gap_3()
                    .border_t_1()
                    .border_color(theme.border)
                    .when(selected, |this| this.bg(theme.primary.opacity(0.1)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_traffic_detail(label.clone(), cx);
                    }))
                    .child(div().flex_1().min_w_0().text_sm().child(item.label.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format_bytes(item.total)),
                    )
                    .into_any_element()
            })
            .collect()
    }
}

fn render_trend(
    points: &[TrafficTrendPoint],
    maximum: u64,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    if points.is_empty() {
        return empty_state("这个时间范围还没有流量样本", theme).into_any_element();
    }
    h_flex()
        .h(px(190.))
        .items_end()
        .gap_1()
        .p_4()
        .children(points.iter().enumerate().map(|(index, point)| {
            let upload_height = 154.0 * normalized_fraction(point.upload, maximum);
            let download_height = 154.0 * normalized_fraction(point.download, maximum);
            v_flex()
                .id(("history-signal", index))
                .flex_1()
                .h_full()
                .justify_end()
                .gap_0()
                .child(
                    div()
                        .w_full()
                        .h(px(download_height.max(1.)))
                        .rounded_t_sm()
                        .bg(theme.primary.opacity(0.78)),
                )
                .child(
                    div()
                        .w_full()
                        .h(px(upload_height.max(1.)))
                        .rounded_b_sm()
                        .bg(theme.success.opacity(0.78)),
                )
        }))
        .into_any_element()
}
