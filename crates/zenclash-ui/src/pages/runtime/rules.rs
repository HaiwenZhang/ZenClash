use std::collections::HashSet;

use super::{
    AppContext, Button, Context, Disableable, Entity, FluentBuilder, IconName, Input, InputEvent,
    InputState, InteractiveElement, IntoElement, Page, ParentElement, RuntimeData, RuntimePage,
    Sizable, Styled, Subscription, Switch, Window, contains_ascii_case_insensitive, div,
    empty_state, h_flex, list_page, message_banner, pagination_summary, px, v_flex,
};

const RULES_PER_PAGE: usize = 100;

pub(super) struct RulesUiState {
    pub(super) filter: Entity<InputState>,
    pub(super) page: usize,
    pub(super) pending: HashSet<usize>,
}

impl RulesUiState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<RuntimePage>) -> (Self, Subscription) {
        let filter = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(zenclash_i18n::text("runtime.placeholders.rule_filter"))
        });
        let subscription = cx.subscribe(&filter, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.rules.page = 0;
                cx.notify();
            }
        });
        (
            Self {
                filter,
                page: 0,
                pending: HashSet::new(),
            },
            subscription,
        )
    }
}

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
        if self.page != Page::Rules || !self.rules.pending.insert(index) {
            return;
        }
        self.invalidate_page_load();
        let token = self.page_task_token_for(Page::Rules);
        self.error = None;
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            client
                .apply_rule_disabled(index, !enabled)
                .await
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
                this.rules.pending.remove(&index);
                match result {
                    Ok(()) => {
                        if this.is_page_task_current(token) {
                            apply_rule_disabled(&mut this.data, index, !enabled);
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
                            if this.rules.pending.is_empty() {
                                this.refresh(cx);
                            }
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

    fn set_rules_page(&mut self, page: usize, cx: &mut Context<Self>) {
        self.rules.page = page;
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
        let query = normalize_rule_query(&self.rules.filter.read(cx).value());
        let filtered_count = rules
            .iter()
            .filter(|rule| rule_matches(rule, &query))
            .count();
        let page = list_page(filtered_count, self.rules.page, RULES_PER_PAGE);
        let filtered = rules
            .iter()
            .filter(|rule| rule_matches(rule, &query))
            .skip(page.start)
            .take(page.end - page.start)
            .collect::<Vec<_>>();
        let previous_page = page.index.saturating_sub(1);
        let next_page = page.index + 1;
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
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(pagination_summary(page, filtered_count)),
                    ),
            )
            .child(Input::new(&self.rules.filter).small())
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
                    .children(filtered.into_iter().enumerate().map(|(offset, rule)| {
                        self.render_rule_row(page.start + offset, rule, theme, cx)
                    })),
            )
            .when(page.count > 1, |this| {
                this.child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(pagination_summary(page, filtered_count)),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("previous-rules-page")
                                        .icon(IconName::ChevronLeft)
                                        .label(zenclash_i18n::text("common.actions.previous_page"))
                                        .small()
                                        .outline()
                                        .disabled(page.index == 0)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.set_rules_page(previous_page, cx);
                                        })),
                                )
                                .child(
                                    Button::new("next-rules-page")
                                        .icon(IconName::ChevronRight)
                                        .label(zenclash_i18n::text("common.actions.next_page"))
                                        .small()
                                        .outline()
                                        .disabled(page.index + 1 >= page.count)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.set_rules_page(next_page, cx);
                                        })),
                                ),
                        ),
                )
            })
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
                    .disabled(
                        self.rules.pending.contains(&index)
                            || !self.core_kind.capabilities().rule_toggle,
                    )
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
    query.trim().to_owned()
}

fn rule_matches(rule: &zenclash_core::Rule, query: &str) -> bool {
    query.is_empty()
        || contains_ascii_case_insensitive(&rule.kind, query)
        || contains_ascii_case_insensitive(&rule.payload, query)
        || contains_ascii_case_insensitive(&rule.proxy, query)
}

fn apply_rule_disabled(data: &mut RuntimeData, index: usize, disabled: bool) -> bool {
    let RuntimeData::Rules(catalog) = data else {
        return false;
    };
    let Some(stats) = catalog
        .rules
        .iter_mut()
        .find(|rule| rule.index == Some(index))
        .and_then(|rule| rule.extra.as_mut())
    else {
        return false;
    };
    stats.disabled = disabled;
    true
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

    #[test]
    fn acknowledged_rule_toggle_updates_only_the_matching_local_rule() {
        let mut data = RuntimeData::Rules(zenclash_core::RuleCatalog {
            rules: vec![zenclash_core::Rule {
                index: Some(12),
                extra: Some(zenclash_core::RuleRuntimeStats::default()),
                ..Default::default()
            }],
        });

        assert!(apply_rule_disabled(&mut data, 12, true));
        let RuntimeData::Rules(catalog) = data else {
            panic!("expected rule catalog");
        };
        assert!(
            catalog.rules[0]
                .extra
                .as_ref()
                .is_some_and(|stats| stats.disabled)
        );
    }
}
