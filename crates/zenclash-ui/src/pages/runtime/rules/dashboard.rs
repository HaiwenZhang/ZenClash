use super::*;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{Selectable, progress::Progress};

impl RuntimePage {
    pub(super) fn rule_dashboard(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let Some(projection) = &self.rules.projection else {
            return v_flex()
                .gap_3()
                .child(Input::new(&self.rules.filter).small())
                .child(empty_state(
                    zenclash_i18n::text(if self.rules.projecting {
                        "runtime.empty.loading"
                    } else {
                        "runtime.empty.unavailable"
                    }),
                    theme,
                ))
                .into_any_element();
        };
        let rules = &projection.snapshot.rules;
        let page = list_page(projection.indices.len(), self.rules.page, RULES_PER_PAGE);
        let mut table = panel(theme)
            .w_full()
            .min_w(gpui_kit::rems(42.))
            .child(Input::new(&self.rules.filter).small())
            .child(rule_columns(theme))
            .when(projection.indices.is_empty(), |this| {
                this.child(empty_state(
                    zenclash_i18n::text(if rules.is_empty() {
                        "rules.empty.runtime"
                    } else {
                        "rules.empty.filtered"
                    }),
                    theme,
                ))
            });
        for &position in &projection.indices[page.start..page.end] {
            table = table.child(self.render_rule_row(position, &rules[position], theme, cx));
        }
        table = table.child(
            h_flex()
                .justify_between()
                .pt_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(pagination_summary(page, projection.indices.len())),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("previous-rules-page")
                                .icon(IconName::ChevronLeft)
                                .small()
                                .outline()
                                .label(zenclash_i18n::text("common.actions.previous_page"))
                                .disabled(page.index == 0)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_rules_page(page.index.saturating_sub(1), cx)
                                })),
                        )
                        .child(
                            Button::new("next-rules-page")
                                .icon(IconName::ChevronRight)
                                .small()
                                .outline()
                                .label(zenclash_i18n::text("common.actions.next_page"))
                                .disabled(page.index + 1 >= page.count)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_rules_page(page.index + 1, cx)
                                })),
                        ),
                ),
        );
        let mut inspector = panel(theme).w_80().flex_shrink_0().child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(zenclash_i18n::text("rules.details.title")),
        );
        if let Some((position, rule)) = self
            .rules
            .selected
            .and_then(|position| rules.get(position).map(|rule| (position, rule)))
        {
            inspector = inspector
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(rule.payload.clone()),
                )
                .child(detail(
                    "rules.columns.order",
                    (position + 1).to_string(),
                    theme,
                ))
                .child(detail("rules.columns.kind", rule.kind.clone(), theme))
                .child(detail("rules.columns.proxy", rule.proxy.clone(), theme));
            if let Some(stats) = &rule.extra {
                inspector = inspector
                    .child(detail(
                        "rules.columns.hits",
                        stats.hit_count.to_string(),
                        theme,
                    ))
                    .child(detail(
                        "rules.details.misses",
                        stats.miss_count.to_string(),
                        theme,
                    ))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        if stats.hit_at.is_empty() {
                            zenclash_i18n::text("rules.row.never_hit")
                        } else {
                            zenclash_i18n::text_with(
                                "rules.row.last_hit",
                                &[("time", stats.hit_at.clone())],
                            )
                        },
                    ));
            } else {
                inspector = inspector.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("—"),
                );
            }
        } else {
            inspector = inspector.child(empty_state(
                zenclash_i18n::text("rules.empty.filtered"),
                theme,
            ));
        }
        v_flex()
            .gap_4()
            .child(
                panel(theme).child(
                    h_flex()
                        .justify_between()
                        .child(
                            h_flex()
                                .gap_3()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child(zenclash_i18n::text("rules.summary.runtime")),
                                )
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .child(rules.len().to_string()),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(pagination_summary(page, projection.indices.len())),
                        ),
                ),
            )
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
                    .gap_3()
                    .items_stretch()
                    .child(distribution("rules.charts.types", &projection.kinds, theme))
                    .child(distribution("rules.charts.hits", &projection.hits, theme)),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .flex_wrap()
                    .child(
                        div()
                            .flex_1()
                            .flex_basis(gpui_kit::rems(42.))
                            .min_w_0()
                            .child(table)
                            .overflow_x_scrollbar(),
                    )
                    .child(inspector),
            )
            .into_any_element()
    }

    fn render_rule_row(
        &self,
        position: usize,
        rule: &zenclash_core::Rule,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let identity = rule.index.unwrap_or(position);
        let disabled = rule
            .index
            .and_then(|index| self.rules.confirmed_disabled.get(&index).copied())
            .unwrap_or_else(|| rule.extra.as_ref().is_some_and(|stats| stats.disabled));
        let active = self.rules.selected == Some(position);
        let mut row = h_flex()
            .id(("rule-row", identity))
            .test_support()
            .gap_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .when(active, |row| row.bg(theme.primary.opacity(0.1)))
            .child(div().w_10().text_xs().child((position + 1).to_string()))
            .child(div().w_32().text_xs().truncate().child(rule.kind.clone()))
            .child(
                div().flex_1().min_w_0().child(
                    Button::new(("rule-details", identity))
                        .label(if rule.payload.is_empty() {
                            rule.kind.clone()
                        } else {
                            rule.payload.clone()
                        })
                        .ghost()
                        .small()
                        .w_full()
                        .min_w_0()
                        .truncate()
                        .tooltip(rule.payload.clone())
                        .selected(active)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.rules.selected = Some(position);
                            cx.notify();
                        })),
                ),
            )
            .child(div().w_24().text_xs().truncate().child(rule.proxy.clone()))
            .child(
                div().w_16().text_xs().text_right().child(
                    rule.extra
                        .as_ref()
                        .map_or_else(|| "—".to_owned(), |stats| stats.hit_count.to_string()),
                ),
            );
        if let (Some(index), Some(_)) = (rule.index, &rule.extra) {
            row = row.child(
                div().w_16().child(
                    Switch::new(("rule-enabled", index))
                        .accessibility_label(zenclash_i18n::text_with(
                            "rules.row.enabled_named",
                            &[
                                ("index", index.to_string()),
                                ("payload", rule.payload.clone()),
                            ],
                        ))
                        .small()
                        .checked(!disabled)
                        .disabled(
                            self.rules.pending.contains(&index)
                                || !self.core_kind.capabilities().rule_toggle,
                        )
                        .on_click(cx.listener(move |this, checked, _, cx| {
                            this.set_rule_enabled(index, *checked, cx)
                        })),
                ),
            );
        } else {
            row = row.child(
                div()
                    .w_16()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("—"),
            );
        }
        row.into_any_element()
    }
}

