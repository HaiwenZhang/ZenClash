use super::{
    div, format_bytes, format_speed, h_flex, metric, normalized_fraction, px, v_flex,
    ConnectionsSnapshot, InteractiveElement, IntoElement, ParentElement, RuntimeData, RuntimePage,
    Styled,
};

impl RuntimePage {
    pub(super) fn render_traffic(&self, theme: &gpui_component::Theme) -> gpui::AnyElement {
        let realtime = self.traffic_monitor.snapshot();
        let connections = match &self.data {
            RuntimeData::Connections(data) => data.clone(),
            _ => ConnectionsSnapshot::default(),
        };
        let maximum = self
            .traffic_samples
            .iter()
            .copied()
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
                        "累计上传",
                        format_bytes(connections.upload_total),
                        theme.success,
                        theme,
                    ))
                    .child(metric(
                        "累计下载",
                        format_bytes(connections.download_total),
                        theme.primary,
                        theme,
                    )),
            )
            .child(
                v_flex()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .child(
                        div()
                            .px_4()
                            .pt_4()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("最近 24 秒实时吞吐"),
                    )
                    .child(
                        h_flex().h(px(180.)).items_end().gap_1().p_4().children(
                            self.traffic_samples
                                .iter()
                                .enumerate()
                                .map(|(index, value)| {
                                    div()
                                        .id(("traffic-bar", index))
                                        .flex_1()
                                        .h(px(144.0f32
                                            .mul_add(normalized_fraction(*value, maximum), 4.)))
                                        .rounded_sm()
                                        .bg(theme.primary.opacity(0.7))
                                }),
                        ),
                    ),
            )
            .into_any_element()
    }
}
