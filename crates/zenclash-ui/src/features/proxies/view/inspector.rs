use gpui_kit::base::StyledExt;
use gpui_kit::component::{ActiveTheme as _, WindowExt, button::ButtonVariants};
use gpui_kit::{InteractiveElement, StatefulInteractiveElement, TestSupportExt};

use super::super::{
    Button, Context, Disableable, FluentBuilder, Icon, IconName, IntoElement, ParentElement,
    ProxiesPage, ProxyCatalog, ProxyGroup, ProxyGroupBehavior, ProxyNodeId, Sizable, Styled, div,
    group_has_unique_current, h_flex, v_flex,
};
use super::{group_behavior_label, proxy_element_id};

impl ProxiesPage {
    pub(super) fn open_node_details(
        &self,
        group_name: String,
        id: ProxyNodeId,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let content = owner
                .update(cx, |page, cx| {
                    let theme = cx.theme().clone();
                    let catalog = page.catalog.as_ref()?;
                    let group = catalog
                        .groups()
                        .iter()
                        .find(|group| group.controller_name() == group_name)?;
                    catalog.node(&id)?;
                    Some(page.render_node_details(catalog, group, &id, &theme, cx))
                })
                .ok()
                .flatten();
            let missing = content.is_none();
            dialog
                .title(zenclash_i18n::text("unified.proxies.details"))
                .width(window.rem_size() * 42.)
                .when_some(content, |dialog, content| dialog.child(content))
                .when(missing, |dialog| {
                    dialog.child(zenclash_i18n::text("home.proxy.no_node"))
                })
        });
    }

    fn render_node_details(
        &self,
        catalog: &ProxyCatalog,
        group: &ProxyGroup,
        id: &ProxyNodeId,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let id = Some(id.clone());
        let node = id.as_ref().and_then(|id| catalog.node(id));
        let inspector = v_flex()
            .id("proxy-node-inspector")
            .test_support()
            .gap_4()
            .child(
                h_flex().justify_end().gap_2().when(
                    group_has_unique_current(&group.behavior)
                        && id
                            .as_ref()
                            .is_some_and(|id| group.now == id.controller_name()),
                    |row| {
                        row.child(
                            h_flex()
                                .gap_2()
                                .text_xs()
                                .text_color(theme.primary)
                                .child(
                                    div()
                                        .size_2()
                                        .rounded_full()
                                        .bg(crate::foundation::design::online_indicator(theme)),
                                )
                                .child(zenclash_i18n::text("proxies.design.selected")),
                        )
                    },
                ),
            )
            .when_some(node.zip(id.as_ref()), |this, (node, id)| {
                let node_name = node.name.clone();
                let points = node
                    .history
                    .iter()
                    .rev()
                    .take(10)
                    .rev()
                    .filter(|point| point.delay > 0)
                    .map(|point| (delay_time(&point.time, "%H:%M"), point.delay))
                    .collect::<Vec<_>>();
                let has_history = !points.is_empty();
                let peak = f64::from(points.iter().map(|point| point.1).max().unwrap_or(0));
                let chart_ceiling = (peak / 30.).ceil().max(1.) * 30.;
                let capabilities = node.capabilities().collect::<Vec<_>>().join(" · ");
                let (delay_color, delay_background) = match node.latest_delay() {
                    Some(0) => (theme.danger, theme.danger.opacity(0.12)),
                    Some(delay) if delay >= 500 => (theme.warning, theme.warning.opacity(0.12)),
                    Some(_) => (theme.primary, theme.list_active),
                    None => (theme.muted_foreground, theme.muted),
                };
                this.child(
                    h_flex()
                        .gap_3()
                        .child(
                            h_flex()
                                .size_16()
                                .border_1()
                                .border_color(
                                    crate::foundation::design::brand_accent(theme).opacity(0.3),
                                )
                                .flex_shrink_0()
                                .justify_center()
                                .rounded_full()
                                .bg(crate::foundation::design::brand_accent(theme).opacity(0.12))
                                .text_color(theme.primary)
                                .child(Icon::new(gpui_kit::assets::IconName::Server).size_8()),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(
                                    div()
                                        .id(proxy_element_id(
                                            "inspector-node-name",
                                            group.controller_name(),
                                            id,
                                        ))
                                        .test_support()
                                        .text_2xl()
                                        .w_full()
                                        .min_w_0()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .overflow_hidden()
                                        .tooltip(move |window, cx| {
                                            gpui_kit::component::tooltip::Tooltip::new(
                                                node_name.clone(),
                                            )
                                            .build(window, cx)
                                        })
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .child(node.name.clone()),
                                )
                                .child(div().text_sm().text_color(theme.muted_foreground).child(
                                    if capabilities.is_empty() {
                                        node.kind.clone()
                                    } else {
                                        format!("{} · {capabilities}", node.kind)
                                    },
                                )),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .px_3()
                                .py_1()
                                .h_12()
                                .flex()
                                .items_center()
                                .rounded(theme.radius)
                                .bg(delay_background)
                                .text_2xl()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(delay_color)
                                .child(node.latest_delay().map_or_else(
                                    || zenclash_i18n::text("home.proxy.untested"),
                                    |delay| {
                                        if delay == 0 {
                                            zenclash_i18n::text("proxies.status.timeout")
                                        } else {
                                            format!("{delay} ms")
                                        }
                                    },
                                )),
                        ),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .flex_wrap()
                        .child(
                            div()
                                .text_lg()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child(zenclash_i18n::text("proxies.design.latency_history")),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(zenclash_i18n::text("proxies.design.history_window")),
                        ),
                )
                .when(has_history, |this| {
                    this.child(
                        // Both plots share identical axes so the area and point markers align.
                        // The area is passive; the line owns hover and tooltip interaction.
                        div()
                            .relative()
                            .h_40()
                            .w_full()
                            .child(
                                gpui_kit::component::chart::AreaChart::new(points.clone())
                                    .id(proxy_element_id(
                                        "delay-history-fill",
                                        group.controller_name(),
                                        id,
                                    ))
                                    .interactive(false)
                                    .x(|point| point.0.clone())
                                    .y(|point| f64::from(point.1))
                                    .y_domain(0., chart_ceiling)
                                    .y_padding(0., 0.)
                                    .stroke(theme.chart_1)
                                    .fill(theme.chart_1.opacity(0.10))
                                    .linear()
                                    .grid(false)
                                    .y_tick_count(4)
                                    .x_tick_count(6)
                                    .y_axis(true)
                                    .y_tick_format(|value| format!("{value:.0}")),
                            )
                            .child(
                                div().absolute().inset_0().child(
                                    gpui_kit::component::chart::LineChart::new(points)
                                        .id(proxy_element_id(
                                            "delay-history",
                                            group.controller_name(),
                                            id,
                                        ))
                                        .x(|point| point.0.clone())
                                        .y(|point| f64::from(point.1))
                                        .y_domain(0., chart_ceiling)
                                        .y_padding(0., 0.)
                                        .stroke(theme.chart_1)
                                        .linear()
                                        .dot()
                                        .grid_columns(6)
                                        .x_tick_count(6)
                                        .grid_dashed(false)
                                        .y_tick_count(4)
                                        .y_axis(true)
                                        .y_tick_format(|value| format!("{value:.0}")),
                                ),
                            ),
                    )
                })
                .when(!has_history, |this| {
                    this.child(
                        div()
                            .h_40()
                            .w_full()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("home.proxy.untested")),
                    )
                })
                .child(
                    v_flex()
                        .gap_0()
                        .child(inspector_row(
                            "proxies.design.group",
                            group.name.clone(),
                            theme,
                        ))
                        .child(inspector_row(
                            "proxies.design.source",
                            id.provider()
                                .or_else(|| {
                                    self.active_profile.as_ref().map(|(name, _)| name.as_str())
                                })
                                .unwrap_or("—")
                                .to_owned(),
                            theme,
                        ))
                        .child(inspector_row(
                            "proxies.design.udp",
                            inspector_badge(
                                zenclash_i18n::text(if node.udp {
                                    "proxies.design.supported"
                                } else {
                                    "proxies.design.unsupported"
                                }),
                                node.udp,
                                theme,
                            ),
                            theme,
                        ))
                        .child(inspector_row(
                            "proxies.design.last_test",
                            node.history
                                .last()
                                .map(|sample| delay_time(&sample.time, "%H:%M:%S"))
                                .filter(|time| !time.is_empty())
                                .unwrap_or_else(|| "—".into()),
                            theme,
                        )),
                )
                .child(
                    Button::new(proxy_element_id(
                        "inspector-test",
                        group.controller_name(),
                        id,
                    ))
                    .icon(IconName::Play)
                    .label(zenclash_i18n::text("proxies.actions.test"))
                    .outline()
                    .small()
                    .h_10()
                    .disabled(
                        self.proxy_selection_blocked(group.controller_name())
                            || self
                                .testing
                                .get(group.controller_name())
                                .is_some_and(|nodes| nodes.contains(id)),
                    )
                    .on_click(cx.listener({
                        let group_name = group.controller_name().to_owned();
                        let node_id = id.clone();
                        let url = group.test_url.clone();
                        move |this, _, _, cx| {
                            this.test_proxy(group_name.clone(), node_id.clone(), url.clone(), cx)
                        }
                    })),
                )
            })
            .when(node.is_none(), |this| {
                this.child(div().text_xs().text_color(theme.muted_foreground).child(
                    zenclash_i18n::text(if group_has_unique_current(&group.behavior) {
                        "home.proxy.no_node"
                    } else {
                        "home.proxy.load_balance_description"
                    }),
                ))
            });
        v_flex()
            .w_full()
            .max_w_full()
            .flex_shrink_0()
            .gap_2()
            .child(inspector)
            .when(
                matches!(group.behavior, ProxyGroupBehavior::Automatic { .. }),
                |column| column.child(self.render_auto_selection(group, theme, cx)),
            )
    }

    fn render_auto_selection(
        &self,
        group: &ProxyGroup,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let automatic = matches!(group.behavior, ProxyGroupBehavior::Automatic { .. });
        let name = group.controller_name().to_owned();
        let url = group.test_url.clone();
        let restoring_name = group.controller_name().to_owned();
        let pending = self.measuring_and_restoring_auto.as_deref() == Some(group.controller_name());
        v_flex()
            .p_3()
            .gap_2()
            .min_h(gpui_kit::rems(13.5))
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_lg()
                            .font_bold()
                            .child(zenclash_i18n::text("proxies.design.automatic_title")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .px_2()
                            .py_1()
                            .border_1()
                            .border_color(theme.border)
                            .rounded(theme.radius)
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("proxies.design.automatic_only")),
                    ),
            )
            .child(inspector_row(
                "proxies.design.scope",
                zenclash_i18n::text("proxies.design.automatic_scope"),
                theme,
            ))
            .child(inspector_row(
                "proxies.design.state",
                inspector_badge(
                    group_behavior_label(&group.behavior),
                    matches!(
                        group.behavior,
                        ProxyGroupBehavior::Automatic { fixed: false }
                    ),
                    theme,
                ),
                theme,
            ))
            .child(inspector_row(
                "proxies.design.explanation",
                zenclash_i18n::text("proxies.design.automatic_description"),
                theme,
            ))
            .child(
                Button::new((
                    gpui_kit::ElementId::from("measure-restore-auto"),
                    group.controller_name().to_owned(),
                ))
                .icon(crate::foundation::assets::AppIcon::RefreshCw)
                .label(zenclash_i18n::text("proxies.actions.measure_restore_auto"))
                .h(gpui_kit::rems(2.5))
                .outline()
                .small()
                .w_full()
                .loading(pending)
                .disabled(!automatic || self.operation_pending())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.measure_group_and_restore_auto(name.clone(), url.clone(), cx)
                })),
            )
            .when(
                matches!(
                    group.behavior,
                    ProxyGroupBehavior::Automatic { fixed: true }
                ),
                |view| {
                    view.child(
                        Button::new((
                            gpui_kit::ElementId::from("restore-auto"),
                            group.controller_name().to_owned(),
                        ))
                        .label(zenclash_i18n::text("proxies.actions.restore_auto"))
                        .small()
                        .ghost()
                        .disabled(self.operation_pending())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.restore_auto(restoring_name.clone(), cx)
                        })),
                    )
                },
            )
    }
}

fn inspector_row(
    key: &str,
    value: impl IntoElement,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    h_flex()
        .py_1()
        .gap_3()
        .border_b_1()
        .border_color(theme.border)
        .text_sm()
        .child(
            div()
                .w(gpui_kit::rems(6.))
                .flex_shrink_0()
                .text_color(theme.muted_foreground)
                .child(zenclash_i18n::text(key)),
        )
        .child(h_flex().flex_1().min_w_0().child(value))
}

fn inspector_badge(
    label: String,
    active: bool,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    div()
        .flex()
        .items_center()
        .min_h_6()
        .flex_shrink_0()
        .px_2()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(if active {
            theme.list_active
        } else {
            theme.muted
        })
        .text_color(if active {
            theme.primary
        } else {
            theme.muted_foreground
        })
        .child(label)
}

fn delay_time(value: &str, format: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map_or_else(|_| "—".into(), |time| time.format(format).to_string())
}
