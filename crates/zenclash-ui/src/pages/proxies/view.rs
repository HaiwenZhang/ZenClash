use gpui_kit::base::StyledExt;
use gpui_kit::component::{
    Selectable, WindowExt,
    button::ButtonVariants,
    menu::{DropdownMenu, PopupMenuItem},
};
use gpui_kit::{InteractiveElement, StatefulInteractiveElement, TestSupportExt};

use gpui_kit::component::ActiveTheme as _;

use super::{
    Button, Context, Disableable, FluentBuilder, Icon, IconName, IntoElement, ParentElement,
    ProxiesPage, ProxyCatalog, ProxyGroup, ProxyGroupBehavior, ProxyNode, ProxyNodeId, ProxyPage,
    Sizable, Styled, Switch, div, group_allows_manual_selection, group_has_unique_current, h_flex,
    v_flex,
};

impl ProxiesPage {
    pub(super) fn render_summary(
        &self,
        catalog: &ProxyCatalog,
        theme: &gpui_kit::component::Theme,
    ) -> gpui_kit::AnyElement {
        let counts = self.node_summary.counts();
        h_flex()
            .id("proxy-summary")
            .test_support()
            .w_full()
            .gap_5()
            .px_4()
            .py_3()
            .min_h_16()
            .flex_wrap()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .child(
                h_flex()
                    .w(gpui_kit::rems(17.))
                    .flex_shrink_0()
                    .min_w_0()
                    .gap_3()
                    .child(
                        h_flex()
                            .size_8()
                            .justify_center()
                            .rounded(theme.radius)
                            .bg(theme.muted)
                            .child(Icon::new(IconName::File).size_6()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_base()
                            .font_bold()
                            .truncate()
                            .child(
                                self.active_profile
                                    .as_ref()
                                    .map_or("—", |(name, _)| name.as_str())
                                    .to_owned(),
                            ),
                    )
                    .when_some(self.active_profile.as_ref(), |row, (_, remote)| {
                        row.child(
                            div()
                                .text_xs()
                                .px_2()
                                .py_1()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.chart_3.opacity(0.3))
                                .bg(theme.chart_3.opacity(0.12))
                                .text_color(theme.primary)
                                .child(zenclash_i18n::text(if *remote {
                                    "profiles.source.remote"
                                } else {
                                    "profiles.source.local"
                                })),
                        )
                    }),
            )
            .children(
                [
                    ("proxies.summary.group_count", catalog.groups().len()),
                    ("proxies.summary.node_count", counts.iter().sum()),
                ]
                .into_iter()
                .map(|(key, count)| {
                    div()
                        .flex_1()
                        .pl_5()
                        .border_l_1()
                        .border_color(theme.border)
                        .text_sm()
                        .child(zenclash_i18n::text_with(
                            key,
                            &[("count", count.to_string())],
                        ))
                }),
            )
            .children(
                [
                    ("proxies.summary.available", counts[0], theme.chart_3),
                    ("proxies.summary.unavailable", counts[1], theme.chart_1),
                    ("proxies.design.untested", counts[2], theme.muted_foreground),
                ]
                .into_iter()
                .map(|(key, count, color)| {
                    h_flex()
                        .flex_1()
                        .gap_2()
                        .text_sm()
                        .child(div().size_2p5().rounded_full().bg(color))
                        .child(zenclash_i18n::text(key))
                        .child(count.to_string())
                }),
            )
            .into_any_element()
    }

