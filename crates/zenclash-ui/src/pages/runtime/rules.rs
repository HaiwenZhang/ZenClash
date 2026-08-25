use super::{
    div, h_flex, metric, px, v_flex, InteractiveElement, IntoElement, ParentElement, RuntimeData,
    RuntimePage, Styled,
};

impl RuntimePage {
    pub(super) fn render_rules(&self, theme: &gpui_component::Theme) -> gpui::AnyElement {
        let rules = match &self.data {
            RuntimeData::Rules(data) => data.rules.clone(),
            _ => Vec::new(),
        };
        let visible = rules.len().min(800);
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .justify_between()
                    .child(metric(
                        "运行时规则",
                        rules.len().to_string(),
                        theme.primary,
                        theme,
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("显示前 {visible} 条")),
                    ),
            )
            .child(
                v_flex()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .children(
                        rules
                            .into_iter()
                            .take(visible)
                            .enumerate()
                            .map(|(index, rule)| {
                                h_flex()
                                    .id(("rule-row", index))
                                    .min_h(px(42.))
                                    .px_4()
                                    .gap_3()
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .child(
                                        div()
                                            .w(px(116.))
                                            .text_xs()
                                            .text_color(theme.primary)
                                            .child(rule.kind),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_sm()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .overflow_hidden()
                                            .child(rule.payload),
                                    )
                                    .child(
                                        div()
                                            .w(px(190.))
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .overflow_hidden()
                                            .child(rule.proxy),
                                    )
                            }),
                    ),
            )
            .into_any_element()
    }
}
