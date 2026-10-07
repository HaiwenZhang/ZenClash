use super::super::super::{
    Button, Context, Disableable, FluentBuilder, IconName, IntoElement, ParentElement, RuntimePage,
    Sizable, Styled, div, h_flex, info_row, px, setting_card, v_flex,
};

impl RuntimePage {
    pub(in crate::pages::runtime::settings) fn render_backup_card(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let recovering = self
            .mutations
            .active(crate::pages::runtime::busy::MutationDomain::BackupRecovery);
        setting_card(zenclash_i18n::text("backup.local.title"), theme)
            .child(info_row(
                zenclash_i18n::text("backup.local.contents"),
                zenclash_i18n::text("settings_redesign.backup_contents"),
                theme,
            ))
            .when(
                self.profile_service.pending_backup_restore().is_some() || recovering,
                |this| {
                    this.child(
                        h_flex()
                            .items_start()
                            .px_4()
                            .py_3()
                            .gap_3()
                            .justify_between()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .child(div().text_sm().text_color(theme.warning).child(
                                        zenclash_i18n::text(if recovering {
                                            "backup.local.retrying_runtime"
                                        } else {
                                            "backup.local.runtime_pending"
                                        }),
                                    ))
                                    .when(!recovering, |this| {
                                        this.child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .child(zenclash_i18n::text(
                                                    "backup.local.runtime_pending_description",
                                                )),
                                        )
                                    }),
                            )
                            .child(
                                Button::new("backup-retry-runtime")
                                    .label(zenclash_i18n::text("backup.local.retry_runtime"))
                                    .small()
                                    .loading(recovering)
                                    .disabled(self.mutation_busy(
                                        crate::pages::runtime::busy::MutationDomain::Backup,
                                    ))
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.retry_backup_runtime(cx)),
                                    ),
                            ),
                    )
                },
            )
            .child(
                h_flex()
                    .min_h(px(72.))
                    .px_4()
                    .gap_3()
                    .flex_wrap()
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
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(zenclash_i18n::text("settings_redesign.backup_summary")),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("backup-export")
                                    .icon(crate::assets::AppIcon::SquareArrowRightExit)
                                    .label(zenclash_i18n::text("backup.local.export"))
                                    .small()
                                    .disabled(self.mutation_busy(
                                        crate::pages::runtime::busy::MutationDomain::Backup,
                                    ))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.choose_backup_export(cx);
                                    })),
                            )
                            .child(
                                Button::new("backup-import")
                                    .icon(IconName::FolderOpen)
                                    .label(zenclash_i18n::text("backup.local.import"))
                                    .small()
                                    .disabled(self.mutation_busy(
                                        crate::pages::runtime::busy::MutationDomain::Backup,
                                    ))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.choose_backup_import(cx);
                                    })),
                            ),
                    ),
            )
    }
}