    pub(super) fn render_workspace(
        &self,
        catalog: &ProxyCatalog,
        page: ProxyPage,
        compact: bool,
        columns: u16,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let visible = &self.visible_group_indices[page.start..page.end];
        let selected = super::presentation::selected_group_index(catalog, visible, &self.expanded);
        let Some(selected) = selected else {
            return div().into_any_element();
        };
        let group = &catalog.groups()[selected];
        let navigation = v_flex()
            .id("proxy-group-navigation")
            .test_support()
            .w(gpui_kit::rems(17.))
            .min_w_0()
            .max_w_full()
            .flex_shrink_0()
            .gap_2()
            .min_h_0()
            .p_3()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .child(
                div()
                    .pb_2()
                    .text_lg()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(format!(
                        "{} ({})",
                        zenclash_i18n::text("proxies.header.title"),
                        self.visible_group_indices.len()
                    )),
            )
            .children(visible.iter().map(|&index| {
                let item = &catalog.groups()[index];
                let name = item.name.clone();
                Button::new((gpui_kit::ElementId::from("toggle-group"), name.clone()))
                    .accessibility_label(item.name.clone())
                    .tooltip(item.name.clone())
                    .ghost()
                    .child(
                        group_icon(&item.behavior)
                            .size_8()
                            .when(index == selected, |icon| icon.text_color(theme.primary)),
                    )
                    .when(index == selected, |this| this.bg(theme.list_active))
                    .min_h_16()
                    .child(
                        v_flex()
                            .w_full()
                            .min_w_0()
                            .text_left()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .truncate()
                                    .child(item.name.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child(item.now.clone()),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .px_2()
                            .py_1()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border)
                            .child(group_behavior_label(&item.behavior)),
                    )
                    .selected(index == selected)
                    .w_full()
                    .min_w_0()
                    .justify_start()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.expanded.contains(&name) {
                            this.toggle_group(&name, cx);
                        }
                    }))
            }))
            .child(div().flex_1())
            .when(page.count > 1, |navigation| {
                navigation.child(self.render_group_pagination(page, theme, cx))
            })
            .child(
                div()
                    .pt_3()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(self.render_group_visibility(cx)),
            );
        h_flex()
            .items_stretch()
            .gap_3()
            .when(!compact, |row| row.child(navigation))
            .child(
                v_flex()
                    .flex_1()
                    .flex_basis(gpui_kit::rems(30.))
                    .min_w_0()
                    .max_w_full()
                    .gap_3()
                    .when(compact, |column| {
                        let owner = cx.entity().downgrade();
                        let choices = catalog
                            .groups()
                            .iter()
                            .enumerate()
                            .filter(|(index, _)| self.visible_group_indices.contains(index))
                            .map(|(_, group)| group.name.clone())
                            .collect::<Vec<_>>();
                        column.child(
                            Button::new("compact-proxy-group")
                                .label(group.name.clone())
                                .outline()
                                .dropdown_caret(true)
                                .dropdown_menu(move |mut menu, _, _| {
                                    for name in &choices {
                                        let name = name.clone();
                                        let owner = owner.clone();
                                        menu =
                                            menu.item(PopupMenuItem::new(name.clone()).on_click(
                                                move |_, _, cx| {
                                                    let _ = owner.update(cx, |page, cx| {
                                                        if !page.expanded.contains(&name) {
                                                            page.toggle_group(&name, cx);
                                                        }
                                                    });
                                                },
                                            ));
                                    }
                                    menu
                                }),
                        )
                    })
                    .child(self.render_group(catalog, group, columns, theme, cx)),
            )
            .when(self.show_node_details, |row| {
                row.child(self.render_current_node(catalog, group, theme, cx))
            })
            .into_any_element()
    }

    fn render_current_node(
        &self,
        catalog: &ProxyCatalog,
        group: &ProxyGroup,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let id = group_has_unique_current(&group.behavior)
            .then(|| self.group_orders.current_node(group))
            .flatten();
        let node = id.as_ref().and_then(|id| catalog.node(id));
        let inspector = v_flex()
            .id("proxy-node-inspector")
            .test_support()
            .min_h(gpui_kit::rems(35.))
            .p_4()
            .gap_4()
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
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(zenclash_i18n::text("home.proxy.title")),
                    )
                    .when(id.is_some(), |row| {
                        row.child(
                            h_flex()
                                .gap_2()
                                .text_xs()
                                .text_color(theme.primary)
                                .child(div().size_2().rounded_full().bg(theme.chart_3))
                                .child(zenclash_i18n::text("proxies.design.selected")),
                        )
                    }),
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
                                .border_color(theme.chart_3.opacity(0.3))
                                .flex_shrink_0()
                                .justify_center()
                                .rounded_full()
                                .bg(theme.chart_3.opacity(0.12))
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
                                            &group.name,
                                            id,
                                        ))
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
                                    .id(proxy_element_id("delay-history-fill", &group.name, id))
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
                                        .id(proxy_element_id("delay-history", &group.name, id))
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
                    Button::new(proxy_element_id("inspector-test", &group.name, id))
                        .icon(IconName::Play)
                        .label(zenclash_i18n::text("proxies.actions.test_current"))
                        .outline()
                        .small()
                        .h_10()
                        .disabled(
                            self.proxy_selection_blocked(&group.name)
                                || self
                                    .testing
                                    .get(&group.name)
                                    .is_some_and(|nodes| nodes.contains(id)),
                        )
                        .on_click(cx.listener({
                            let group_name = group.name.clone();
                            let node_id = id.clone();
                            let url = group.test_url.clone();
                            move |this, _, _, cx| {
                                this.test_proxy(
                                    group_name.clone(),
                                    node_id.clone(),
                                    url.clone(),
                                    cx,
                                )
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
            .w(gpui_kit::rems(27.))
            .max_w_full()
            .flex_shrink_0()
            .gap_2()
            .child(inspector)
            .child(self.render_auto_selection(group, theme, cx))
    }

    fn render_auto_selection(
        &self,
        group: &ProxyGroup,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let automatic = matches!(group.behavior, ProxyGroupBehavior::Automatic { .. });
        let name = group.name.clone();
        let url = group.test_url.clone();
        let restoring_name = group.name.clone();
        let pending = self.measuring_and_restoring_auto.as_deref() == Some(&group.name);
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
                    group.name.clone(),
                ))
                .icon(crate::assets::AppIcon::RefreshCw)
                .label(zenclash_i18n::text("proxies.actions.measure_restore_auto"))
                .h(gpui_kit::px(40.))
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
                            group.name.clone(),
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

    pub(super) fn render_group_pagination(
        &self,
        page: ProxyPage,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .py_2()
            .items_center()
            .justify_between()
            .gap_2()
            .flex_wrap()
            .child(div().text_xs().text_color(theme.muted_foreground).child(
                zenclash_i18n::text_with(
                    "proxies.pagination.groups_summary",
                    &[
                        ("current", (page.index + 1).to_string()),
                        ("total", page.count.to_string()),
                        ("first", (page.start + 1).to_string()),
                        ("last", page.end.to_string()),
                        ("count", self.visible_group_indices.len().to_string()),
                    ],
                ),
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("previous-proxy-group-page")
                            .icon(IconName::ChevronLeft)
                            .label(zenclash_i18n::text("common.actions.previous_page"))
                            .small()
                            .outline()
                            .disabled(page.index == 0)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_catalog_page(page.index.saturating_sub(1), cx);
                            })),
                    )
                    .child(
                        Button::new("next-proxy-group-page")
                            .icon(IconName::ChevronRight)
                            .label(zenclash_i18n::text("common.actions.next_page"))
                            .small()
                            .outline()
                            .disabled(page.index + 1 >= page.count)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_catalog_page(page.index + 1, cx);
                            })),
                    ),
            )
    }

    pub(super) fn render_header(
        &self,
        _theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.catalog.as_ref().and_then(|catalog| {
            let page = super::group_page(self.visible_group_indices.len(), self.group_page_index);
            super::presentation::selected_group_index(
                catalog,
                &self.visible_group_indices[page.start..page.end],
                &self.expanded,
            )
            .map(|index| catalog.groups()[index].name.clone())
        });
        let testing = selected
            .as_ref()
            .is_some_and(|name| self.active_testing_groups.contains_key(name));
        v_flex()
            .px_8()
            .pt_4()
            .pb_3()
            .gap_3()
            .child(
                h_flex().gap_1().children(
                    [
                        crate::components::sidebar::OutboundMode::Rule,
                        crate::components::sidebar::OutboundMode::Global,
                        crate::components::sidebar::OutboundMode::Direct,
                    ]
                    .into_iter()
                    .map(|mode| {
                        Button::new((gpui_kit::ElementId::from("proxy-mode"), mode.api_value()))
                            .label(mode.label())
                            .outline()
                            .selected(self.outbound_mode == mode.api_value())
                            .disabled(self.operation_pending())
                            .on_click(move |_, window, cx| {
                                use crate::components::sidebar::OutboundMode;
                                match mode {
                                    OutboundMode::Rule => window
                                        .dispatch_action(Box::new(crate::app::SetRuleMode), cx),
                                    OutboundMode::Global => window
                                        .dispatch_action(Box::new(crate::app::SetGlobalMode), cx),
                                    OutboundMode::Direct => window
                                        .dispatch_action(Box::new(crate::app::SetDirectMode), cx),
                                }
                            })
                    }),
                ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .items_end()
                    .gap_3()
                    .child(crate::components::workspace::title(
                        crate::pages::Page::Proxies,
                        cx,
                    ))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("refresh-proxies")
                                    .icon(crate::assets::AppIcon::RefreshCw)
                                    .label(zenclash_i18n::text("proxies.actions.refresh"))
                                    .small()
                                    .h_10()
                                    .outline()
                                    .loading(self.loading)
                                    .disabled(self.operation_pending())
                                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                            )
                            .child(
                                Button::new((
                                    gpui_kit::ElementId::from("test-group"),
                                    selected.clone().unwrap_or_default(),
                                ))
                                .icon(IconName::Play)
                                .label(zenclash_i18n::text(if testing {
                                    "proxies.actions.testing"
                                } else {
                                    "proxies.actions.test_selected_group"
                                }))
                                .outline()
                                .small()
                                .h_10()
                                .loading(testing)
                                .disabled(selected.is_none() || self.operation_pending())
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        if let Some(name) = &selected {
                                            this.test_group(name, cx);
                                        }
                                    },
                                )),
                            ),
                    ),
            )
    }

    fn render_node_filters(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let owner = cx.entity().downgrade();
        let sort_by_latency = self.sort_by_latency;
        h_flex()
            .gap_2()
            .flex_wrap()
            .child(
                Button::new("proxy-details")
                    .label(zenclash_i18n::text("home.proxy.title"))
                    .small()
                    .outline()
                    .selected(self.show_node_details)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_node_details = !this.show_node_details;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("sort-proxies-by-latency")
                    .icon(gpui_kit::assets::IconName::ArrowDownUp)
                    .label(zenclash_i18n::text(if sort_by_latency {
                        "proxies.actions.sort_menu"
                    } else {
                        "proxies.actions.original_order"
                    }))
                    .h(gpui_kit::px(44.))
                    .small()
                    .outline()
                    .dropdown_caret(true)
                    .dropdown_menu(move |mut menu, _, _| {
                        for (sort, key) in [
                            (false, "proxies.actions.original_order"),
                            (true, "proxies.actions.sort_latency"),
                        ] {
                            let owner = owner.clone();
                            menu = menu.item(
                                PopupMenuItem::new(zenclash_i18n::text(key))
                                    .checked(sort_by_latency == sort)
                                    .on_click(move |_, _, cx| {
                                        let _ = owner.update(cx, |page, cx| {
                                            page.sort_by_latency = sort;
                                            page.prepare_search(cx);
                                            cx.notify();
                                        });
                                    }),
                            );
                        }
                        menu
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Switch::new("hide-unavailable-proxies")
                            .accessibility_label(zenclash_i18n::text(
                                "proxies.actions.hide_unavailable",
                            ))
                            .checked(self.hide_unavailable)
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.hide_unavailable = *checked;
                                this.prepare_search(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(zenclash_i18n::text("proxies.actions.hide_unavailable")),
                    ),
            )
    }

    pub(super) fn render_group_visibility(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let theme = cx.theme().clone();
        let show_hidden = self.show_hidden;
        let loading = self.loading;
        let operation_pending = self.operation_pending();
        h_flex().gap_2().child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(zenclash_i18n::text("proxies.actions.show_hidden")),
                )
                .child(
                    Switch::new("proxies-show-hidden")
                        .accessibility_label(zenclash_i18n::text("proxies.actions.show_hidden"))
                        .checked(show_hidden)
                        .disabled(loading || operation_pending)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.set_show_hidden(*checked, cx);
                        })),
                ),
        )
    }

    pub(super) fn render_group(
        &self,
        catalog: &ProxyCatalog,
        group: &ProxyGroup,
        columns: u16,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let expanded = true;
        v_flex()
            .min_h_0()
            .id("proxy-node-panel")
            .test_support()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .overflow_hidden()
            .child(
                h_flex()
                    .px_3()
                    .py_3()
                    .justify_between()
                    .gap_2()
                    .child(div().text_lg().font_bold().child(group.name.clone()))
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(group_behavior_label(&group.behavior)),
                    ),
            )
            .when(expanded, |this| {
                let nodes = self.displayed_nodes(catalog, group);

                let focus_handles = nodes
                    .iter()
                    .filter_map(|&index| {
                        self.node_focus
                            .get(&(group.name.clone(), group.all[index].clone()))
                            .cloned()
                    })
                    .collect::<Vec<_>>();
                this.child(
                    v_flex()
                        .flex_1()
                        .child(
                            h_flex()
                                .px_3()
                                .pt_3()
                                .gap_2()
                                .flex_wrap()
                                .when_some(self.search_input.as_ref(), |this, input| {
                                    this.child(
                                        div().flex_1().min_w(gpui_kit::rems(10.)).child(
                                            gpui_kit::component::input::Input::new(input)
                                                .id("proxy-node-search")
                                                .large()
                                                .prefix(Icon::new(IconName::Search)),
                                        ),
                                    )
                                })
                                .child(self.render_node_filters(cx)),
                        )
                        .child(
                            div()
                                .grid()
                                .grid_cols(columns)
                                .on_key_down(move |event, window, cx| {
                                    let Some(index) = focus_handles
                                        .iter()
                                        .position(|handle| handle.is_focused(window))
                                    else {
                                        return;
                                    };
                                    let step = match event.keystroke.key.as_str() {
                                        "left" => -1,
                                        "right" => 1,
                                        "up" => -isize::try_from(columns).unwrap_or(1),
                                        "down" => isize::try_from(columns).unwrap_or(1),
                                        _ => return,
                                    };
                                    let target = index
                                        .saturating_add_signed(step)
                                        .min(focus_handles.len().saturating_sub(1));
                                    focus_handles[target].focus(window, cx);
                                    cx.stop_propagation();
                                })
                                .p_3()
                                .gap_3()
                                .when(!self.search_query.is_empty() && nodes.is_empty(), |body| {
                                    body.child(
                                        div()
                                            .id("proxy-search-feedback")
                                            .test_support()
                                            .pt_4()
                                            .text_sm()
                                            .text_color(theme.muted_foreground)
                                            .child(zenclash_i18n::text(
                                                if self.search_projection.is_none() {
                                                    "common.actions.loading"
                                                } else {
                                                    "proxies.design.no_matches"
                                                },
                                            )),
                                    )
                                })
                                .children(nodes.iter().filter_map(|&index| {
                                    let id = &group.all[index];
                                    catalog
                                        .node(id)
                                        .map(|node| self.render_proxy(group, id, node, theme, cx))
                                })),
                        ),
                )
            })
            .into_any_element()
    }

    pub(super) fn render_proxy(
        &self,

        group: &ProxyGroup,
        id: &ProxyNodeId,
        proxy: &ProxyNode,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let selected =
            group_has_unique_current(&group.behavior) && group.now == id.controller_name();
        let selectable = group_allows_manual_selection(&group.behavior);
        let testing = self
            .testing
            .get(&group.name)
            .is_some_and(|nodes| nodes.contains(id));
        let switching = self
            .switching
            .proxy_pending(&group.name, id.controller_name());
        let selection_blocked = self.proxy_selection_blocked(&group.name);
        let group_name = group.name.clone();
        let proxy_name = id.controller_name().to_owned();
        let delay_group = group.name.clone();
        let delay_proxy = id.clone();
        let test_url = group.test_url.clone();
        let delay = proxy.latest_delay();
        let failure = self.test_failures.get(id);
        let delay_color = match (failure, delay) {
            (Some(_), _) => theme.danger,
            (None, Some(0)) => theme.danger,
            (None, Some(value)) if value < 500 => theme.success,
            (None, Some(_)) => theme.warning,
            (None, None) => theme.muted_foreground,
        };
        let delay_text = if testing {
            zenclash_i18n::text("proxies.status.testing")
        } else if let Some(failure) = failure {
            failure.label()
        } else {
            match delay {
                Some(0) => zenclash_i18n::text("proxies.status.timeout"),
                Some(value) => format!("{value} ms"),
                None => zenclash_i18n::text("proxies.design.untested"),
            }
        };
        v_flex()
            .relative()
            .flex_basis(gpui_kit::rems(12.))
            .flex_grow(1.)
            .min_w_0()
            .rounded(theme.radius)
            .border_1()
            .border_color(if selected {
                theme.primary
            } else {
                theme.border
            })
            .bg(if selected {
                theme.list_active
            } else {
                theme.group_box
            })
            .child(
                gpui_kit::base::Button::new(proxy_element_id("select-proxy", &group.name, id))
                    .when_some(
                        self.node_focus.get(&(group.name.clone(), id.clone())),
                        |button, focus| button.track_focus(focus),
                    )
                    .accessibility_label(proxy.name.clone())
                    .rounded(theme.radius)
                    .hover(|style| style.bg(theme.list_hover))
                    .focus(|style| style.border_2().border_color(theme.ring))
                    .styles(|styles| styles.disabled(|style| style.opacity(0.6)))
                    .w_full()
                    .h_auto()
                    .p_4()
                    .selected(selected)
                    .disabled(selection_blocked || !selectable)
                    .child(
                        v_flex()
                            .w_full()
                            .min_w_0()
                            .gap_3()
                            .text_left()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .justify_between()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_ellipsis()
                                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                            .child(proxy.name.clone()),
                                    )
                                    .when(selected, |row| {
                                        row.child(
                                            div()
                                                .id(proxy_element_id(
                                                    "current-proxy",
                                                    &group.name,
                                                    id,
                                                ))
                                                .test_support()
                                                .child(Icon::new(IconName::Check).size_4()),
                                        )
                                    }),
                            )
                            .child(div().text_lg().text_color(delay_color).child(if switching {
                                zenclash_i18n::text("proxies.actions.switching")
                            } else {
                                delay_text
                            }))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(
                                format!("{}{}", proxy.kind, if proxy.udp { " · UDP" } else { "" }),
                            )),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.change_proxy(group_name.clone(), proxy_name.clone(), cx);
                    })),
            )
            .child({
                let owner = cx.entity().downgrade();
                let name = proxy.name.clone();
                let protocol = proxy.kind.clone();
                let provider = id.provider().unwrap_or("—").to_owned();
                Button::new(proxy_element_id("proxy-menu", &group.name, id))
                    .label("···")
                    .accessibility_label(zenclash_i18n::text("unified.proxies.details"))
                    .small()
                    .ghost()
                    .loading(testing)
                    .dropdown_menu(move |menu, _, _| {
                        let owner = owner.clone();
                        let group = delay_group.clone();
                        let node = delay_proxy.clone();
                        let url = test_url.clone();
                        let name = name.clone();
                        let protocol = protocol.clone();
                        let provider = provider.clone();
                        menu.item(
                            PopupMenuItem::new(zenclash_i18n::text("proxies.actions.test"))
                                .disabled(testing || selection_blocked)
                                .on_click(move |_, _, cx| {
                                    let _ = owner.update(cx, |page, cx| {
                                        page.test_proxy(
                                            group.clone(),
                                            node.clone(),
                                            url.clone(),
                                            cx,
                                        )
                                    });
                                }),
                        )
                        .item(
                            PopupMenuItem::new(zenclash_i18n::text("unified.proxies.details"))
                                .on_click(move |_, window, cx| {
                                    let name = name.clone();
                                    let protocol = protocol.clone();
                                    let provider = provider.clone();
                                    window.open_dialog(cx, move |dialog, _, _| {
                                        dialog.title(name.clone()).child(
                                            v_flex()
                                                .gap_3()
                                                .child(protocol.clone())
                                                .child(provider.clone()),
                                        )
                                    });
                                }),
                        )
                    })
            })
            .into_any_element()
    }
}

