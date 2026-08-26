use super::{
    div, h_flex, px, test_key, v_flex, Button, Context, Disableable, FluentBuilder, Icon, IconName,
    InteractiveElement, IntoElement, ParentElement, Progress, ProxiesPage, ProxyGroup, ProxyNode,
    Sizable, StatefulInteractiveElement, Styled,
};

impl ProxiesPage {
    pub(super) fn render_header(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let loading = self.loading;
        let refreshing_disabled = self.switching.is_some();
        h_flex()
            .h(px(86.))
            .px_6()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .size(px(38.))
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.secondary)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(IconName::GalleryVerticalEnd)
                                    .size_5()
                                    .text_color(theme.primary),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_0()
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme.primary)
                                    .child("OVERVIEW / LIVE ROUTING"),
                            )
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("代理与策略"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("选择策略组节点、查看协议能力并执行真实延迟测试。"),
                            ),
                    ),
            )
            .child(
                Button::new("refresh-proxies")
                    .icon(IconName::Redo2)
                    .label(if loading { "读取中" } else { "刷新状态" })
                    .small()
                    .outline()
                    .loading(loading)
                    .disabled(refreshing_disabled)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
    }

    pub(super) fn render_group(
        &self,
        group_index: usize,
        group: &ProxyGroup,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let expanded = self.expanded.contains(&group.name);
        let group_name = group.name.clone();
        let group_for_test = group.clone();
        let testing_group = group
            .all
            .iter()
            .any(|proxy| self.testing.contains(&test_key(&group.name, &proxy.name)));

        v_flex()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .when(theme.shadow, |this| this.shadow_sm())
            .overflow_hidden()
            .child(
                h_flex()
                    .id(("proxy-group", group_index))
                    .min_h(px(64.))
                    .px_4()
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .hover(|this| this.bg(theme.muted.opacity(0.55)))
                    .child(
                        h_flex()
                            .items_center()
                            .gap_3()
                            .child(Icon::new(if expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            }))
                            .child(
                                v_flex()
                                    .gap_0()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(gpui::FontWeight::BOLD)
                                                    .child(group.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .px_2()
                                                    .py(px(2.))
                                                    .rounded_full()
                                                    .bg(theme.muted)
                                                    .text_color(theme.muted_foreground)
                                                    .child(group.kind.clone()),
                                            ),
                                    )
                                    .child(
                                        div().text_xs().text_color(theme.muted_foreground).child(
                                            format!(
                                                "当前：{} · {} 个节点",
                                                group.now,
                                                group.all.len()
                                            ),
                                        ),
                                    ),
                            ),
                    )
                    .child(
                        Button::new(("test-group", group_index))
                            .icon(IconName::Redo2)
                            .label(if testing_group {
                                "测速中"
                            } else {
                                "全部测速"
                            })
                            .small()
                            .outline()
                            .loading(testing_group)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.test_group(&group_for_test, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_group(&group_name, cx);
                    })),
            )
            .when(expanded, |this| {
                this.child(
                    h_flex()
                        .p_3()
                        .gap_2()
                        .flex_wrap()
                        .border_t_1()
                        .border_color(theme.border)
                        .children(group.all.iter().enumerate().map(|(proxy_index, proxy)| {
                            self.render_proxy(group_index, proxy_index, group, proxy, theme, cx)
                        })),
                )
            })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one proxy card is a cohesive declarative GPUI element tree"
    )]
    pub(super) fn render_proxy(
        &self,
        group_index: usize,
        proxy_index: usize,
        group: &ProxyGroup,
        proxy: &ProxyNode,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let selected = group.now == proxy.name;
        let testing = self.testing.contains(&test_key(&group.name, &proxy.name));
        let switching = self.switching.as_ref() == Some(&(group.name.clone(), proxy.name.clone()));
        let group_name = group.name.clone();
        let proxy_name = proxy.name.clone();
        let delay_group = group.name.clone();
        let delay_proxy = proxy.name.clone();
        let test_url = group.test_url.clone();
        let delay_provider = proxy.provider_name.clone();
        let delay = proxy.latest_delay();
        let delay_color = match delay {
            Some(0) => theme.danger,
            Some(value) if value < 500 => theme.success,
            Some(_) => theme.warning,
            None => theme.muted_foreground,
        };
        let delay_text = if testing {
            "测速中…".to_owned()
        } else {
            match delay {
                Some(0) => "超时".to_owned(),
                Some(value) => format!("{value} ms"),
                None => "测速".to_owned(),
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
            .id((
                gpui::ElementId::from(("proxy", group_index)),
                proxy_index.to_string(),
            ))
            .relative()
            .w(px(236.))
            .min_h(px(98.))
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
            .cursor_pointer()
            .hover(|this| this.bg(theme.muted))
            .when(selected, |this| {
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(12.))
                        .bottom(px(12.))
                        .w(px(3.))
                        .rounded_r_full()
                        .bg(theme.primary),
                )
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
                                gpui::FontWeight::BOLD
                            } else {
                                gpui::FontWeight::NORMAL
                            })
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(if switching {
                                format!("{}（切换中…）", proxy.name)
                            } else {
                                proxy.name.clone()
                            }),
                    )
                    .child(
                        div()
                            .id((
                                gpui::ElementId::from(("proxy-delay", group_index)),
                                proxy_index.to_string(),
                            ))
                            .px_2()
                            .py_1()
                            .rounded(theme.radius)
                            .text_xs()
                            .text_color(delay_color)
                            .hover(|this| this.bg(theme.muted))
                            .child(delay_text)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.test_proxy(
                                    delay_group.clone(),
                                    delay_proxy.clone(),
                                    test_url.clone(),
                                    delay_provider.clone(),
                                    cx,
                                );
                            })),
                    ),
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
            .child(Progress::new().h(px(3.)).bg(delay_color).value(health))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.change_proxy(group_name.clone(), proxy_name.clone(), cx);
            }))
            .into_any_element()
    }
}
