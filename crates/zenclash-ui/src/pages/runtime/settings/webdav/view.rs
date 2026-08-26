use super::super::super::{
    compact_text, config_input_row, div, h_flex, px, setting_card, setting_switch, v_flex, Button,
    ButtonVariants, Context, Disableable, Icon, IconName, Input, IntoElement, ParentElement,
    RuntimePage, Sizable, Styled,
};
use super::super::backup::format_backup_size;

impl RuntimePage {
    pub(in crate::pages::runtime::settings) fn render_webdav_card(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut card = setting_card("WebDAV 远端保险库", theme)
            .child(self.render_webdav_status(theme))
            .child(self.render_webdav_fields(theme, cx))
            .child(self.render_webdav_actions(theme, cx))
            .child(webdav_storage_note(theme));

        if self.webdav.backups.is_empty() {
            return card.child(
                div()
                    .p_5()
                    .text_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(if self.webdav.verified {
                        "远端目录为空；点击“立即备份”创建第一份快照"
                    } else {
                        "保存并测试连接后，这里会显示可恢复的 ZIP 快照"
                    }),
            );
        }

        for (index, backup) in self.webdav.backups.iter().enumerate() {
            card = card.child(self.render_webdav_backup(index, backup, theme, cx));
        }
        card
    }

    fn render_webdav_fields(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        v_flex()
            .child(config_input_row(
                "服务器 URL",
                "填写 WebDAV 根地址；凭据不能写在 URL 中",
                Input::new(&self.webdav.url),
                theme,
            ))
            .child(config_input_row(
                "远程目录",
                "ZenClash 会逐级创建这个相对目录",
                Input::new(&self.webdav.directory),
                theme,
            ))
            .child(config_input_row(
                "用户名",
                "留空时不发送 HTTP Basic 凭据",
                Input::new(&self.webdav.username),
                theme,
            ))
            .child(config_input_row(
                "密码",
                "优先使用服务商提供的独立应用密码",
                Input::new(&self.webdav.password).mask_toggle(),
                theme,
            ))
            .child(config_input_row(
                "保留份数",
                "只清理当前设备生成的旧备份；0 表示不限",
                Input::new(&self.webdav.max_backups),
                theme,
            ))
            .child(config_input_row(
                "定时备份",
                "使用本地时区的 Cron；支持常用 5 字段，留空即停用",
                Input::new(&self.webdav.backup_cron),
                theme,
            ))
            .child(setting_switch(
                "允许无效 TLS 证书",
                "仅用于你确认可信的自签名服务器",
                self.webdav.accept_invalid_certificates,
                "webdav-invalid-tls",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.webdav.accept_invalid_certificates = *checked;
                    this.webdav.verified = false;
                    this.webdav.dirty = true;
                    cx.notify();
                }),
            ))
    }

    fn render_webdav_actions(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        h_flex()
            .min_h(px(62.))
            .px_4()
            .gap_3()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child("手动同步"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("上传真实本地快照；下载后先验证，再事务切换活动配置"),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("webdav-save-test")
                            .icon(IconName::Globe)
                            .label("保存并测试")
                            .small()
                            .outline()
                            .disabled(self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| this.test_webdav(cx))),
                    )
                    .child(
                        Button::new("webdav-upload")
                            .icon(IconName::ArrowUp)
                            .label("立即备份")
                            .small()
                            .primary()
                            .disabled(self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| this.upload_webdav_backup(cx))),
                    )
                    .child(
                        Button::new("webdav-refresh")
                            .icon(IconName::Redo2)
                            .label("刷新")
                            .small()
                            .ghost()
                            .disabled(self.mutating)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.refresh_webdav_backups(cx)),
                            ),
                    ),
            )
    }

    fn render_webdav_status(&self, theme: &gpui_component::Theme) -> gpui::Div {
        let (label, detail, color) = if self.webdav.verified {
            (
                "连接已验证",
                format!("已发现 {} 份安全 ZIP", self.webdav.backups.len()),
                theme.success,
            )
        } else {
            (
                "等待验证",
                "不会在未验证服务器前自动上传".to_owned(),
                theme.warning,
            )
        };
        h_flex()
            .relative()
            .min_h(px(58.))
            .px_4()
            .gap_3()
            .border_b_1()
            .border_color(theme.border)
            .overflow_hidden()
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(3.))
                    .bg(color),
            )
            .child(
                div()
                    .size(px(30.))
                    .rounded(theme.radius)
                    .bg(color.opacity(0.14))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(IconName::FolderClosed).size_4().text_color(color)),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(label),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(detail),
                    ),
            )
    }

    fn render_webdav_backup(
        &self,
        index: usize,
        backup: &zenclash_core::WebDavBackup,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let restore_name = backup.filename.clone();
        let delete_name = backup.filename.clone();
        let size = backup
            .size_bytes
            .map_or_else(|| "大小未知".into(), format_backup_size);
        let modified = backup.modified.as_deref().unwrap_or("时间未知");
        h_flex()
            .min_h(px(64.))
            .px_4()
            .py_3()
            .gap_3()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_family(theme.mono_font_family.clone())
                            .child(compact_text(&backup.filename, 72)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{size} · {}", compact_text(modified, 48))),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new(("webdav-restore", index))
                            .icon(IconName::ArrowDown)
                            .label("恢复")
                            .small()
                            .outline()
                            .disabled(self.mutating)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_webdav_restore(restore_name.clone(), window, cx);
                            })),
                    )
                    .child(
                        Button::new(("webdav-delete", index))
                            .icon(IconName::Delete)
                            .small()
                            .ghost()
                            .danger()
                            .disabled(self.mutating)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_webdav_delete(delete_name.clone(), window, cx);
                            })),
                    ),
            )
    }
}

fn webdav_storage_note(theme: &gpui_component::Theme) -> gpui::Div {
    div()
        .px_4()
        .py_3()
        .text_xs()
        .text_color(theme.muted_foreground)
        .border_b_1()
        .border_color(theme.border)
        .child("连接设置保存在当前用户私有文件中，不写入备份 ZIP；定时任务在应用运行期间按本地时区执行。")
}
