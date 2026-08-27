use super::super::super::{
    Button, Context, Disableable, IconName, IntoElement, ParentElement, RuntimePage, Sizable,
    Styled, div, h_flex, info_row, px, setting_card, v_flex,
};

impl RuntimePage {
    pub(in crate::pages::runtime::settings) fn render_backup_card(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        setting_card(zenclash_i18n::text("backup.local.title"), theme)
            .child(info_row(
                zenclash_i18n::text("backup.local.contents"),
                zenclash_i18n::text("backup.local.contents_description"),
                theme,
            ))
            .child(
                div()
                    .px_4()
                    .py_3()
                    .text_xs()
                    .text_color(theme.warning)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(zenclash_i18n::text_with(
                        "backup.local.validation",
                        &[("core", self.core_kind.display_name().to_owned())],
                    )),
            )
            .child(
                h_flex()
                    .min_h(px(58.))
                    .px_4()
                    .gap_3()
                    .justify_between()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .child(zenclash_i18n::text("backup.local.snapshot")),
                            )
                            .child(
                                div().text_xs().text_color(theme.muted_foreground).child(
                                    zenclash_i18n::text("backup.local.snapshot_description"),
                                ),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("backup-export")
                                    .icon(IconName::File)
                                    .label(zenclash_i18n::text("backup.local.export"))
                                    .small()
                                    .disabled(self.mutating)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.choose_backup_export(cx);
                                    })),
                            )
                            .child(
                                Button::new("backup-import")
                                    .icon(IconName::FolderOpen)
                                    .label(zenclash_i18n::text("backup.local.import"))
                                    .small()
                                    .disabled(self.mutating)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.choose_backup_import(cx);
                                    })),
                            ),
                    ),
            )
    }
}
