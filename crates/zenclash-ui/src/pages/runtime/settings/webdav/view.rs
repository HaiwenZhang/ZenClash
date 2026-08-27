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
        let mut card = setting_card(zenclash_i18n::text("webdav.title"), theme)
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
                        zenclash_i18n::text("webdav.empty.verified")
                    } else {
                        zenclash_i18n::text("webdav.empty.unverified")
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
                zenclash_i18n::text("webdav.fields.url"),
                zenclash_i18n::text("webdav.fields.url_description"),
                Input::new(&self.webdav.url),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("webdav.fields.directory"),
                zenclash_i18n::text("webdav.fields.directory_description"),
                Input::new(&self.webdav.directory),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("webdav.fields.username"),
                zenclash_i18n::text("webdav.fields.username_description"),
                Input::new(&self.webdav.username),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("webdav.fields.password"),
                zenclash_i18n::text("webdav.fields.password_description"),
                Input::new(&self.webdav.password).mask_toggle(),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("webdav.fields.retention"),
                zenclash_i18n::text("webdav.fields.retention_description"),
                Input::new(&self.webdav.max_backups),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("webdav.fields.schedule"),
                zenclash_i18n::text("webdav.fields.schedule_description"),
                Input::new(&self.webdav.backup_cron),
                theme,
            ))
            .child(setting_switch(
                zenclash_i18n::text("webdav.fields.invalid_tls"),
                zenclash_i18n::text("webdav.fields.invalid_tls_description"),
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
                    .child(
                        div()
                            .text_sm()
                            .child(zenclash_i18n::text("webdav.actions.manual")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("webdav.actions.manual_description")),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("webdav-save-test")
                            .icon(IconName::Globe)
                            .label(zenclash_i18n::text("webdav.actions.save_test"))
                            .small()
                            .outline()
                            .disabled(self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| this.test_webdav(cx))),
                    )
                    .child(
                        Button::new("webdav-upload")
                            .icon(IconName::ArrowUp)
                            .label(zenclash_i18n::text("webdav.actions.backup_now"))
                            .small()
                            .primary()
                            .disabled(self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| this.upload_webdav_backup(cx))),
                    )
                    .child(
                        Button::new("webdav-refresh")
                            .icon(IconName::Redo2)
                            .label(zenclash_i18n::text("webdav.actions.refresh"))
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
                zenclash_i18n::text("webdav.status.verified"),
                zenclash_i18n::text_with(
                    "webdav.status.found",
                    &[("count", self.webdav.backups.len().to_string())],
                ),
                theme.success,
            )
        } else {
            (
                zenclash_i18n::text("webdav.status.waiting"),
                zenclash_i18n::text("webdav.status.waiting_description"),
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
        let size = backup.size_bytes.map_or_else(
            || zenclash_i18n::text("webdav.status.size_unknown"),
            format_backup_size,
        );
        let modified = backup
            .modified
            .clone()
            .unwrap_or_else(|| zenclash_i18n::text("webdav.status.time_unknown"));
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
                            .child(format!("{size} · {}", compact_text(&modified, 48))),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new(("webdav-restore", index))
                            .icon(IconName::ArrowDown)
                            .label(zenclash_i18n::text("webdav.actions.restore"))
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
        .child(zenclash_i18n::text("webdav.storage_note"))
}
