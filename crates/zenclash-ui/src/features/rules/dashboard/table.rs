use super::super::*;
use crate::components::mint_switch::MintSwitch as Switch;
use gpui_kit::component::button::{ButtonCustomVariant, ButtonVariants};

impl RulesView {
    pub(super) fn render_rule_row(
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
            .and_then(|index| self.confirmed_disabled.get(&index).copied())
            .unwrap_or_else(|| rule.extra.as_ref().is_some_and(|stats| stats.disabled));
        let active = self.selected == Some(position);
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
                    .flex_shrink_0()
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
            .child(
                h_flex()
                    .w_24()
                    .flex_shrink_0()
                    .child(policy_badge(&rule.proxy, theme)),
            )
            .child(
                div().w_20().flex_shrink_0().text_sm().text_right().child(
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
                                self.pending_toggles.contains(&index)
                                    || !self
                                        .context
                                        .is_some_and(|context| context.toggle_supported),
                            )
                            .on_click(cx.listener(move |this, checked, _, cx| {
                                this.request_rule_enabled(index, *checked, cx)
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
                    .flex_shrink_0()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("—"),
            );
        }
        row.child(
            h_flex().w_16().flex_shrink_0().justify_end().child(
                Button::new(("open-rule-details", identity))
                    .ghost()
                    .small()
                    .label(zenclash_i18n::text("redesign.details"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_rule_details(position, window, cx)
                    })),
            ),
        )
        .into_any_element()
    }
}

pub(super) fn rule_columns(theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
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
                .flex_shrink_0()
                .child(zenclash_i18n::text("rules.columns.order")),
        )
        .child(
            div()
                .w(gpui_kit::rems(9.))
                .flex_shrink_0()
                .child(zenclash_i18n::text("rules.columns.kind")),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(zenclash_i18n::text("rules.columns.payload")),
        )
        .child(
            div()
                .w_24()
                .flex_shrink_0()
                .child(zenclash_i18n::text("rules.columns.proxy")),
        )
        .child(
            div()
                .w_20()
                .flex_shrink_0()
                .whitespace_nowrap()
                .text_right()
                .child(zenclash_i18n::text("rules.columns.hits")),
        )
        .child(
            div()
                .w_24()
                .flex_shrink_0()
                .child(zenclash_i18n::text("redesign.status")),
        )
        .child(
            div()
                .w_16()
                .flex_shrink_0()
                .text_right()
                .child(zenclash_i18n::text("redesign.action")),
        )
}

pub(super) fn policy_badge(policy: &str, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
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