fn panel(theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    v_flex()
        .gap_3()
        .p_4()
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius)
        .bg(theme.group_box)
}

fn detail(key: &str, value: String, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    h_flex()
        .gap_3()
        .text_xs()
        .child(
            div()
                .w_24()
                .text_color(theme.muted_foreground)
                .child(zenclash_i18n::text(key)),
        )
        .child(div().flex_1().min_w_0().child(value))
}

fn distribution(
    title: &str,
    values: &[(String, u64)],
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    let maximum = values.iter().map(|value| value.1).max().unwrap_or(1).max(1);
    panel(theme)
        .flex_1()
        .min_w_0()
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(zenclash_i18n::text(title)),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(zenclash_i18n::text("runtime.charts.top_five")),
        )
        .when(values.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("—"),
            )
        })
        .children(values.iter().map(|(label, count)| {
            h_flex()
                .gap_3()
                .child(div().w_32().text_xs().truncate().child(label.clone()))
                .child(
                    Progress::new((gpui_kit::ElementId::from(title.to_owned()), label.clone()))
                        .accessibility_label(label.clone())
                        .value(*count as f32 / maximum as f32 * 100.)
                        .color(theme.info)
                        .flex_1(),
                )
                .child(div().w_12().text_xs().text_right().child(count.to_string()))
        }))
}

fn rule_columns(theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    h_flex()
        .gap_3()
        .py_2()
        .text_xs()
        .text_color(theme.muted_foreground)
        .border_b_1()
        .border_color(theme.border)
        .child(
            div()
                .w_10()
                .child(zenclash_i18n::text("rules.columns.order")),
        )
        .child(
            div()
                .w_32()
                .child(zenclash_i18n::text("rules.columns.kind")),
        )
        .child(
            div()
                .flex_1()
                .child(zenclash_i18n::text("rules.columns.payload")),
        )
        .child(
            div()
                .w_24()
                .child(zenclash_i18n::text("rules.columns.proxy")),
        )
        .child(
            div()
                .w_16()
                .text_right()
                .child(zenclash_i18n::text("rules.columns.hits")),
        )
        .child(
            div()
                .w_16()
                .child(zenclash_i18n::text("rules.columns.enabled")),
        )
}
