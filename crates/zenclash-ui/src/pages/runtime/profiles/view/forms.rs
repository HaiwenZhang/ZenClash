use zenclash_core::RuntimeConfig;

use super::super::super::{
    div, h_flex, info_row, px, setting_card, v_flex, Button, ButtonVariants, Context, Disableable,
    Icon, IconName, Input, ParentElement, RemoteProfileRoute, RuntimePage, Styled, Switch,
};

impl RuntimePage {
    pub(super) fn render_subscription_form(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        setting_card("添加在线订阅", theme).child(
            v_flex()
                .p_4()
                .gap_3()
                .child(
                    h_flex()
                        .gap_3()
                        .child(subscription_input(
                            "订阅名称",
                            Input::new(&self.profile_forms.subscription_name)
                                .prefix(Icon::new(IconName::File))
                                .cleanable(true),
                            theme,
                        ))
                        .child(
                            subscription_input(
                                "自定义 User-Agent",
                                Input::new(&self.profile_forms.subscription_user_agent)
                                    .prefix(Icon::new(IconName::Bot))
                                    .cleanable(true),
                                theme,
                            )
                            .w(px(220.)),
                        ),
                )
                .child(subscription_input(
                    "Clash / Mihomo 订阅 URL",
                    Input::new(&self.profile_forms.subscription_url)
                        .prefix(Icon::new(IconName::Globe))
                        .cleanable(true),
                    theme,
                ))
                .child(
                    h_flex()
                        .gap_3()
                        .child(subscription_input(
                            "Authorization",
                            Input::new(&self.profile_forms.subscription_authorization)
                                .prefix(Icon::new(IconName::Asterisk))
                                .mask_toggle()
                                .cleanable(true),
                            theme,
                        ))
                        .child(self.render_subscription_route_controls(theme, cx)),
                )
                .child(
                    h_flex()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("下载后将依次执行 YAML 校验、内核验收并设为当前配置。"),
                        )
                        .child(
                            Button::new("download-subscription")
                                .icon(IconName::Inbox)
                                .label("下载并启用")
                                .primary()
                                .loading(self.mutating)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.add_remote_profile(cx);
                                })),
                        ),
                ),
        )
    }

    fn render_subscription_route_controls(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        v_flex()
            .w(px(220.))
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Switch::new("subscription-use-mihomo-proxy")
                            .checked(
                                self.profile_forms.subscription_route == RemoteProfileRoute::Mihomo,
                            )
                            .disabled(self.mutating)
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.profile_forms.subscription_route = if *checked {
                                    RemoteProfileRoute::Mihomo
                                } else {
                                    RemoteProfileRoute::DirectWithMihomoFallback
                                };
                                cx.notify();
                            })),
                    )
                    .child(div().text_sm().child("始终经内核代理下载")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Switch::new("subscription-mihomo-fallback")
                            .checked(
                                self.profile_forms.subscription_route
                                    == RemoteProfileRoute::DirectWithMihomoFallback,
                            )
                            .disabled(
                                self.mutating
                                    || self.profile_forms.subscription_route
                                        == RemoteProfileRoute::Mihomo,
                            )
                            .on_click(cx.listener(|this, checked, _, cx| {
                                if this.profile_forms.subscription_route
                                    != RemoteProfileRoute::Mihomo
                                {
                                    this.profile_forms.subscription_route = if *checked {
                                        RemoteProfileRoute::DirectWithMihomoFallback
                                    } else {
                                        RemoteProfileRoute::Direct
                                    };
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_sm().child("失败自动回退"))
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(theme.muted_foreground)
                                    .child("直连失败后由内核代理重试"),
                            ),
                    ),
            )
    }

    pub(super) fn render_local_import(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        setting_card("本地 Clash YAML", theme).child(
            h_flex()
                .p_4()
                .gap_4()
                .child(
                    v_flex()
                        .flex_1()
                        .gap_1()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("导入到 ZenClash 配置仓库"),
                        )
                        .child(
                            div().text_xs().text_color(theme.muted_foreground).child(
                                "支持 .yaml / .yml Clash 与 Mihomo 配置；原文件不会被修改。",
                            ),
                        ),
                )
                .child(
                    Button::new("choose-profile")
                        .icon(IconName::FolderOpen)
                        .label("选择本地 YAML")
                        .outline()
                        .disabled(self.mutating)
                        .on_click(cx.listener(|this, _, _, cx| this.choose_profile(cx))),
                ),
        )
    }

    pub(super) fn render_current_profile(
        &self,
        config: &RuntimeConfig,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let path = self
            .profile_path
            .as_ref()
            .map_or_else(|| "未指定".into(), |path| path.display().to_string());
        let remote_count = self
            .profile_catalog
            .profiles
            .iter()
            .filter(|profile| profile.is_remote())
            .count();

        setting_card("当前真实配置", theme)
            .child(info_row("配置路径", &path, theme))
            .child(info_row("运行模式", &config.mode, theme))
            .child(info_row("日志等级", &config.log_level, theme))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .p_3()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "{} 份托管配置 · {} 份在线订阅",
                                self.profile_catalog.profiles.len(),
                                remote_count
                            )),
                    )
                    .child(
                        Button::new("reload-profile")
                            .icon(IconName::Redo2)
                            .label("热重载配置")
                            .primary()
                            .loading(self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| this.reload_profile(cx))),
                    ),
            )
    }
}

fn subscription_input(
    label: &'static str,
    input: Input,
    theme: &gpui_component::Theme,
) -> gpui::Div {
    v_flex()
        .flex_1()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(input)
}
