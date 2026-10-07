use super::*;
use crate::components::mint_switch::MintSwitch as Switch;
use gpui_kit::StatefulInteractiveElement;
use gpui_kit::component::Selectable;
use gpui_kit::component::button::{ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement;

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
            .gap_0p5()
            .w_full()
            .min_w_0()
            .h(gpui_kit::rems(32.))
            .child(
                Input::new(&self.rules.filter)
                    .prefix(gpui_kit::component::Icon::new(IconName::Search))
                    .large(),
            )
            .child(filters)
            .when(!compact, |table| table.child(rule_columns(theme).mt_3()));
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
        let mut inspector = panel(theme)
            .id("rule-inspector")
            .test_support()
            .flex_1()
            .flex_basis(gpui_kit::rems(24.))
            .min_w_0()
            .min_h(gpui_kit::rems(34.))
            .gap_3()
            .child(
                section_title("rules.details.title", theme)
                    .text_xl()
                    .pb_1()
                    .border_b_1()
                    .border_color(theme.border),
            );
        if let Some((position, rule)) = self
            .rules
            .selected
            .and_then(|position| rules.get(position).map(|rule| (position, rule)))
        {
            let identity = rule.index.unwrap_or(position);
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
                                                    self.rules.pending.contains(&index)
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
                .child(section_title("rules.details.flow", theme))
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
                .child(section_title("rules.details.statistics", theme))
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
                .child(section_title("rules.details.notes", theme))
                .child(
                    h_flex()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(zenclash_i18n::text("rules.details.order_note")),
                        )
                        .child(
                            Button::new(("rule-overrides", identity))
                                .outline()
                                .small()
                                .h_9()
                                .label(zenclash_i18n::text("rules.details.overrides"))
                                .on_click(|_, window, cx| {
                                    window
                                        .dispatch_action(Box::new(crate::app::NavigateOverride), cx)
                                }),
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
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .items_stretch()
                    .flex_wrap()
                    .child(
                        div()
                            .flex_grow(1.5)
                            .flex_basis(gpui_kit::rems(38.))
                            .min_w_0()
                            .min_h(gpui_kit::rems(34.))
                            .child(table)
                            .overflow_x_scrollbar(),
                    )
                    .when(!compact || self.rules.selected.is_some(), |row| {
                        row.child(inspector)
                    }),
            )
            .child(
                Button::new("rules-statistics")
                    .ghost()
                    .label(zenclash_i18n::text("rules.summary.statistics"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.rules.analytics_expanded = !this.rules.analytics_expanded;
                        cx.notify();
                    })),
            )
            .when(self.rules.analytics_expanded, |view| {
                view.child(
        v_flex()
            .gap_3()
            .child(
                panel(theme).child(
                    h_flex().gap_4().children(
                        [
                            (
                                "profiles.metrics.current",
                                self.profiles
                                    .active_profile()
                                    .map_or_else(|| "—".into(), |profile| profile.name.clone()),
                            ),
                            ("rules.summary.runtime", rules.len().to_string()),
                            (
                                "rules.summary.mode",
                                match &self.data {
                                    RuntimeData::Rules {
                                        config: Some(config),
                                        ..
                                    } => crate::components::sidebar::OutboundMode::from_api(
                                        &config.mode,
                                    )
                                    .label(),
                                    _ => "—".into(),
                                },
                            ),
                            (
                                "rules.summary.statistics",
                                zenclash_i18n::text(
                                    if rules.iter().any(|rule| rule.extra.is_some()) {
                                        "rules.summary.from_core"
                                    } else {
                                        "common.status.unavailable"
                                    },
                                ),
                            ),
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(index, (key, value))| {
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_4()
                                .when(index > 0, |this| {
                                    this.pl_4().border_l_1().border_color(theme.border)
                                })
                                .child(
                                    gpui_kit::component::Icon::new(
                                        [
                                            gpui_kit::assets::IconName::FileText,
                                            gpui_kit::assets::IconName::Layers,
                                            gpui_kit::assets::IconName::List,
                                            gpui_kit::assets::IconName::ChartColumn,
                                        ][index],
                                    )
                                    .size_8()
                                    .text_color(theme.foreground),
                                )
                                .child(
                                    v_flex()
                                        .min_w_0()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(theme.muted_foreground)
                                                .child(zenclash_i18n::text(key)),
                                        )
                                        .child(
                                            div()
                                                .text_xl()
                                                .truncate()
                                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                .child(value),
                                        ),
                                )
                        }),
                    ),
                ),
            )

            .child(
                h_flex()
                    .gap_3()
                    .items_stretch()
                    .flex_wrap()
                    .child(distribution("rules.charts.types", &projection.kinds, theme))
                    .child(policy_distribution(&projection.hits, theme)),
            )
            )
            })
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
            .child(div().w_8().text_sm().child((position + 1).to_string()))
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
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.rules.selected =
                                (this.rules.selected != Some(position)).then_some(position);
                            cx.notify();
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
                div().w_12().child(
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
                ),
            );
        } else {
            row = row.child(
                div()
                    .w_12()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("—"),
            );
        }
        row.into_any_element()
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

