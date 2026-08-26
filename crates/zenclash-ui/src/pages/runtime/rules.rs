use super::{
    div, empty_state, h_flex, message_banner, px, v_flex, Context, Disableable, FluentBuilder,
    Input, InteractiveElement, IntoElement, Page, ParentElement, RuntimeData, RuntimePage, Sizable,
    Styled, Switch,
};

const MAX_VISIBLE_RULES: usize = 800;

impl RuntimePage {
    fn set_rule_enabled(&mut self, index: usize, enabled: bool, cx: &mut Context<Self>) {
        if !self.core_kind.capabilities().rule_toggle {
            self.error = Some(format!(
                "{} 暂不支持单条规则启停",
                self.core_kind.display_name()
            ));
            cx.notify();
            return;
        }
        let Some(token) = self.begin_mutation(Page::Rules) else {
            return;
        };
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            client
                .set_rule_disabled(index, !enabled)
                .await
                .map(RuntimeData::Rules)
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("规则状态任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data) {
                            this.notice = Some(if enabled {
                                format!("规则 #{index} 已启用并通过状态回读")
                            } else {
                                format!("规则 #{index} 已禁用并通过状态回读")
                            });
                        }
                    }
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn render_rules(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let rules = match &self.data {
            RuntimeData::Rules(data) => data.rules.as_slice(),
            _ => &[],
        };
        let query = normalize_rule_query(&self.rule_filter.read(cx).value());
        let filtered = rules
            .iter()
            .filter(|rule| rule_matches(rule, &query))
            .take(MAX_VISIBLE_RULES)
            .collect::<Vec<_>>();
        let filtered_count = filtered.len();
        v_flex()
            .gap_3()
            .when(!self.core_kind.capabilities().rule_toggle, |this| {
                this.child(message_banner(
                    format!(
                        "{} 支持规则查看与匹配，但暂不提供单条规则启停和命中统计。",
                        self.core_kind.display_name()
                    ),
                    theme.warning,
                    theme,
                ))
            })
            .child(
                h_flex()
                    .min_h(px(64.))
                    .px_4()
                    .gap_4()
                    .justify_between()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .child(
                        h_flex()
                            .gap_3()
                            .child(div().text_sm().text_color(theme.muted_foreground).child(
                                if query.is_empty() {
                                    "运行时规则"
                                } else {
                                    "过滤结果"
                                },
                            ))
                            .child(
                                div()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_lg()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(theme.primary)
                                    .child(if query.is_empty() {
                                        rules.len().to_string()
                                    } else {
                                        format!("{filtered_count} / {}", rules.len())
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("最多显示 {MAX_VISIBLE_RULES} 条")),
                    ),
            )
            .child(Input::new(&self.rule_filter).small())
            .child(
                v_flex()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .overflow_hidden()
                    .when(filtered.is_empty(), |this| {
                        this.child(empty_state(
                            if rules.is_empty() {
                                "当前内核没有返回规则"
                            } else {
                                "没有匹配的规则"
                            },
                            theme,
                        ))
                    })
                    .children(
                        filtered.into_iter().enumerate().map(|(position, rule)| {
                            self.render_rule_row(position, rule, theme, cx)
                        }),
                    ),
            )
            .into_any_element()
    }

    fn render_rule_row(
        &self,
        position: usize,
        rule: &zenclash_core::Rule,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let runtime_index = rule.index;
        let stats = rule.extra.as_ref();
        let disabled = stats.is_some_and(|stats| stats.disabled);
        let enabled = !disabled;
        let mut row = h_flex()
            .id(("rule-row", position))
            .items_center()
            .min_h(px(72.))
            .px_4()
            .py_3()
            .gap_4()
            .border_b_1()
            .border_color(theme.border)
            .opacity(if disabled { 0.55 } else { 1.0 })
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(div().text_sm().child(rule.payload.clone()))
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(rule_badge(rule.kind.clone(), theme.primary, theme))
                            .child(rule_badge(rule.proxy.clone(), theme.success, theme))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(
                                format!("规则 #{}", runtime_index.map_or(position, |index| index)),
                            )),
                    ),
            );
        if let Some(stats) = stats {
            let total = stats.hit_count.saturating_add(stats.miss_count);
            row = row.child(
                v_flex()
                    .items_end()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .child(format!("命中 {} / {total}", stats.hit_count)),
                    )
                    .child(
                        div()
                            .max_w(px(180.))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(if stats.hit_at.is_empty() {
                                "尚无命中时间".into()
                            } else {
                                format!("最近命中 {}", stats.hit_at)
                            }),
                    ),
            );
        }
        if let (Some(index), Some(_)) = (runtime_index, stats) {
            row = row.child(
                Switch::new(("rule-enabled", index))
                    .small()
                    .checked(enabled)
                    .disabled(self.mutating || !self.core_kind.capabilities().rule_toggle)
                    .tooltip(if enabled {
                        "禁用规则"
                    } else {
                        "启用规则"
                    })
                    .on_click(cx.listener(move |this, checked, _, cx| {
                        this.set_rule_enabled(index, *checked, cx);
                    })),
            );
        }
        row
    }
}

fn rule_badge(text: String, color: gpui::Hsla, theme: &gpui_component::Theme) -> gpui::Div {
    div()
        .px_2()
        .py_1()
        .rounded(theme.radius)
        .border_1()
        .border_color(color.opacity(0.45))
        .bg(color.opacity(0.1))
        .text_xs()
        .text_color(color)
        .child(text)
}

fn normalize_rule_query(query: &str) -> String {
    query.trim().to_ascii_lowercase()
}

fn rule_matches(rule: &zenclash_core::Rule, query: &str) -> bool {
    query.is_empty()
        || rule.kind.to_ascii_lowercase().contains(query)
        || rule.payload.to_ascii_lowercase().contains(query)
        || rule.proxy.to_ascii_lowercase().contains(query)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_matches_all_visible_rule_fields_case_insensitively() {
        let rule = zenclash_core::Rule {
            kind: "DomainSuffix".into(),
            payload: "Example.COM".into(),
            proxy: "Auto Select".into(),
            ..Default::default()
        };

        assert!(rule_matches(&rule, &normalize_rule_query("DOMAIN")));
        assert!(rule_matches(&rule, &normalize_rule_query("example.com")));
        assert!(rule_matches(&rule, &normalize_rule_query("auto select")));
        assert!(!rule_matches(&rule, &normalize_rule_query("DIRECT")));
    }
}
