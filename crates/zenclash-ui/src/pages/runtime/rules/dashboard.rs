use super::*;
use crate::components::mint_switch::MintSwitch as Switch;
use gpui_kit::component::Selectable;
use gpui_kit::component::button::{ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, WindowExt};

impl RuntimePage {
    pub(super) fn rule_dashboard(
        &self,
        compact: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let Some(projection) = &self.rules.projection else {
            return v_flex()
                .gap_3()
                .child(
                    Input::new(&self.rules.filter)
                        .prefix(gpui_kit::component::Icon::new(IconName::Search))
                        .small(),
                )
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
        let show_results = !self.rules.projecting && projection.query == self.rules.query;
        let indices = if show_results {
            projection.indices.as_slice()
        } else {
            &[]
        };
        let page = list_page(indices.len(), self.rules.page, RULES_PER_PAGE);
        let filters = h_flex().gap_2().flex_wrap().children(
            [
                (
                    true,
                    "unified.rules.all_types",
                    self.rules.worker.kind.clone(),
                    projection.types.clone(),
                ),
                (
                    false,
                    "unified.rules.all_policies",
                    self.rules.worker.policy.clone(),
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
                                    let _ = owner.update(cx, |page, cx| {
                                        if is_kind {
                                            page.rules.worker.kind = value.clone();
                                        } else {
                                            page.rules.worker.policy = value.clone();
                                        }
                                        page.rules.page = 0;
                                        page.rules.projection = None;
                                        page.update_rule_presentation(cx);
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
                            Input::new(&self.rules.filter)
                                .prefix(gpui_kit::component::Icon::new(IconName::Search)),
                        ),
                    )
                    .child(filters)
                    .child(
                        gpui_kit::component::checkbox::Checkbox::new("rules-disabled-filter")
                            .label(zenclash_i18n::text("redesign.only_disabled"))
                            .checked(self.rules.worker.disabled_only)
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.rules.worker.disabled_only = *checked;
                                this.rules.page = 0;
                                this.rules.projection = None;
                                this.update_rule_presentation(cx);
                                cx.notify();
                            })),
                    ),
            )
            .when(!compact, |table| table.child(rule_columns(theme)));
        let mut rows = v_flex().gap_0().when(indices.is_empty(), |this| {
            this.child(empty_state(
                zenclash_i18n::text(if self.rules.projecting {
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
                            zenclash_i18n::text(if self.rules.projecting {
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
            .filter(|rule| {
                rule.index
                    .and_then(|index| self.rules.confirmed_disabled.get(&index).copied())
                    .unwrap_or_else(|| rule.extra.as_ref().is_some_and(|stats| stats.disabled))
            })
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

    fn open_rule_details(&mut self, position: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.rules.selected = Some(position);
        self.rules.details = self
            .rules
            .projection
            .as_ref()
            .map(|projection| projection.snapshot.clone());
        let owner = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let content = owner
                .update(cx, |page, cx| {
                    let theme = cx.theme().clone();
                    page.render_rule_details(&theme, cx)
                })
                .ok();
            let close_owner = owner.clone();
            dialog
                .bg(cx.theme().group_box)
                .title(zenclash_i18n::text("rules.details.title"))
                .width(window.rem_size() * 34.)
                .margin_top(
                    ((window.viewport_size().height - window.rem_size() * 36.) / 2.)
                        .max(gpui_kit::px(16.)),
                )
                .when_some(content, |dialog, content| dialog.child(content))
                .on_close(move |_, _, cx| {
                    let _ = close_owner.update(cx, |page, _| page.rules.details = None);
                })
        });
        cx.notify();
    }

    fn render_rule_details(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let Some(snapshot) = &self.rules.details else {
            return empty_state(zenclash_i18n::text("runtime.empty.unavailable"), theme)
                .into_any_element();
        };
        let rules = &snapshot.rules;
        let current = matches!(&self.data, RuntimeData::Rules { catalog, .. } if std::sync::Arc::ptr_eq(catalog, snapshot));
        let mut inspector = v_flex()
            .id("rule-inspector")
            .test_support()
            .gap_3()
            .min_w_0();
        if let Some((position, rule)) = self
            .rules
            .selected
            .and_then(|position| rules.get(position).map(|rule| (position, rule)))
        {
            let disabled = rule
                .index
                .and_then(|index| self.rules.confirmed_disabled.get(&index).copied())
                .or_else(|| rule.extra.as_ref().map(|stats| stats.disabled));
            let outlet = match &self.data {
                RuntimeData::Rules { proxies, .. } => rule_outlet(proxies.as_ref(), &rule.proxy),
                _ => None,
            };
            let payload = if rule.payload.is_empty() {
                &rule.kind
            } else {
                &rule.payload
            };
            inspector = inspector
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(field_label("rules.columns.payload", theme))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .truncate()
                                        .child(payload.clone()),
                                )
                                .child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .rounded(theme.radius)
                                        .border_1()
                                        .border_color(theme.border)
                                        .text_sm()
                                        .child(format!("# {}", position + 1)),
                                ),
                        )
                        .child(detail("rules.details.match_kind", rule.kind.clone(), theme))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(field_label("rules.details.policy", theme))
                                .child(policy_badge(&rule.proxy, theme)),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(field_label("rules.details.enabled", theme))
                                .when_some(
                                    rule.index.filter(|_| rule.extra.is_some()),
                                    |row, index| {
                                        row.child(
                                            Switch::new(("inspector-rule-enabled", index))
                                                .accessibility_label(zenclash_i18n::text_with(
                                                    "rules.row.enabled_named",
                                                    &[
                                                        ("index", index.to_string()),
                                                        ("payload", rule.payload.clone()),
                                                    ],
                                                ))
                                                .checked(disabled == Some(false))
                                                .disabled(
                                                    !current
                                                        || self.rules.pending.contains(&index)
                                                        || !self
                                                            .core_kind
                                                            .capabilities()
                                                            .rule_toggle,
                                                )
                                                .on_click(cx.listener(
                                                    move |this, checked, _, cx| {
                                                        this.set_rule_enabled(index, *checked, cx)
                                                    },
                                                )),
                                        )
                                    },
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(if disabled == Some(false) {
                                            theme.primary
                                        } else {
                                            theme.muted_foreground
                                        })
                                        .child(zenclash_i18n::text(match disabled {
                                            Some(false) => "common.status.enabled",
                                            Some(true) => "common.status.disabled",
                                            None => "common.status.unknown",
                                        })),
                                ),
                        ),
                )
                .when_some(
                    match &self.data {
                        RuntimeData::Rules { config, .. } => config.as_ref(),
                        _ => None,
                    },
                    |view, config| {
                        view.child(detail("rules.summary.mode", config.mode.clone(), theme))
                    },
                )
                .child(
                    h_flex()
                        .gap_2()
                        .w_full()
                        .child(flow_step("rules.details.match", payload, false, theme))
                        .child(
                            gpui_kit::component::Icon::new(IconName::ArrowRight)
                                .size_4()
                                .flex_shrink_0(),
                        )
                        .child(flow_step("rules.details.policy", &rule.proxy, true, theme))
                        .child(
                            gpui_kit::component::Icon::new(IconName::ArrowRight)
                                .size_4()
                                .flex_shrink_0(),
                        )
                        .child(flow_step(
                            "rules.details.outlet",
                            outlet.as_deref().unwrap_or("—"),
                            false,
                            theme,
                        )),
                )
                .child(
                    h_flex().gap_3().children(
                        [
                            (
                                "rules.details.matches",
                                rule.extra.as_ref().map_or_else(
                                    || "—".into(),
                                    |stats| stats.hit_count.to_string(),
                                ),
                            ),
                            (
                                "rules.details.misses",
                                rule.extra.as_ref().map_or_else(
                                    || "—".into(),
                                    |stats| stats.miss_count.to_string(),
                                ),
                            ),
                            (
                                "rules.details.last_hit",
                                rule.extra
                                    .as_ref()
                                    .filter(|stats| stats.hit_count > 0)
                                    .map_or_else(
                                        || "—".into(),
                                        |stats| rule_hit_time(&stats.hit_at),
                                    ),
                            ),
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(index, (key, value))| {
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .when(index > 0, |item| {
                                    item.pl_3().border_l_1().border_color(theme.border)
                                })
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child(zenclash_i18n::text(key)),
                                )
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .truncate()
                                        .child(value),
                                )
                        }),
                    ),
                )
                .child(
                    div()
                        .p_3()
                        .rounded(theme.radius)
                        .bg(theme.secondary)
                        .text_sm()
                        .font_family(theme.mono_font_family.clone())
                        .child(if rule.payload.is_empty() {
                            format!("{},{}", rule.kind, rule.proxy)
                        } else {
                            format!("{},{},{}", rule.kind, rule.payload, rule.proxy)
                        }),
                )
                .child(
                    h_flex().justify_between().gap_3().child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("rules.details.order_note")),
                    ),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(zenclash_i18n::text("rules.details.stats_note")),
                );
        } else {
            inspector = inspector.child(empty_state(
                zenclash_i18n::text("rules.details.select"),
                theme,
            ));
        }
        inspector
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("copy-rule")
                            .outline()
                            .label(zenclash_i18n::text("redesign.copy_rule"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(rule) =
                                    this.rules.details.as_ref().and_then(|snapshot| {
                                        this.rules
                                            .selected
                                            .and_then(|position| snapshot.rules.get(position))
                                    })
                                {
                                    let content = if rule.payload.is_empty() {
                                        format!("{},{}", rule.kind, rule.proxy)
                                    } else {
                                        format!("{},{},{}", rule.kind, rule.payload, rule.proxy)
                                    };
                                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                                        content,
                                    ));
                                }
                            })),
                    )
                    .child(
                        Button::new("close-rule-details")
                            .primary()
                            .label(zenclash_i18n::text("common.actions.close"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.rules.details = None;
                                window.close_dialog(cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_rule_row(
        &self,
        position: usize,
        rule: &zenclash_core::Rule,
        compact: bool,
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
            .when(compact, |row| row.flex_wrap())
            .gap_2()
            .py_1()
            .px_2()
            .min_h(gpui_kit::rems(2.125))
            .border_b_1()
            .border_color(theme.border)
            .when(active, |row| {
                row.rounded(theme.radius)
                    .border_color(theme.table_active)
                    .bg(theme.table_active)
            })
            .child(
                div()
                    .w_12()
                    .flex_shrink_0()
                    .text_sm()
                    .child((position + 1).to_string()),
            )
            .child(
                div()
                    .w(gpui_kit::rems(if compact { 7. } else { 9. }))
                    .text_sm()
                    .truncate()
                    .child(rule.kind.clone()),
            )
            .child(
                div().flex_1().min_w(gpui_kit::rems(10.)).child(
                    Button::new(("rule-details", identity))
                        .accessibility_label(if rule.payload.is_empty() {
                            rule.kind.clone()
                        } else {
                            rule.payload.clone()
                        })
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .text_left()
                                .font_weight(gpui_kit::FontWeight::NORMAL)
                                .truncate()
                                .child(if rule.payload.is_empty() {
                                    rule.kind.clone()
                                } else {
                                    rule.payload.clone()
                                }),
                        )
                        .custom(ButtonCustomVariant::new(cx).foreground(theme.foreground))
                        .small()
                        .px_0()
                        .font_weight(gpui_kit::FontWeight::NORMAL)
                        .w_full()
                        .justify_start()
                        .min_w_0()
                        .truncate()
                        .tooltip(rule.payload.clone())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_rule_details(position, window, cx);
                        })),
                ),
            )
            .child(h_flex().w_24().child(policy_badge(&rule.proxy, theme)))
            .child(
                div().w_20().text_sm().text_right().child(
                    rule.extra
                        .as_ref()
                        .map_or_else(|| "—".to_owned(), |stats| stats.hit_count.to_string()),
                ),
            );
        if let (Some(index), Some(_)) = (rule.index, &rule.extra) {
            row = row.child(
                h_flex()
                    .w_24()
                    .flex_shrink_0()
                    .gap_2()
                    .child(
                        Switch::new(("rule-enabled", index))
                            .accessibility_label(zenclash_i18n::text_with(
                                "rules.row.enabled_named",
                                &[
                                    ("index", index.to_string()),
                                    ("payload", rule.payload.clone()),
                                ],
                            ))
                            .checked(!disabled)
                            .disabled(
                                self.rules.pending.contains(&index)
                                    || !self.core_kind.capabilities().rule_toggle,
                            )
                            .on_click(cx.listener(move |this, checked, _, cx| {
                                this.set_rule_enabled(index, *checked, cx)
                            })),
                    )
                    .child(div().text_xs().child(zenclash_i18n::text(if disabled {
                        "redesign.disabled"
                    } else {
                        "common.status.enabled"
                    }))),
            );
        } else {
            row = row.child(
                div()
                    .w_24()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("—"),
            );
        }
        row.child(
            Button::new(("open-rule-details", identity))
                .ghost()
                .small()
                .label(zenclash_i18n::text("redesign.details"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_rule_details(position, window, cx)
                })),
        )
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

