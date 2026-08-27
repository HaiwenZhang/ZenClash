use super::{
    Context, Disableable, FluentBuilder, Input, InteractiveElement, IntoElement, Page,
    ParentElement, RuntimeData, RuntimePage, Sizable, Styled, Switch, div, empty_state, h_flex,
    message_banner, px, v_flex,
};

const MAX_VISIBLE_RULES: usize = 800;

impl RuntimePage {
    fn set_rule_enabled(&mut self, index: usize, enabled: bool, cx: &mut Context<Self>) {
        if !self.core_kind.capabilities().rule_toggle {
            self.error = Some(zenclash_i18n::text_with(
                "rules.warnings.toggle_unavailable",
                &[("core", self.core_kind.display_name().to_owned())],
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
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "rules.errors.status_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data) {
                            this.notice = Some(if enabled {
                                zenclash_i18n::text_with(
                                    "rules.notices.enabled",
                                    &[("index", index.to_string())],
                                )
                            } else {
                                zenclash_i18n::text_with(
                                    "rules.notices.disabled",
                                    &[("index", index.to_string())],
                                )
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
                    zenclash_i18n::text_with(
                        "rules.warnings.stats_unavailable",
                        &[("core", self.core_kind.display_name().to_owned())],
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
                                    zenclash_i18n::text("rules.summary.runtime")
                                } else {
                                    zenclash_i18n::text("rules.summary.filtered")
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
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        zenclash_i18n::text_with(
                            "rules.summary.limit",
                            &[("limit", MAX_VISIBLE_RULES.to_string())],
                        ),
                    )),
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
                                zenclash_i18n::text("rules.empty.runtime")
                            } else {
                                zenclash_i18n::text("rules.empty.filtered")
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
    ) -> gpui::AnyElement {
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
                                zenclash_i18n::text_with(
                                    "rules.row.index",
                                    &[("index", runtime_index.unwrap_or(position).to_string())],
                                ),
                            )),
                    ),
            );
        if let Some(stats) = stats {
            let total = stats.hit_count.saturating_add(stats.miss_count);
            row = row.child(
                v_flex()
                    .items_end()
                    .gap_1()
                    .child(div().text_xs().child(zenclash_i18n::text_with(
                        "rules.row.hits",
                        &[
                            ("hits", stats.hit_count.to_string()),
                            ("total", total.to_string()),
                        ],
                    )))
                    .child(
                        div()
                            .max_w(px(180.))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(if stats.hit_at.is_empty() {
                                zenclash_i18n::text("rules.row.never_hit")
                            } else {
                                zenclash_i18n::text_with(
                                    "rules.row.last_hit",
                                    &[("time", stats.hit_at.clone())],
                                )
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
                        zenclash_i18n::text("rules.row.disable")
                    } else {
                        zenclash_i18n::text("rules.row.enable")
                    })
                    .on_click(cx.listener(move |this, checked, _, cx| {
                        this.set_rule_enabled(index, *checked, cx);
                    })),
            );
        }
        row.into_any_element()
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
