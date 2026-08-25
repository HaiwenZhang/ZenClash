use super::{
    div, empty_state, h_flex, metric, px, v_flex, FluentBuilder, InteractiveElement, IntoElement,
    ParentElement, RuntimePage, Styled,
};

impl RuntimePage {
    pub(super) fn render_logs(&self, theme: &gpui_component::Theme) -> gpui::AnyElement {
        let entries = self.log_monitor.entries();
        let connected = self.log_monitor.connected();
        let visible = entries.len().min(600);
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .justify_between()
                    .child(metric(
                        "日志条目",
                        entries.len().to_string(),
                        theme.primary,
                        theme,
                    ))
                    .child(
                        h_flex()
                            .gap_2()
                            .text_xs()
                            .text_color(if connected {
                                theme.success
                            } else {
                                theme.danger
                            })
                            .child(div().size_2().rounded_full().bg(if connected {
                                theme.success
                            } else {
                                theme.danger
                            }))
                            .child(if connected {
                                "实时流已连接"
                            } else {
                                "正在重连"
                            }),
                    ),
            )
            .child(
                v_flex()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .when(entries.is_empty(), |this| {
                        this.child(empty_state("等待 Mihomo 日志事件…", theme))
                    })
                    .children(entries.into_iter().rev().take(visible).enumerate().map(
                        |(index, entry)| {
                            let color = match entry.level.as_str() {
                                "error" => theme.danger,
                                "warning" | "warn" => theme.warning,
                                "debug" => theme.muted_foreground,
                                _ => theme.success,
                            };
                            h_flex()
                                .id(("log-row", index))
                                .items_start()
                                .gap_3()
                                .px_4()
                                .py_2()
                                .border_b_1()
                                .border_color(theme.border)
                                .child(
                                    div()
                                        .w(px(62.))
                                        .text_xs()
                                        .text_color(color)
                                        .child(entry.level.to_uppercase()),
                                )
                                .child(div().flex_1().text_xs().child(entry.payload))
                        },
                    )),
            )
            .into_any_element()
    }
}