fn field_label(key: &str, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .w_24()
        .flex_shrink_0()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(zenclash_i18n::text(key))
}

fn detail(key: &str, value: String, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    h_flex()
        .gap_2()
        .text_sm()
        .child(field_label(key, theme))
        .child(div().flex_1().min_w_0().truncate().child(value))
}

fn flow_step(
    key: &str,
    value: &str,
    active: bool,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    v_flex()
        .flex_1()
        .min_w_0()
        .gap_1()
        .py_3()
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(if active {
                    theme.primary
                } else {
                    theme.foreground
                })
                .truncate()
                .child(value.to_owned()),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(zenclash_i18n::text(key)),
        )
}

fn rule_hit_time(time: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(time)
        .map(|time| time.format("%H:%M:%S").to_string())
        .unwrap_or_else(|_| {
            if time.len() == 8 && chrono::NaiveTime::parse_from_str(time, "%H:%M:%S").is_ok() {
                time.to_owned()
            } else {
                "—".into()
            }
        })
}

// A load-balancing group has no unique exit. Stop on missing members or cycles.
fn rule_outlet(catalog: Option<&zenclash_core::ProxyCatalog>, policy: &str) -> Option<String> {
    if policy.eq_ignore_ascii_case("DIRECT") || policy.starts_with("REJECT") {
        return Some(policy.to_owned());
    }
    let catalog = catalog?;
    let mut current = policy;
    for _ in 0..=catalog.groups().len() {
        let Some(group) = catalog.groups().iter().find(|group| group.name == current) else {
            let id = zenclash_core::ProxyNodeId::new(current.to_owned(), None);
            return catalog.node(&id).map(|node| node.name.clone());
        };
        if !matches!(
            group.behavior,
            zenclash_core::ProxyGroupBehavior::Selector
                | zenclash_core::ProxyGroupBehavior::Automatic { .. }
        ) {
            return None;
        }
        if group.now.is_empty() {
            return None;
        }
        if group.now.eq_ignore_ascii_case("DIRECT") || group.now.starts_with("REJECT") {
            return Some(group.now.clone());
        }
        if !catalog
            .groups()
            .iter()
            .any(|candidate| candidate.name == group.now)
        {
            return group
                .all
                .iter()
                .find(|id| id.controller_name() == group.now)
                .and_then(|id| catalog.node(id))
                .map(|node| node.name.clone());
        }
        current = &group.now;
    }
    None
}

