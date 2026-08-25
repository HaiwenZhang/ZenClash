use super::{
    div, h_flex, info_row, json, px, setting_card, setting_switch, v_flex, Button, Context,
    HideTrafficIcon, IconName, IntoElement, ParentElement, RuntimePage, SetDarkTheme,
    SetLightTheme, ShowTrafficIcon, Sizable, Styled,
};

impl RuntimePage {
    pub(super) fn render_settings(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let config = self.config().cloned().unwrap_or_default();
        setting_card("应用与控制器", theme)
            .child(info_row("界面技术", "Rust · GPUI · gpui-component", theme))
            .child(info_row(
                "控制器",
                &self.client.endpoint().controller,
                theme,
            ))
            .child(setting_switch(
                "IPv6",
                "同步修改 Mihomo 运行时设置",
                config.ipv6,
                "settings-ipv6",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_config(json!({"ipv6": *checked}), "IPv6 设置已更新", cx);
                }),
            ))
            .child(
                h_flex()
                    .min_h(px(58.))
                    .px_4()
                    .gap_3()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        v_flex().gap_1().child(div().text_sm().child("主题")).child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("切换 GPUI 原生明暗外观"),
                        ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("theme-light")
                                    .icon(IconName::Sun)
                                    .label("浅色")
                                    .small()
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(SetLightTheme), cx);
                                    }),
                            )
                            .child(
                                Button::new("theme-dark")
                                    .icon(IconName::Moon)
                                    .label("深色")
                                    .small()
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(SetDarkTheme), cx);
                                    }),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .min_h(px(58.))
                    .px_4()
                    .gap_3()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_sm().child("状态栏流量图标"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("显示实时上传、下载速率和动态箭头图标"),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Button::new("tray-show").label("显示").small().on_click(
                                |_, window, cx| {
                                    window.dispatch_action(Box::new(ShowTrafficIcon), cx);
                                },
                            ))
                            .child(Button::new("tray-hide").label("隐藏").small().on_click(
                                |_, window, cx| {
                                    window.dispatch_action(Box::new(HideTrafficIcon), cx);
                                },
                            )),
                    ),
            )
            .into_any_element()
    }
}
