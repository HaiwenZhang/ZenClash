use gpui_component::{button::ButtonVariants, Selectable};

use super::{
    div, h_flex, px, test_key, v_flex, Button, Context, Disableable, FluentBuilder, Icon, IconName,
    IntoElement, ParentElement, Progress, ProxiesPage, ProxyGroup, ProxyNode, Sizable, Styled,
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
            .h_16()
            .px_5()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_3()
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
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("代理组"),
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
                    .ghost()
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
                    .min_h(px(64.))
                    .px_4()
                    .items_center()
                    .justify_between()
                    .child(
                        h_flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .size_8()
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
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new(("test-group", group_index))
                                    .icon(IconName::Redo2)
                                    .label(if testing_group {
                                        "测速中"
                                    } else {
                                        "全部测速"
                                    })
                                    .small()
                                    .ghost()
                                    .loading(testing_group)
                                    .disabled(testing_group)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.test_group(&group_for_test, cx);
                                    })),
                            )
                            .child(
                                Button::new(("toggle-group", group_index))
                                    .icon(if expanded {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .label(if expanded { "收起" } else { "展开" })
                                    .small()
                                    .outline()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.toggle_group(&group_name, cx);
                                    })),
                            ),
                    ),
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
        let failure = self.test_failures.get(&test_key(&group.name, &proxy.name));
        let delay_color = match (failure, delay) {
            (Some(_), _) => theme.danger,
            (None, Some(0)) => theme.danger,
            (None, Some(value)) if value < 500 => theme.success,
            (None, Some(_)) => theme.warning,
            (None, None) => theme.muted_foreground,
        };
        let delay_text = if testing {
            "测速中…".to_owned()
        } else if let Some(failure) = failure {
            failure.label().to_owned()
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
            .relative()
            .w(px(236.))
            .min_h(px(126.))
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
            .child(Progress::new().h(px(3.)).bg(delay_color).value(health))
            .child(
                h_flex()
                    .justify_end()
                    .gap_1()
                    .child(
                        Button::new((
                            gpui::ElementId::from(("test-proxy", group_index)),
                            proxy_index.to_string(),
                        ))
                        .icon(IconName::Redo2)
                        .label(if testing { "测速中" } else { "测速" })
                        .small()
                        .ghost()
                        .loading(testing)
                        .disabled(testing)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.test_proxy(
                                delay_group.clone(),
                                delay_proxy.clone(),
                                test_url.clone(),
                                delay_provider.clone(),
                                cx,
                            );
                        })),
                    )
                    .child(
                        Button::new((
                            gpui::ElementId::from(("select-proxy", group_index)),
                            proxy_index.to_string(),
                        ))
                        .icon(if selected {
                            IconName::Check
                        } else {
                            IconName::ArrowRight
                        })
                        .label(if selected {
                            "当前节点"
                        } else if switching {
                            "切换中"
                        } else {
                            "选择"
                        })
                        .small()
                        .outline()
                        .selected(selected)
                        .loading(switching)
                        .disabled(selected || self.switching.is_some())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.change_proxy(group_name.clone(), proxy_name.clone(), cx);
                        })),
                    ),
            )
            .into_any_element()
    }
}
