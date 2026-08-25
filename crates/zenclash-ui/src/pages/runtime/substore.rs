use super::{
    div, empty_state, h_flex, info_row, message_banner, px, setting_card, v_flex, Button, Context,
    Disableable, FluentBuilder, IconName, InteractiveElement, IntoElement, ParentElement,
    RuntimeData, RuntimePage, Styled, SubStoreItem, SubStoreSnapshot,
};

impl RuntimePage {
    pub(super) fn render_substore(
        &self,
        theme: &gpui_component::Theme,
        _cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let snapshot = match &self.data {
            RuntimeData::SubStore(snapshot) => snapshot.clone(),
            _ => SubStoreSnapshot::default(),
        };
        let frontend_url = snapshot.frontend_url.clone();
        v_flex()
            .gap_4()
            .child(
                setting_card("Sub-Store 服务", theme)
                    .child(info_row(
                        "后端服务",
                        if snapshot.connected {
                            "已连接"
                        } else {
                            "等待连接"
                        },
                        theme,
                    ))
                    .child(info_row("后端地址", &snapshot.backend_url, theme))
                    .child(info_row("前端地址", &snapshot.frontend_url, theme))
                    .child(
                        h_flex().justify_end().p_3().child(
                            Button::new("open-substore")
                                .icon(IconName::ExternalLink)
                                .label("在浏览器中打开")
                                .disabled(frontend_url.is_empty())
                                .on_click(move |_, _, cx| cx.open_url(&frontend_url)),
                        ),
                    ),
            )
            .when_some(snapshot.error, |this, error| {
                this.child(message_banner(
                    format!(
                        "未连接 Sub-Store：{error}。可通过 ZENCLASH_SUBSTORE_URL 和 ZENCLASH_SUBSTORE_FRONTEND_URL 接入现有服务。"
                    ),
                    theme.warning,
                    theme,
                ))
            })
            .child(substore_items(
                "订阅",
                snapshot.subscriptions,
                theme.primary,
                theme,
            ))
            .child(substore_items(
                "集合",
                snapshot.collections,
                theme.success,
                theme,
            ))
            .into_any_element()
    }
}

fn substore_items(
    title: &'static str,
    items: Vec<SubStoreItem>,
    accent: gpui::Hsla,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    let count = items.len();
    let id_prefix = if title == "订阅" {
        "substore-subscription"
    } else {
        "substore-collection"
    };
    setting_card(title, theme)
        .when(count == 0, |this| {
            this.child(empty_state("没有可显示的项目", theme))
        })
        .children(items.into_iter().enumerate().map(|(index, item)| {
            let label = if item.display_name.is_empty() {
                item.name
            } else {
                item.display_name
            };
            h_flex()
                .id((id_prefix, index))
                .min_h(px(48.))
                .px_4()
                .gap_3()
                .border_b_1()
                .border_color(theme.border)
                .child(div().size_2().rounded_full().bg(accent))
                .child(div().flex_1().text_sm().child(label))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(item.tag.join(" · ")),
                )
        }))
        .into_any_element()
}
