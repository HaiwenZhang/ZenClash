use super::super::*;
use super::table::policy_badge;
use crate::components::mint_switch::MintSwitch as Switch;
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::ButtonVariants;

impl RulesView {
    pub(super) fn open_rule_details(
        &mut self,
        position: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected = Some(position);
        self.details = self
            .projection
            .as_ref()
            .map(|projection| projection.snapshot.clone());
        let owner = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let content = owner
                .update(cx, |view, cx| {
                    let theme = cx.theme().clone();
                    view.render_rule_details(&theme, cx)
                })
                .ok();
            let close_owner = owner.clone();
            dialog
                .bg(cx.theme().group_box)
                .title(zenclash_i18n::text("rules.details.title"))
                .width(window.rem_size() * 34.)
                .margin_top(
                    ((window.viewport_size().height - window.rem_size() * 36.) / 2.)
                        .max(window.rem_size()),
                )
                .when_some(content, |dialog, content| dialog.child(content))
                .on_close(move |_, _, cx| {
                    let _ = close_owner.update(cx, |view, _| view.details = None);
                })
        });
        cx.notify();
    }

    fn render_rule_details(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let Some(snapshot) = &self.details else {
            return empty_state(zenclash_i18n::text("runtime.empty.unavailable"), theme)
                .into_any_element();
        };
        let rules = &snapshot.rules;
        let current = self
            .source
            .as_ref()
            .is_some_and(|source| Arc::ptr_eq(&source.catalog, snapshot));
        let mut inspector = v_flex()
            .id("rule-inspector")
            .test_support()
            .gap_3()
            .min_w_0();
        if let Some((position, rule)) = self
            .selected
            .and_then(|position| rules.get(position).map(|rule| (position, rule)))
        {
            let disabled = rule
                .index
                .and_then(|index| self.confirmed_disabled.get(&index).copied())
                .or_else(|| rule.extra.as_ref().map(|stats| stats.disabled));
            let outlet = rule_outlet(
                self.source
                    .as_ref()
                    .and_then(|source| source.proxies.as_ref()),
                &rule.proxy,
            );
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
                                                        || self.pending_toggles.contains(&index)
                                                        || !self.context.is_some_and(|context| {
                                                            context.toggle_supported
                                                        }),
                                                )
                                                .on_click(cx.listener(
                                                    move |this, checked, _, cx| {
                                                        this.request_rule_enabled(
                                                            index, *checked, cx,
                                                        )
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
                    self.source
                        .as_ref()
                        .and_then(|source| source.config.as_ref()),
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
                                if let Some(rule) = this.details.as_ref().and_then(|snapshot| {
                                    this.selected
                                        .and_then(|position| snapshot.rules.get(position))
                                }) {
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
                                this.details = None;
                                window.close_dialog(cx);
                            })),
                    ),
            )
            .into_any_element()
    }
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
        let Some(group) = catalog
            .groups()
            .iter()
            .find(|group| group.controller_name() == current)
        else {
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
            .any(|candidate| candidate.controller_name() == group.now)
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

#[cfg(test)]
mod tests {
    use super::*;
    use zenclash_core::{ProxyCatalog, ProxyGroup, ProxyGroupBehavior, ProxyNode};

    #[test]
    fn outlet_follows_nested_selection_but_never_invents_a_load_balancer_exit() {
        let catalog = ProxyCatalog::from_group_nodes(
            vec![
                (
                    ProxyGroup::new("Route")
                        .with_current_member("Auto")
                        .with_behavior(ProxyGroupBehavior::Selector),
                    vec![],
                ),
                (
                    ProxyGroup::new("Auto")
                        .with_current_member("Actual node")
                        .with_behavior(ProxyGroupBehavior::Automatic { fixed: false }),
                    vec![ProxyNode {
                        name: "Actual node".into(),
                        provider_name: Some("Provider".into()),
                        ..Default::default()
                    }],
                ),
                (
                    ProxyGroup::new("Balance")
                        .with_current_member("Actual node")
                        .with_behavior(ProxyGroupBehavior::LoadBalance),
                    vec![],
                ),
                (
                    ProxyGroup::new("Cycle")
                        .with_current_member("Cycle")
                        .with_behavior(ProxyGroupBehavior::Selector),
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
