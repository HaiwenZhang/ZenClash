mod inspector;
mod proxy_card;

use gpui_kit::base::StyledExt;
use gpui_kit::component::{
    ActiveTheme as _, Selectable,
    button::ButtonVariants,
    menu::{DropdownMenu, PopupMenuItem},
    scroll::ScrollableElement,
};
use gpui_kit::{InteractiveElement, StatefulInteractiveElement, TestSupportExt};

use super::{
    Button, Context, Disableable, FluentBuilder, Icon, IconName, IntoElement, ParentElement,
    ProxiesPage, ProxyCatalog, ProxyGroup, ProxyGroupBehavior, ProxyNodeId, Sizable, Styled,
    Switch, div, h_flex, v_flex,
};

impl ProxiesPage {
    pub(super) fn render_workspace(
        &self,
        catalog: &ProxyCatalog,
        compact: bool,
        columns: u16,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let visible = &self.visible_group_indices;
        let selected = super::presentation::selected_group_index(
            catalog,
            visible,
            self.selected_group.as_deref(),
        );
        let Some(selected) = selected else {
            return div().into_any_element();
        };
        let group = &catalog.groups()[selected];
        let navigation =
            v_flex()
                .id("proxy-group-navigation")
                .test_support()
                .w(gpui_kit::rems(17.))
                .min_w_0()
                .max_w_full()
                .h_full()
                .min_h_0()
                .flex_shrink_0()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .bg(theme.group_box)
                .overflow_hidden()
                .child(
                    div()
                        .px_3()
                        .pt_3()
                        .pb_2()
                        .text_lg()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(format!(
                            "{} ({})",
                            zenclash_i18n::text("home.proxy.groups"),
                            self.visible_group_indices.len()
                        )),
                )
                .child(
                    div()
                        .id("proxy-group-list")
                        .test_support()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.group_scroll)
                        .child(v_flex().w_full().gap_2().px_3().pb_3().children(
                            visible.iter().map(|&index| {
                                let item = &catalog.groups()[index];
                                let name = item.controller_name().to_owned();
                                Button::new((
                                    gpui_kit::ElementId::from("toggle-group"),
                                    name.clone(),
                                ))
                                .accessibility_label(item.name.clone())
                                .tooltip(item.name.clone())
                                .ghost()
                                .flex_shrink_0()
                                .child(
                                    group_icon(&item.behavior)
                                        .size_8()
                                        .when(index == selected, |icon| {
                                            icon.text_color(theme.primary)
                                        }),
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
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        if this.selected_group.as_deref() != Some(name.as_str()) {
                                            this.toggle_group(&name, cx);
                                        }
                                    },
                                ))
                            }),
                        ))
                        .vertical_scrollbar(&self.group_scroll),
                );
        h_flex()
            .flex_1()
            .min_h_0()
            .items_stretch()
            .gap_3()
            .when(!compact, |row| row.child(navigation))
            .child(
                v_flex()
                    .flex_1()
                    .flex_basis(gpui_kit::rems(30.))
                    .min_w_0()
                    .max_w_full()
                    .h_full()
                    .min_h_0()
                    .gap_3()
                    .when(compact, |column| {
                        let owner = cx.entity().downgrade();
                        let choices = self
                            .visible_group_indices
                            .iter()
                            .map(|&index| {
                                let group = &catalog.groups()[index];
                                (group.controller_name().to_owned(), group.name.clone())
                            })
                            .collect::<Vec<_>>();
                        column.child(
                            Button::new("compact-proxy-group")
                                .label(group.name.clone())
                                .outline()
                                .dropdown_caret(true)
                                .dropdown_menu(move |mut menu, window, _| {
                                    menu = menu
                                        .scrollable(true)
                                        .max_h(window.viewport_size().height * 0.6);
                                    for (key, label) in &choices {
                                        let name = key.clone();
                                        let label = label.clone();
                                        let owner = owner.clone();
                                        menu = menu.item(PopupMenuItem::new(label).on_click(
                                            move |_, _, cx| {
                                                let _ = owner.update(cx, |page, cx| {
                                                    if page.selected_group.as_deref()
                                                        != Some(name.as_str())
                                                    {
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
            .into_any_element()
    }

    pub(super) fn render_header(
        &self,
        _theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.catalog.as_ref().and_then(|catalog| {
            super::presentation::selected_group_index(
                catalog,
                &self.visible_group_indices,
                self.selected_group.as_deref(),
            )
            .map(|index| catalog.groups()[index].controller_name().to_owned())
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
                        crate::outbound_mode::OutboundMode::Rule,
                        crate::outbound_mode::OutboundMode::Global,
                        crate::outbound_mode::OutboundMode::Direct,
                    ]
                    .into_iter()
                    .map(|mode| {
                        Button::new((gpui_kit::ElementId::from("proxy-mode"), mode.api_value()))
                            .label(mode.label())
                            .outline()
                            .selected(self.outbound_mode == mode.api_value())
                            .disabled(self.operation_pending())
                            .on_click(move |_, window, cx| {
                                use crate::outbound_mode::OutboundMode;
                                match mode {
                                    OutboundMode::Rule => window.dispatch_action(
                                        Box::new(crate::foundation::actions::SetRuleMode),
                                        cx,
                                    ),
                                    OutboundMode::Global => window.dispatch_action(
                                        Box::new(crate::foundation::actions::SetGlobalMode),
                                        cx,
                                    ),
                                    OutboundMode::Direct => window.dispatch_action(
                                        Box::new(crate::foundation::actions::SetDirectMode),
                                        cx,
                                    ),
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
                        crate::foundation::actions::Page::Proxies,
                        cx,
                    ))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("refresh-proxies")
                                    .icon(crate::foundation::assets::AppIcon::RefreshCw)
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
                Button::new("sort-proxies-by-latency")
                    .icon(gpui_kit::assets::IconName::ArrowDownUp)
                    .label(zenclash_i18n::text(if sort_by_latency {
                        "proxies.actions.sort_menu"
                    } else {
                        "proxies.actions.original_order"
                    }))
                    .h(gpui_kit::rems(2.75))
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
            .flex_1()
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
            .when_some(
                self.switching.status_text(group.controller_name()),
                |this, status| {
                    this.child(
                        div()
                            .id((
                                gpui_kit::ElementId::from("proxy-selection-status"),
                                group.controller_name().to_owned(),
                            ))
                            .test_support()
                            .px_3()
                            .pb_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(status),
                    )
                },
            )
            .when(expanded, |this| {
                this.child(
                    v_flex()
                        .flex_1()
                        .min_h_0()
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
                        .child(self.render_node_grid(catalog, group, columns, cx)),
                )
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

fn group_icon(behavior: &ProxyGroupBehavior) -> Icon {
    match behavior {
        ProxyGroupBehavior::Selector => Icon::default().path(crate::foundation::assets::GROUP_ICON_PATH),
        ProxyGroupBehavior::Automatic { .. } => Icon::default().data(br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M13 2 3 14h7l-1 8 12-14h-7l1-8Z"/></svg>"#),
        ProxyGroupBehavior::LoadBalance => Icon::default().path("icons/network.svg"),
        ProxyGroupBehavior::Unknown(kind) if kind.eq_ignore_ascii_case("direct") => Icon::new(IconName::Globe),
        ProxyGroupBehavior::Unknown(_) => Icon::new(IconName::GalleryVerticalEnd),
    }
}

#[cfg(test)]
mod tests {
    use super::super::ProxyNode;
    use super::*;

    #[test]
    fn latency_filter_keeps_untested_nodes_and_sorts_before_paging() {
        let catalog = ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup::new(String::new()),
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
