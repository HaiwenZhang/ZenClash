mod details;
mod table;

use std::borrow::Cow;

use self::table::rule_columns;
use super::*;
use gpui_kit::component::Selectable;
use gpui_kit::component::button::{ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement;

impl RulesView {
    fn update_rule_presentation_immediate(&mut self, cx: &mut Context<Self>) {
        let Some(source) = &self.source else {
            self.release_presentation(cx);
            return;
        };
        if !self.context.is_some_and(|context| context.active) {
            return;
        }
        let snapshot = &source.catalog;
        if self.projection.as_ref().is_some_and(|projection| {
            Arc::ptr_eq(&projection.snapshot, snapshot)
                && projection.filter == self.filters
                && projection.confirmed_disabled == self.confirmed_disabled
        }) {
            self.cancel_projection();
            return;
        }
        let source = snapshot.clone();
        let (generation, task) = self.worker.start_with_options(
            &self.runtime,
            source.clone(),
            self.filters.clone(),
            self.confirmed_disabled.clone(),
            std::time::Duration::ZERO,
        );
        self.projecting = true;
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                if !this.worker.is_current(generation)
                    || !this
                        .source
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(&current.catalog, &source))
                {
                    return;
                }
                this.projecting = false;
                match result {
                    Ok(Some(projection)) => this.projection = Some(projection),
                    Ok(None) => {}
                    Err(error) => {
                        if let Some(context) = this.context {
                            cx.emit(RuleEvent::PresentationFailed {
                                navigation: context.navigation,
                                error: zenclash_i18n::text_with(
                                    "rules.errors.filter_task",
                                    &[("error", error)],
                                ),
                            });
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn rule_dashboard(
        &self,
        compact: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let Some(projection) = &self.projection else {
            return v_flex()
                .gap_3()
                .child(
                    Input::new(&self.filter_input)
                        .prefix(gpui_kit::component::Icon::new(IconName::Search))
                        .small(),
                )
                .child(empty_state(
                    zenclash_i18n::text(if self.projecting {
                        "runtime.empty.loading"
                    } else {
                        "runtime.empty.unavailable"
                    }),
                    theme,
                ))
                .into_any_element();
        };
        let rules = &projection.snapshot.rules;
        let query_changing = self.projecting && projection.filter.query != self.filters.query;
        let show_results =
            !query_changing && (projection.filter == self.filters || self.projecting);
        let indices: Cow<'_, [usize]> = if !show_results {
            Cow::Borrowed(&[])
        } else if self.filters.disabled_only
            && projection.filter == self.filters
            && projection.confirmed_disabled != self.confirmed_disabled
        {
            Cow::Owned(
                rules
                    .iter()
                    .enumerate()
                    .filter_map(|(index, rule)| {
                        (projection::rule_is_disabled(rule, &self.confirmed_disabled)
                            && rule_matches(rule, &self.filters.query)
                            && self
                                .filters
                                .rule_kind
                                .as_ref()
                                .is_none_or(|kind| *kind == rule.kind)
                            && self
                                .filters
                                .policy
                                .as_ref()
                                .is_none_or(|policy| *policy == rule.proxy))
                        .then_some(index)
                    })
                    .collect(),
            )
        } else {
            Cow::Borrowed(projection.indices.as_slice())
        };
        let page = list_page(indices.len(), self.page_index, RULES_PER_PAGE);
        let filters = h_flex().gap_2().flex_wrap().children(
            [
                (
                    true,
                    "unified.rules.all_types",
                    self.filters.rule_kind.clone(),
                    projection.types.clone(),
                ),
                (
                    false,
                    "unified.rules.all_policies",
                    self.filters.policy.clone(),
                    projection.policies.clone(),
                ),
            ]
            .into_iter()
            .map(|(is_kind, key, selected, options)| {
                let owner = cx.entity().downgrade();
                Button::new(if is_kind {
                    "rules-type-filter"
                } else {
                    "rules-policy-filter"
                })
                .outline()
                .icon(IconName::ChevronDown)
                .label(selected.clone().unwrap_or_else(|| zenclash_i18n::text(key)))
                .dropdown_menu(move |mut menu, _, _| {
                    for value in std::iter::once(None).chain(options.iter().cloned().map(Some)) {
                        let owner = owner.clone();
                        let label = value.clone().unwrap_or_else(|| zenclash_i18n::text(key));
                        menu = menu.item(
                            PopupMenuItem::new(label)
                                .checked(value == selected)
                                .on_click(move |_, _, cx| {
                                    let _ = owner.update(cx, |view, cx| {
                                        if is_kind {
                                            view.filters.rule_kind = value.clone();
                                        } else {
                                            view.filters.policy = value.clone();
                                        }
                                        view.page_index = 0;
                                        view.update_rule_presentation_immediate(cx);
                                        cx.notify();
                                    });
                                }),
                        );
                    }
                    menu
                })
            }),
        );
        let mut table = panel(theme)
            .id("rules-table-panel")
            .test_support()
            .gap_0()
            .w_full()
            .min_w_0()
            .flex_1()
            .min_h_0()
            .child(
                h_flex()
                    .gap_2()
                    .pb_3()
                    .flex_wrap()
                    .child(
                        div().flex_1().min_w(gpui_kit::rems(12.)).child(
                            Input::new(&self.filter_input)
                                .prefix(gpui_kit::component::Icon::new(IconName::Search)),
                        ),
                    )
                    .child(filters)
                    .child(
                        gpui_kit::component::checkbox::Checkbox::new("rules-disabled-filter")
                            .label(zenclash_i18n::text("redesign.only_disabled"))
                            .checked(self.filters.disabled_only)
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.filters.disabled_only = *checked;
                                this.page_index = 0;
                                this.update_rule_presentation_immediate(cx);
                                cx.notify();
                            })),
                    ),
            )
            .when(!compact, |table| table.child(rule_columns(theme)));
        let mut rows = v_flex().gap_0().when(indices.is_empty(), |this| {
            this.child(empty_state(
                zenclash_i18n::text(if self.projecting {
                    "runtime.empty.loading"
                } else if !show_results {
                    "runtime.empty.unavailable"
                } else if rules.is_empty() {
                    "rules.empty.runtime"
                } else {
                    "rules.empty.filtered"
                }),
                theme,
            ))
        });
        for &position in &indices[page.start..page.end] {
            rows = rows.child(self.render_rule_row(position, &rules[position], compact, theme, cx));
        }
        table = table.child(
            div()
                .id(("rule-table-viewport", page.index))
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .child(rows),
        );
        table =
            table.child(
                h_flex()
                    .justify_between()
                    .flex_wrap()
                    .gap_2()
                    .pt_2()
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        if show_results {
                            pagination_summary(page, indices.len())
                        } else {
                            zenclash_i18n::text(if self.projecting {
                                "runtime.empty.loading"
                            } else {
                                "runtime.empty.unavailable"
                            })
                        },
                    ))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("previous-rules-page")
                                    .h_10()
                                    .outline()
                                    .label(zenclash_i18n::text("common.actions.previous_page"))
                                    .disabled(!show_results || page.index == 0)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_rules_page(page.index.saturating_sub(1), cx)
                                    })),
                            )
                            .children(
                                (page.index.saturating_sub(2)
                                    ..page.count.min(page.index.saturating_sub(2) + 5))
                                    .map(|index| {
                                        Button::new(("rules-numbered-page", index))
                                            .label((index + 1).to_string())
                                            .h_10()
                                            .outline()
                                            .selected(index == page.index)
                                            .when(index == page.index, |button| {
                                                button.custom(
                                                    ButtonCustomVariant::new(cx)
                                                        .color(theme.primary.opacity(0.2))
                                                        .foreground(theme.primary)
                                                        .hover(theme.table_active)
                                                        .active(theme.table_active),
                                                )
                                            })
                                            .disabled(!show_results)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.set_rules_page(index, cx)
                                            }))
                                    }),
                            )
                            .child(
                                Button::new("next-rules-page")
                                    .h_10()
                                    .outline()
                                    .label(zenclash_i18n::text("common.actions.next_page"))
                                    .disabled(!show_results || page.index + 1 >= page.count)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_rules_page(page.index + 1, cx)
                                    })),
                            ),
                    ),
            );
        let known = rules.iter().all(|rule| rule.extra.is_some());
        let disabled = rules
            .iter()
            .filter(|rule| projection::rule_is_disabled(rule, &self.confirmed_disabled))
            .count();
        v_flex()
            .h_full()
            .min_h_0()
            .gap_3()
            .child(
                h_flex().gap_3().children(
                    [
                        (
                            "rules.summary.runtime",
                            rules.len().to_string(),
                            gpui_kit::assets::IconName::FileText,
                        ),
                        (
                            "common.status.enabled",
                            if known {
                                (rules.len() - disabled).to_string()
                            } else {
                                "—".into()
                            },
                            gpui_kit::assets::IconName::Check,
                        ),
                        (
                            "redesign.disabled",
                            if known {
                                disabled.to_string()
                            } else {
                                "—".into()
                            },
                            gpui_kit::assets::IconName::Minus,
                        ),
                    ]
                    .into_iter()
                    .map(|(label, value, icon)| {
                        panel(theme).flex_1().min_w_0().py_3().child(
                            h_flex()
                                .gap_3()
                                .child(
                                    h_flex()
                                        .size_10()
                                        .justify_center()
                                        .rounded_full()
                                        .bg(theme.secondary)
                                        .text_color(theme.primary)
                                        .child(gpui_kit::component::Icon::new(icon).size_5()),
                                )
                                .child(
                                    v_flex()
                                        .gap_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(theme.muted_foreground)
                                                .child(zenclash_i18n::text(label)),
                                        )
                                        .child(
                                            div()
                                                .text_2xl()
                                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                .child(value),
                                        ),
                                ),
                        )
                    }),
                ),
            )
            .child(table)
            .into_any_element()
    }
}

fn panel(theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    v_flex()
        .gap_2()
        .p_4()
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius_lg)
        .bg(theme.group_box)
}
