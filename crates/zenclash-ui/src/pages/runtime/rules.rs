use gpui_kit::base::TestSupportExt;
use std::collections::{HashMap, HashSet};

mod projection;

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
    query: String,
    projection: Option<projection::RuleProjection>,
    worker: projection::ProjectionWorker,
    pub(super) confirmed_disabled: HashMap<usize, bool>,
    pub(super) projecting: bool,
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
                this.rules.query = normalize_rule_query(&this.rules.filter.read(cx).value());
                this.update_rule_presentation(cx);
                cx.notify();
            }
        });
        (
            Self {
                filter,
                page: 0,
                pending: HashSet::new(),
                query: String::new(),
                projection: None,
                worker: projection::ProjectionWorker::default(),
                confirmed_disabled: HashMap::new(),
                projecting: false,
            },
            subscription,
        )
    }
}

impl RulesUiState {
    pub(super) fn cancel_projection(&mut self) {
        self.worker.cancel();
        self.projecting = false;
    }

    pub(super) fn release_presentation(&mut self) {
        self.cancel_projection();
        self.projection = None;
        self.confirmed_disabled.clear();
    }
}

impl RuntimePage {
    pub(super) fn update_rule_presentation(&mut self, cx: &mut Context<Self>) {
        let RuntimeData::Rules(snapshot) = &self.data else {
            self.rules.release_presentation();
            return;
        };
        if self.page != Page::Rules || !self.live_updates_enabled() {
            return;
        }
        if self.rules.projection.as_ref().is_some_and(|projection| {
            std::sync::Arc::ptr_eq(&projection.snapshot, snapshot)
                && projection.query == self.rules.query
        }) {
            return;
        }
        let source = snapshot.clone();
        let (generation, task) =
            self.rules
                .worker
                .start(&self.runtime, source.clone(), self.rules.query.clone());
        self.rules.projecting = true;
        self.rules.projection = None;
        cx.spawn(async move |this, cx| {
            let result = task.await.map_err(|error| error.to_string()).and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                if !this.rules.worker.is_current(generation)
                    || !matches!(&this.data, RuntimeData::Rules(current) if std::sync::Arc::ptr_eq(current, &source)) { return; }
                this.rules.projecting = false;
                match result {
                    Ok(Some(projection)) => this.rules.projection = Some(projection),
                    Ok(None) => {},
                    Err(error) => this.error = Some(zenclash_i18n::text_with("rules.errors.filter_task", &[("error", error)])),
                }
                cx.notify();
            });
        }).detach();
    }

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
                            this.rules.confirmed_disabled.insert(index, !enabled);
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
        let query = &projection.query;
        let filtered_count = projection.indices.len();
        let page = list_page(filtered_count, self.rules.page, RULES_PER_PAGE);
        let filtered = &projection.indices[page.start..page.end];
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
                                    .font_weight(gpui_kit::FontWeight::BOLD)
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
                    .children(
                        filtered
                            .iter()
                            .map(|&index| self.render_rule_row(index, &rules[index], theme, cx)),
                    ),
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
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let runtime_index = rule.index;
        let stats = rule.extra.as_ref();
        let disabled = runtime_index
            .and_then(|index| self.rules.confirmed_disabled.get(&index).copied())
            .unwrap_or_else(|| stats.is_some_and(|stats| stats.disabled));
        let enabled = !disabled;
        let mut row = h_flex()
            .id(("rule-row", position))
            .test_support()
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
                    .accessibility_label(zenclash_i18n::text_with(
                        "rules.row.enabled_named",
                        &[
                            ("index", index.to_string()),
                            ("payload", rule.payload.clone()),
                        ],
                    ))
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

fn rule_badge(
    text: String,
    color: gpui_kit::Hsla,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
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
