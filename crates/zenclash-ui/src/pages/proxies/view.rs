use gpui_kit::component::{Selectable, button::ButtonVariants};
use gpui_kit::{InteractiveElement, StatefulInteractiveElement};

use super::{
    Button, Context, Disableable, FluentBuilder, Icon, IconName, IntoElement, ParentElement,
    Progress, ProxiesPage, ProxyCatalog, ProxyGroup, ProxyGroupBehavior, ProxyNode, ProxyNodeId,
    ProxyPage, Sizable, Styled, Switch, div, group_allows_manual_selection,
    group_has_unique_current, h_flex, proxy_page, px, v_flex,
};

impl ProxiesPage {
    pub(super) fn render_workspace(
        &self,
        catalog: &ProxyCatalog,
        page: ProxyPage,
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
            .w_56()
            .min_w_0()
            .max_w_full()
            .flex_shrink_0()
            .gap_2()
            .p_3()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .child(
                div()
                    .pb_2()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(zenclash_i18n::text("proxies.header.title")),
            )
            .children(visible.iter().map(|&index| {
                let item = &catalog.groups()[index];
                let name = item.name.clone();
                Button::new((gpui_kit::ElementId::from("toggle-group"), name.clone()))
                    .label(item.name.clone())
                    .tooltip(item.name.clone())
                    .outline()
                    .selected(index == selected)
                    .w_full()
                    .min_w_0()
                    .justify_start()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.expanded.contains(&name) {
                            this.toggle_group(&name, cx);
                        }
                    }))
            }));
        h_flex()
            .items_start()
            .gap_3()
            .flex_wrap()
            .child(navigation)
            .child(
                v_flex()
                    .flex_1()
                    .flex_basis(gpui_kit::rems(28.))
                    .min_w_0()
                    .max_w_full()
                    .gap_3()
                    .when_some(self.search_input.as_ref(), |this, input| {
                        this.child(
                            gpui_kit::component::input::Input::new(input)
                                .id("proxy-node-search")
                                .prefix(Icon::new(IconName::Search)),
                        )
                    })
                    .when(
                        !self.search_query.is_empty() && self.search_projection.is_none(),
                        |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(zenclash_i18n::text("common.actions.loading")),
                            )
                        },
                    )
                    .when(
                        !self.search_query.is_empty()
                            && self.search_projection.is_some()
                            && self.displayed_nodes(catalog, group).is_empty(),
                        |this| {
                            this.child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(zenclash_i18n::text("proxies.design.no_matches")),
                            )
                        },
                    )
                    .child(self.render_latency_comparison(catalog, group, theme))
                    .child(self.render_group(
                        catalog,
                        group,
                        self.active_testing_groups.contains_key(&group.name),
                        theme,
                        cx,
                    )),
            )
            .child(self.render_current_node(catalog, group, theme, cx))
            .into_any_element()
    }

    fn render_latency_comparison(
        &self,
        catalog: &ProxyCatalog,
        group: &ProxyGroup,
        theme: &gpui_kit::component::Theme,
    ) -> gpui_kit::Div {
        let order = self.displayed_nodes(catalog, group);
        let page = proxy_page(
            order.len(),
            self.proxy_pages
                .get(&group.name)
                .copied()
                .unwrap_or_default(),
        );
        let nodes = &order[page.start..page.end];
        let maximum = nodes
            .iter()
            .filter_map(|&index| {
                catalog
                    .node(&group.all[index])
                    .and_then(ProxyNode::latest_delay)
            })
            .max()
            .unwrap_or(1)
            .max(1);
        v_flex()
            .p_4()
            .gap_3()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(zenclash_i18n::text("proxies.design.latency_comparison")),
            )
            .children(nodes.iter().take(5).filter_map(|&index| {
                let id = &group.all[index];
                let node = catalog.node(id)?;
                let failure = self.test_failures.get(id);
                let delay = failure.is_none().then(|| node.latest_delay()).flatten();
                let color = if self.test_failures.contains_key(id) || delay == Some(0) {
                    theme.danger
                } else if group_has_unique_current(&group.behavior)
                    && group.now == id.controller_name()
                {
                    theme.primary
                } else {
                    theme.chart_1
                };
                let value = delay.map_or(0., |delay| {
                    let percent = u64::from(delay) * 100 / u64::from(maximum);
                    f32::from(u16::try_from(percent).unwrap_or(100))
                });
                Some(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(
                            div()
                                .w_32()
                                .min_w_0()
                                .text_xs()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .child(node.name.clone()),
                        )
                        .child(
                            Progress::new(proxy_element_id("latency-comparison", &group.name, id))
                                .flex_1()
                                .h_2()
                                .value(value)
                                .color(color),
                        )
                        .child(
                            div()
                                .w_16()
                                .text_xs()
                                .text_color(color)
                                .child(delay.map_or_else(
                                    || {
                                        failure.map_or_else(
                                            || zenclash_i18n::text("home.proxy.untested"),
                                            |failure| failure.label(),
                                        )
                                    },
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
            }))
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
        v_flex()
            .w_64()
            .max_w_full()
            .flex_shrink_0()
            .p_4()
            .gap_4()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(zenclash_i18n::text("proxies.actions.current")),
            )
            .when_some(node.zip(id.as_ref()), |this, (node, id)| {
                let node_name = node.name.clone();
                let history = node.history.iter().rev().take(10).collect::<Vec<_>>();
                let points = history
                    .into_iter()
                    .rev()
                    .enumerate()
                    .filter(|(_, point)| point.delay > 0)
                    .map(|(index, point)| ((index + 1).to_string(), point.delay))
                    .collect::<Vec<_>>();
                let has_history = !points.is_empty();
                this.child(
                    div()
                        .id(proxy_element_id("inspector-node-name", &group.name, id))
                        .text_lg()
                        .w_full()
                        .min_w_0()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .tooltip(move |window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(node_name.clone())
                                .build(window, cx)
                        })
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(node.name.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(node.kind.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(node.capabilities().collect::<Vec<_>>().join(" · ")),
                )
                .child(
                    div()
                        .text_2xl()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(theme.primary)
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
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(group.name.clone()),
                )
                .child(
                    Button::new(proxy_element_id("inspector-test", &group.name, id))
                        .label(zenclash_i18n::text("proxies.actions.test"))
                        .outline()
                        .small()
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
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(zenclash_i18n::text("proxies.design.latency_history")),
                )
                .when(has_history, |this| {
                    this.child(
                        div().h_40().w_full().child(
                            gpui_kit::component::chart::LineChart::new(points)
                                .id(proxy_element_id("delay-history", &group.name, id))
                                .x(|point| point.0.clone())
                                .y(|point| f64::from(point.1))
                                .stroke(theme.chart_1)
                                .linear()
                                .dot()
                                .y_axis(true)
                                .y_tick_format(|value| format!("{value:.0} ms")),
                        ),
                    )
                })
                .when(!has_history, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("home.proxy.untested")),
                    )
                })
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(group.kind.clone()),
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
            })
    }

    pub(super) fn render_group_pagination(
        &self,
        page: ProxyPage,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .px_5()
            .py_3()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
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
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let loading = self.loading;
        let operation_pending = self.operation_pending();
        let show_hidden = self.show_hidden;
        h_flex()
            .min_h(gpui_kit::rems(4.))
            .py_3()
            .gap_3()
            .flex_wrap()
            .px_5()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_3()
                    .flex_1()
                    .min_w(gpui_kit::rems(20.))
                    .child(
                        Icon::new(IconName::GalleryVerticalEnd)
                            .size_5()
                            .text_color(theme.primary),
                    )
                    .child(
                        v_flex()
                            .gap_0()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(gpui_kit::FontWeight::BOLD)
                                    .child(zenclash_i18n::text("proxies.header.title")),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .max_w(gpui_kit::rems(40.))
                                    .text_color(theme.muted_foreground)
                                    .child(zenclash_i18n::text("proxies.header.description")),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .flex_wrap()
                    .child(
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
                                    .accessibility_label(zenclash_i18n::text(
                                        "proxies.actions.show_hidden",
                                    ))
                                    .checked(show_hidden)
                                    .disabled(loading || operation_pending)
                                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                        this.set_show_hidden(*checked, cx);
                                    })),
                            ),
                    )
                    .child(
                        Button::new("sort-proxies-by-latency")
                            .label(zenclash_i18n::text("proxies.actions.sort_latency"))
                            .small()
                            .outline()
                            .selected(self.sort_by_latency)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sort_by_latency = !this.sort_by_latency;
                                this.proxy_pages.clear();
                                this.prepare_search(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("hide-unavailable-proxies")
                            .label(zenclash_i18n::text("proxies.actions.hide_unavailable"))
                            .small()
                            .outline()
                            .selected(self.hide_unavailable)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.hide_unavailable = !this.hide_unavailable;
                                this.proxy_pages.clear();
                                this.prepare_search(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("refresh-proxies")
                            .icon(crate::assets::AppIcon::RefreshCw)
                            .label(if loading {
                                zenclash_i18n::text("common.actions.loading")
                            } else {
                                zenclash_i18n::text("proxies.actions.refresh")
                            })
                            .small()
                            .ghost()
                            .loading(loading)
                            .disabled(operation_pending)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    ),
            )
    }

    pub(super) fn render_group(
        &self,
        catalog: &ProxyCatalog,
        group: &ProxyGroup,
        testing_group: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let expanded = true;
        let group_for_restore = group.name.clone();
        let group_for_test = group.name.clone();
        let restoring_auto = self.restoring_auto.as_deref() == Some(group.name.as_str());
        let measuring_and_restoring =
            self.measuring_and_restoring_auto.as_deref() == Some(group.name.as_str());
        let group_for_measure_restore = group.name.clone();
        let group_test_url = group.test_url.clone();
        let selection_blocked = self.proxy_selection_blocked(&group.name);

        v_flex()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .overflow_hidden()
            .child(
                h_flex()
                    .min_h(px(64.))
                    .px_4()
                    .py_3()
                    .gap_3()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .child(
                        h_flex()
                            .items_center()
                            .gap_3()
                            .flex_1()
                            .flex_basis(gpui_kit::rems(18.))
                            .min_w_0()
                            .max_w_full()
                            .child(
                                div()
                                    .size_8()
                                    .flex_shrink_0()
                                    .rounded(theme.radius)
                                    .bg(theme.muted)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        Icon::new(IconName::GalleryVerticalEnd)
                                            .size_4()
                                            .text_color(theme.primary),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_0()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .text_ellipsis()
                                                    .whitespace_nowrap()
                                                    .overflow_hidden()
                                                    .font_weight(gpui_kit::FontWeight::BOLD)
                                                    .child(group.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .flex_shrink_0()
                                                    .px_2()
                                                    .py(px(2.))
                                                    .rounded_full()
                                                    .bg(theme.muted)
                                                    .text_color(theme.muted_foreground)
                                                    .child(group.kind.clone()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .min_w_0()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .overflow_hidden()
                                            .text_color(theme.muted_foreground)
                                            .child(match &group.behavior {
                                                ProxyGroupBehavior::Selector => {
                                                    zenclash_i18n::text_with(
                                                        "proxies.summary.current",
                                                        &[
                                                            ("proxy", group.now.clone()),
                                                            ("count", group.all.len().to_string()),
                                                        ],
                                                    )
                                                }
                                                ProxyGroupBehavior::Automatic { fixed: true } => {
                                                    zenclash_i18n::text_with(
                                                        "proxies.summary.fixed",
                                                        &[
                                                            ("proxy", group.now.clone()),
                                                            ("count", group.all.len().to_string()),
                                                        ],
                                                    )
                                                }
                                                ProxyGroupBehavior::Automatic { fixed: false } => {
                                                    zenclash_i18n::text_with(
                                                        "proxies.summary.automatic",
                                                        &[
                                                            ("proxy", group.now.clone()),
                                                            ("count", group.all.len().to_string()),
                                                        ],
                                                    )
                                                }
                                                ProxyGroupBehavior::LoadBalance => {
                                                    zenclash_i18n::text_with(
                                                        "proxies.summary.load_balance",
                                                        &[("count", group.all.len().to_string())],
                                                    )
                                                }
                                                ProxyGroupBehavior::Unknown(kind) => {
                                                    zenclash_i18n::text_with(
                                                        "proxies.summary.unknown",
                                                        &[
                                                            ("type", kind.clone()),
                                                            ("count", group.all.len().to_string()),
                                                        ],
                                                    )
                                                }
                                            }),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .flex_wrap()
                            .max_w_full()
                            .when(
                                matches!(group.behavior, ProxyGroupBehavior::Automatic { .. }),
                                |this| {
                                    this.child(
                                        Button::new((
                                            gpui_kit::ElementId::from("measure-restore-auto"),
                                            group.name.clone(),
                                        ))
                                        .icon(crate::assets::AppIcon::Gauge)
                                        .label(if measuring_and_restoring {
                                            zenclash_i18n::text("proxies.actions.testing")
                                        } else {
                                            zenclash_i18n::text(
                                                "proxies.actions.measure_restore_auto",
                                            )
                                        })
                                        .small()
                                        .ghost()
                                        .loading(measuring_and_restoring)
                                        .disabled(self.operation_pending())
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.measure_group_and_restore_auto(
                                                    group_for_measure_restore.clone(),
                                                    group_test_url.clone(),
                                                    cx,
                                                );
                                            }),
                                        ),
                                    )
                                },
                            )
                            .when(
                                matches!(
                                    group.behavior,
                                    ProxyGroupBehavior::Automatic { fixed: true }
                                ),
                                |this| {
                                    this.child(
                                        Button::new((
                                            gpui_kit::ElementId::from("restore-auto"),
                                            group.name.clone(),
                                        ))
                                        .icon(crate::assets::AppIcon::RefreshCw)
                                        .label(if restoring_auto {
                                            zenclash_i18n::text("proxies.actions.restoring_auto")
                                        } else {
                                            zenclash_i18n::text("proxies.actions.restore_auto")
                                        })
                                        .small()
                                        .outline()
                                        .loading(restoring_auto)
                                        .disabled(self.operation_pending())
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.restore_auto(group_for_restore.clone(), cx);
                                            }),
                                        ),
                                    )
                                },
                            )
                            .child(
                                Button::new((
                                    gpui_kit::ElementId::from("test-group"),
                                    group.name.clone(),
                                ))
                                .icon(crate::assets::AppIcon::Gauge)
                                .label(
                                    if let Some((done, total)) =
                                        self.group_progress.get(&group.name)
                                    {
                                        zenclash_i18n::text_with(
                                            "proxies.actions.testing_progress",
                                            &[
                                                ("done", done.to_string()),
                                                ("total", total.to_string()),
                                            ],
                                        )
                                    } else if testing_group {
                                        zenclash_i18n::text("proxies.actions.testing")
                                    } else {
                                        zenclash_i18n::text("proxies.actions.test_all")
                                    },
                                )
                                .small()
                                .ghost()
                                .loading(testing_group)
                                .disabled(testing_group || selection_blocked)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.test_group(&group_for_test, cx);
                                    },
                                )),
                            ),
                    ),
            )
            .when(expanded, |this| {
                let nodes = self.displayed_nodes(catalog, group);
                let page = proxy_page(
                    nodes.len(),
                    self.proxy_pages
                        .get(&group.name)
                        .copied()
                        .unwrap_or_default(),
                );

                let previous_group = group.name.clone();
                let next_group = group.name.clone();
                this.child(
                    v_flex()
                        .border_t_1()
                        .border_color(theme.border)
                        .child(h_flex().p_3().gap_2().flex_wrap().children(
                            nodes[page.start..page.end].iter().filter_map(|&index| {
                                let id = &group.all[index];
                                catalog
                                    .node(id)
                                    .map(|node| self.render_proxy(group, id, node, theme, cx))
                            }),
                        ))
                        .when(page.count > 1, |this| {
                            this.child(
                                h_flex()
                                    .px_3()
                                    .pb_3()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div().text_xs().text_color(theme.muted_foreground).child(
                                            zenclash_i18n::text_with(
                                                "proxies.pagination.summary",
                                                &[
                                                    ("current", (page.index + 1).to_string()),
                                                    ("total", page.count.to_string()),
                                                    ("first", (page.start + 1).to_string()),
                                                    ("last", page.end.to_string()),
                                                    ("count", nodes.len().to_string()),
                                                ],
                                            ),
                                        ),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(
                                                Button::new((
                                                    gpui_kit::ElementId::from(
                                                        "previous-proxy-page",
                                                    ),
                                                    group.name.clone(),
                                                ))
                                                .icon(IconName::ChevronLeft)
                                                .label(zenclash_i18n::text(
                                                    "proxies.actions.previous_page",
                                                ))
                                                .small()
                                                .outline()
                                                .disabled(page.index == 0)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.set_group_page(
                                                        previous_group.clone(),
                                                        page.index.saturating_sub(1),
                                                        cx,
                                                    );
                                                })),
                                            )
                                            .child(
                                                Button::new((
                                                    gpui_kit::ElementId::from("next-proxy-page"),
                                                    group.name.clone(),
                                                ))
                                                .icon(IconName::ChevronRight)
                                                .label(zenclash_i18n::text(
                                                    "proxies.actions.next_page",
                                                ))
                                                .small()
                                                .outline()
                                                .disabled(page.index + 1 >= page.count)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.set_group_page(
                                                        next_group.clone(),
                                                        page.index + 1,
                                                        cx,
                                                    );
                                                })),
                                            ),
                                    ),
                            )
                        }),
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
                None => zenclash_i18n::text("proxies.actions.test"),
            }
        };
        let capabilities = proxy.capabilities().collect::<Vec<_>>().join(" · ");
        let health = match delay {
            Some(0) | None => 0.,
            Some(value) => {
                let value = u16::try_from(value.min(1_000)).unwrap_or(1_000);
                100. - (f32::from(value) / 10.)
            }
        };

        v_flex()
            .relative()
            .w_56()
            .max_w_full()
            .min_h(gpui_kit::rems(8.))
            .gap_2()
            .p_3()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(if selected {
                theme.primary
            } else {
                theme.border
            })
            .bg(if selected {
                theme.primary.opacity(0.12)
            } else {
                theme.background
            })
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_weight(if selected {
                                gpui_kit::FontWeight::BOLD
                            } else {
                                gpui_kit::FontWeight::NORMAL
                            })
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(if switching {
                                zenclash_i18n::text_with(
                                    "proxies.status.switching",
                                    &[("proxy", proxy.name.clone())],
                                )
                            } else {
                                proxy.name.clone()
                            }),
                    )
                    .child(div().text_xs().text_color(delay_color).child(delay_text)),
            )
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(proxy.kind.clone())
                    .child(if capabilities.is_empty() {
                        "—".to_owned()
                    } else {
                        capabilities
                    }),
            )
            .child(
                Progress::new(proxy_element_id("proxy-health", &group.name, id))
                    .h(px(3.))
                    .color(delay_color)
                    .value(health),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_1()
                    .child(
                        Button::new(proxy_element_id("test-proxy", &group.name, id))
                            .icon(crate::assets::AppIcon::Gauge)
                            .label(if testing {
                                zenclash_i18n::text("proxies.actions.testing")
                            } else {
                                zenclash_i18n::text("proxies.actions.test")
                            })
                            .small()
                            .ghost()
                            .loading(testing)
                            .disabled(testing || selection_blocked)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.test_proxy(
                                    delay_group.clone(),
                                    delay_proxy.clone(),
                                    test_url.clone(),
                                    cx,
                                );
                            })),
                    )
                    .when(selectable, |this| {
                        this.child(
                            Button::new(proxy_element_id("select-proxy", &group.name, id))
                                .tooltip(proxy.name.clone())
                                .icon(if selected {
                                    Icon::new(IconName::Check)
                                } else {
                                    Icon::new(crate::assets::AppIcon::SquareMousePointer)
                                })
                                .label(if selected {
                                    zenclash_i18n::text("proxies.actions.current")
                                } else if switching {
                                    zenclash_i18n::text("proxies.actions.switching")
                                } else {
                                    zenclash_i18n::text("proxies.actions.select")
                                })
                                .small()
                                .outline()
                                .selected(selected)
                                .loading(switching)
                                .disabled(selected || selection_blocked)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.change_proxy(group_name.clone(), proxy_name.clone(), cx);
                                })),
                        )
                    }),
            )
            .into_any_element()
    }
}

fn proxy_element_id(action: &'static str, group: &str, node: &ProxyNodeId) -> gpui_kit::ElementId {
    let group = gpui_kit::ElementId::from((gpui_kit::ElementId::from(action), group.to_owned()));
    let node_id = gpui_kit::ElementId::from((group, node.controller_name().to_owned()));
    match node.provider() {
        Some(provider) => gpui_kit::ElementId::from((node_id, provider.to_owned())),
        None => node_id,
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
