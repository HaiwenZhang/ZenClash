use gpui_kit::{InteractiveElement, TestSupportExt};

use super::super::{
    Button, Context, Disableable, FluentBuilder, Icon, IconName, IntoElement, ParentElement,
    ProxiesPage, ProxyGroup, ProxyNode, ProxyNodeId, Sizable, Styled, div,
    group_allows_manual_selection, group_has_unique_current, h_flex, v_flex,
};
use super::proxy_element_id;

impl ProxiesPage {
    pub(in super::super) fn render_proxy(
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
            .get(group.controller_name())
            .is_some_and(|nodes| nodes.contains(id));
        let switching = self
            .switching
            .proxy_pending(group.controller_name(), id.controller_name());
        let selection_blocked = self.proxy_selection_blocked(group.controller_name());
        let group_name = group.controller_name().to_owned();
        let proxy_name = id.controller_name().to_owned();
        let delay_group = group.controller_name().to_owned();
        let delay_proxy = id.clone();
        let test_url = group.test_url.clone();
        let delay = proxy.latest_delay();
        let failure = self.catalog_presentation.failure(id);
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
            .when_some(self.node_grid.card_focus_scope(id), |card, scope| {
                card.track_focus(scope)
            })
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
                gpui_kit::base::Button::new(proxy_element_id(
                    "select-proxy",
                    group.controller_name(),
                    id,
                ))
                .when_some(
                    self.node_focus
                        .get(&(group.controller_name().to_owned(), id.clone())),
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
                .disabled(!selected && (selection_blocked || !selectable))
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
                                                group.controller_name(),
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
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!(
                                    "{}{}",
                                    proxy.kind,
                                    if proxy.udp { " · UDP" } else { "" }
                                )),
                        ),
                )
                .on_click(cx.listener({
                    let id = id.clone();
                    let focus = self
                        .node_focus
                        .get(&(group.controller_name().to_owned(), id.clone()))
                        .cloned();
                    move |this, _, window, cx| {
                        if selected {
                            if let Some(focus) = &focus {
                                focus.focus(window, cx);
                            }
                            this.open_node_details(group_name.clone(), id.clone(), window, cx);
                        } else {
                            this.change_proxy(group_name.clone(), proxy_name.clone(), cx);
                        }
                    }
                })),
            )
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .pb_3()
                    .gap_2()
                    .child(
                        Button::new(proxy_element_id("test-proxy", group.controller_name(), id))
                            .icon(IconName::Play)
                            .label(zenclash_i18n::text("proxies.actions.test"))
                            .small()
                            .outline()
                            .flex_1()
                            .min_w_0()
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
                    .child({
                        let focus_key = (group.controller_name().to_owned(), id.clone());
                        let focus_ref = self.details_focus.get(&focus_key);
                        let focus_for_click = focus_ref.cloned();
                        details_button(
                            proxy_element_id("proxy-node-details", group.controller_name(), id),
                            zenclash_i18n::text("unified.proxies.details"),
                            focus_ref,
                            theme,
                        )
                        .flex_1()
                        .min_w_0()
                        .on_click(cx.listener({
                            let group_name = group.controller_name().to_owned();
                            let id = id.clone();
                            move |this, _, window, cx| {
                                if let Some(focus) = &focus_for_click {
                                    focus.focus(window, cx);
                                }
                                this.open_node_details(group_name.clone(), id.clone(), window, cx)
                            }
                        }))
                    }),
            )
            .into_any_element()
    }
}

fn details_button(
    id: gpui_kit::ElementId,
    label: String,
    focus: Option<&gpui_kit::FocusHandle>,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::base::Button {
    gpui_kit::base::Button::new(id)
        .when_some(focus, |button, focus| button.track_focus(focus))
        .accessibility_label(label.clone())
        .h_6()
        .px_2()
        .text_sm()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .hover(|style| style.bg(theme.list_hover))
        .focus(|style| style.border_color(theme.ring).border_2())
        .styles(|styles| styles.disabled(|style| style.opacity(0.6)))
        .child(label)
}
