use super::super::super::{
    config_input_row, div, h_flex, px, setting_card, setting_switch, v_flex, Button,
    ButtonVariants, Context, Disableable, IconName, Input, ParentElement, RemoteProfileRoute,
    RuntimePage, Sizable, Styled,
};

impl RuntimePage {
    pub(super) fn render_remote_profile_editor(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let name = self
            .profile_forms
            .editing_profile_id
            .as_deref()
            .and_then(|id| {
                self.profile_catalog
                    .profiles
                    .iter()
                    .find(|profile| profile.id == id)
            })
            .map_or("在线订阅", |profile| profile.name.as_str());
        setting_card("编辑订阅请求", theme)
            .child(
                h_flex()
                    .min_h(px(48.))
                    .px_4()
                    .justify_between()
                    .child(div().text_sm().child(name.to_owned()))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme.muted_foreground)
                            .child("保存后下次手动与后台更新立即使用"),
                    ),
            )
            .child(config_input_row(
                "订阅名称",
                "更新展示名称，不改变托管文件 ID",
                Input::new(&self.profile_forms.request_name),
                theme,
            ))
            .child(config_input_row(
                "订阅 URL",
                "仅支持不含嵌入凭据的 HTTP(S) 地址",
                Input::new(&self.profile_forms.request_url),
                theme,
            ))
            .child(config_input_row(
                "User-Agent",
                "留空使用 clash.meta",
                Input::new(&self.profile_forms.request_user_agent),
                theme,
            ))
            .child(config_input_row(
                "Authorization",
                "Bearer / Basic 等完整头值；留空会删除已保存凭据",
                Input::new(&self.profile_forms.request_authorization).mask_toggle(),
                theme,
            ))
            .child(config_input_row(
                "下载超时（秒）",
                "单个订阅请求允许 1–600 秒；默认 30 秒",
                Input::new(&self.profile_forms.request_timeout_seconds),
                theme,
            ))
            .child(self.render_remote_profile_route_settings(theme, cx))
            .child(config_input_row(
                "5 字段 Cron",
                "本地时间：分 时 日 月 周；例如 0 */6 * * *。留空恢复分钟间隔计划",
                Input::new(&self.profile_forms.update_cron),
                theme,
            ))
            .child(setting_switch(
                "锁定更新间隔",
                "忽略服务端 profile-update-interval，保留本地间隔或 Cron",
                self.profile_forms.editing_fixed_update_interval,
                "edit-profile-fixed-update-interval",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.profile_forms.editing_fixed_update_interval = *checked;
                    cx.notify();
                }),
            ))
            .child(self.render_remote_profile_editor_actions(cx))
    }

    fn render_remote_profile_route_settings(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        v_flex()
            .child(setting_switch(
                "经内核代理下载",
                "使用更新时的 HTTP 或 Mixed 监听端口",
                self.profile_forms.editing_route == RemoteProfileRoute::Mihomo,
                "edit-profile-use-mihomo-proxy",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.profile_forms.editing_route = if *checked {
                        RemoteProfileRoute::Mihomo
                    } else {
                        RemoteProfileRoute::DirectWithMihomoFallback
                    };
                    cx.notify();
                }),
            ))
            .child(setting_switch(
                "直连失败自动回退",
                "直连网络请求失败时，经当前内核 HTTP/Mixed 端口重试一次",
                self.profile_forms.editing_route == RemoteProfileRoute::DirectWithMihomoFallback,
                "edit-profile-mihomo-fallback",
                theme,
                cx.listener(|this, checked, _, cx| {
                    if this.profile_forms.editing_route != RemoteProfileRoute::Mihomo {
                        this.profile_forms.editing_route = if *checked {
                            RemoteProfileRoute::DirectWithMihomoFallback
                        } else {
                            RemoteProfileRoute::Direct
                        };
                    }
                    cx.notify();
                }),
            ))
    }

    fn render_remote_profile_editor_actions(&self, cx: &mut Context<Self>) -> gpui::Div {
        h_flex()
            .px_4()
            .py_3()
            .gap_2()
            .justify_end()
            .child(
                Button::new("cancel-profile-request-edit")
                    .label("取消")
                    .small()
                    .ghost()
                    .disabled(self.mutating)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.cancel_edit_remote_profile(cx);
                    })),
            )
            .child(
                Button::new("save-profile-request-edit")
                    .icon(IconName::Check)
                    .label("保存请求与计划")
                    .small()
                    .primary()
                    .loading(self.mutating)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.save_remote_profile_settings(cx);
                    })),
            )
    }
}