fn group_behavior_label(behavior: &ProxyGroupBehavior) -> String {
    zenclash_i18n::text(match behavior {
        ProxyGroupBehavior::Selector => "proxies.design.manual",
        ProxyGroupBehavior::Automatic { fixed: false } => "proxies.design.automatic",
        ProxyGroupBehavior::Automatic { fixed: true } => "proxies.design.fixed",
        ProxyGroupBehavior::LoadBalance => "proxies.design.multiple",
        ProxyGroupBehavior::Unknown(_) => "common.status.unknown",
    })
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

pub(super) fn proxy_element_id(
    action: &'static str,
    group: &str,
    node: &ProxyNodeId,
) -> gpui_kit::ElementId {
    let group = gpui_kit::ElementId::from((gpui_kit::ElementId::from(action), group.to_owned()));
    let node_id = gpui_kit::ElementId::from((group, node.controller_name().to_owned()));
    match node.provider() {
        Some(provider) => gpui_kit::ElementId::from((node_id, provider.to_owned())),
        None => node_id,
    }
}

fn delay_time(value: &str, format: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map_or_else(|_| "—".into(), |time| time.format(format).to_string())
}

fn group_icon(behavior: &ProxyGroupBehavior) -> Icon {
    match behavior {
        ProxyGroupBehavior::Selector => Icon::default().path(crate::assets::GROUP_ICON_PATH),
        ProxyGroupBehavior::Automatic { .. } => Icon::default().data(br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M13 2 3 14h7l-1 8 12-14h-7l1-8Z"/></svg>"#),
        ProxyGroupBehavior::LoadBalance => Icon::default().path("icons/network.svg"),
        ProxyGroupBehavior::Unknown(kind) if kind.eq_ignore_ascii_case("direct") => Icon::new(IconName::Globe),
        ProxyGroupBehavior::Unknown(_) => Icon::new(IconName::GalleryVerticalEnd),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_filter_keeps_untested_nodes_and_sorts_before_paging() {
        let catalog = ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup {
                    ..Default::default()
                },
                vec![
                    ProxyNode {
                        name: "unknown".into(),
                        ..Default::default()
                    },
                    ProxyNode {
                        name: "timeout".into(),
                        history: vec![zenclash_core::DelayHistory {
                            delay: 0,
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                    ProxyNode {
                        name: "slow".into(),
                        history: vec![zenclash_core::DelayHistory {
                            delay: 100,
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                    ProxyNode {
                        name: "fast".into(),
                        history: vec![zenclash_core::DelayHistory {
                            delay: 10,
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                ],
            )],
            4,
        );
        let group = &catalog.groups()[0];
        let nodes = super::super::presentation::visible_node_indices(
            &catalog,
            group,
            true,
            true,
            &std::collections::HashMap::new(),
        );
        assert_eq!(
            nodes
                .iter()
                .map(|&index| catalog.node(&group.all[index]).unwrap().name.as_str())
                .collect::<Vec<_>>(),
            ["fast", "slow", "unknown"]
        );
    }
}