fn section_title(key: &str, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .text_lg()
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_color(theme.foreground)
        .child(zenclash_i18n::text(key))
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
        .h_16()
        .px_2()
        .py_2()
        .gap_1()
        .justify_center()
        .items_center()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(if active {
            theme.chart_1.opacity(0.12)
        } else {
            theme.secondary
        })
        .child(
            div()
                .text_base()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(zenclash_i18n::text(key)),
        )
        .child(
            div()
                .w_full()
                .text_center()
                .text_sm()
                .truncate()
                .child(value.to_owned()),
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

fn distribution(
    title: &str,
    values: &[(String, u64)],
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    let maximum = values.iter().map(|value| value.1).max().unwrap_or(1).max(1);
    panel(theme)
        .flex_grow(1.)
        .flex_basis(gpui_kit::rems(36.))
        .min_w_0()
        .min_h(gpui_kit::rems(13.5))
        .child(
            div()
                .text_lg()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(zenclash_i18n::text(title))
                .id("rule-type-distribution-title")
                .tooltip(|window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(zenclash_i18n::text(
                        "runtime.charts.top_five",
                    ))
                    .build(window, cx)
                }),
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
                .child(div().w_32().text_sm().truncate().child(label.clone()))
                .child(
                    Progress::new((gpui_kit::ElementId::from(title.to_owned()), label.clone()))
                        .accessibility_label(label.clone())
                        .value(*count as f32 / maximum as f32 * 100.)
                        .color(theme.chart_3)
                        .h_3()
                        .flex_1(),
                )
                .child(div().w_12().text_sm().text_right().child(count.to_string()))
        }))
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
        .child(
            div()
                .w_12()
                .child(zenclash_i18n::text("rules.columns.enabled")),
        )
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

fn policy_distribution(
    values: &[(String, u64)],
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    let total = values.iter().map(|(_, count)| *count as f64).sum::<f64>();
    panel(theme)
        .flex_1()
        .flex_basis(gpui_kit::rems(24.))
        .min_w_0()
        .min_h(gpui_kit::rems(13.5))
        .child(
            div()
                .text_lg()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(zenclash_i18n::text("rules.charts.hits"))
                .id("rule-policy-distribution-title")
                .tooltip(|window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(zenclash_i18n::text(
                        "rules.charts.reported_hits",
                    ))
                    .build(window, cx)
                }),
        )
        .when(total > 0., |this| {
            this.child(
                h_flex()
                    .w_full()
                    .h_7()
                    .rounded(theme.radius)
                    .overflow_hidden()
                    .children(
                        values
                            .iter()
                            .filter(|(_, count)| *count > 0)
                            .enumerate()
                            .map(|(index, (policy, count))| {
                                div()
                                    .h_full()
                                    .flex_grow(*count as f32)
                                    .flex_basis(gpui_kit::px(0.))
                                    .when(index == 0, |segment| segment.rounded_l(theme.radius))
                                    .when(
                                        index + 1
                                            == values
                                                .iter()
                                                .filter(|(_, count)| *count > 0)
                                                .count(),
                                        |segment| segment.rounded_r(theme.radius),
                                    )
                                    .when(index > 0, |segment| {
                                        segment.border_l_1().border_color(theme.group_box)
                                    })
                                    .bg(if policy.eq_ignore_ascii_case("DIRECT") {
                                        theme.chart_3
                                    } else if policy.starts_with("REJECT") {
                                        theme.danger
                                    } else {
                                        theme.chart_1
                                    })
                            }),
                    ),
            )
        })
        .children(values.iter().map(|(policy, count)| {
            h_flex()
                .gap_3()
                .justify_between()
                .child(
                    h_flex()
                        .gap_2()
                        .min_w_0()
                        .child(div().size_4().flex_shrink_0().rounded_full().bg(
                            if policy.eq_ignore_ascii_case("DIRECT") {
                                theme.chart_3
                            } else if policy.starts_with("REJECT") {
                                theme.danger
                            } else {
                                theme.chart_1
                            },
                        ))
                        .child(div().text_sm().truncate().child(policy.clone())),
                )
                .child(div().text_sm().child(if total > 0. {
                    format!("{:.1}%", *count as f64 / total * 100.)
                } else {
                    "—".into()
                }))
        }))
        .when(values.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("—"),
            )
        })
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