fn rule_columns(theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    h_flex()
        .gap_2()
        .py_2()
        .px_2()
        .bg(theme.table_head)
        .text_sm()
        .text_color(theme.muted_foreground)
        .border_b_1()
        .border_color(theme.border)
        .child(
            div()
                .w_12()
                .child(zenclash_i18n::text("rules.columns.order")),
        )
        .child(
            div()
                .w(gpui_kit::rems(9.))
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
                .w_20()
                .whitespace_nowrap()
                .text_right()
                .child(zenclash_i18n::text("rules.columns.hits")),
        )
        .child(div().w_24().child(zenclash_i18n::text("redesign.status")))
        .child(div().w_16().child(zenclash_i18n::text("redesign.action")))
}

fn policy_badge(policy: &str, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    let color = if policy.eq_ignore_ascii_case("DIRECT") {
        theme.primary
    } else if policy.starts_with("REJECT") {
        theme.danger
    } else {
        theme.info
    };
    div()
        .min_w_0()
        .px_2()
        .py_0p5()
        .rounded(theme.radius)
        .bg(color.opacity(0.1))
        .text_color(color)
        .text_xs()
        .truncate()
        .child(policy.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenclash_core::{ProxyCatalog, ProxyGroup, ProxyGroupBehavior, ProxyNode};

    #[test]
    fn outlet_follows_nested_selection_but_never_invents_a_load_balancer_exit() {
        let catalog = ProxyCatalog::from_group_nodes(
            vec![
                (
                    ProxyGroup {
                        name: "Route".into(),
                        now: "Auto".into(),
                        behavior: ProxyGroupBehavior::Selector,
                        ..Default::default()
                    },
                    vec![],
                ),
                (
                    ProxyGroup {
                        name: "Auto".into(),
                        now: "Actual node".into(),
                        behavior: ProxyGroupBehavior::Automatic { fixed: false },
                        ..Default::default()
                    },
                    vec![ProxyNode {
                        name: "Actual node".into(),
                        provider_name: Some("Provider".into()),
                        ..Default::default()
                    }],
                ),
                (
                    ProxyGroup {
                        name: "Balance".into(),
                        now: "Actual node".into(),
                        behavior: ProxyGroupBehavior::LoadBalance,
                        ..Default::default()
                    },
                    vec![],
                ),
                (
                    ProxyGroup {
                        name: "Cycle".into(),
                        now: "Cycle".into(),
                        behavior: ProxyGroupBehavior::Selector,
                        ..Default::default()
                    },
                    vec![],
                ),
            ],
            5,
        );
        assert_eq!(
            rule_outlet(Some(&catalog), "Route"),
            Some("Actual node".into())
        );
        for policy in ["Balance", "Cycle", "Missing"] {
            assert_eq!(rule_outlet(Some(&catalog), policy), None);
        }
        assert_eq!(rule_outlet(None, "DIRECT"), Some("DIRECT".into()));
        assert_eq!(rule_outlet(None, "Route"), None);
    }

    #[test]
    fn hit_time_accepts_reported_times_and_rejects_missing_or_malformed_values() {
        assert_eq!(rule_hit_time("2026-10-06T15:47:08+08:00"), "15:47:08");
        assert_eq!(rule_hit_time("15:47:08"), "15:47:08");
        for value in ["", "never", "25:47:08"] {
            assert_eq!(rule_hit_time(value), "—");
        }
    }
}
