use crate::components::page_primitives::info_row;
use crate::components::page_primitives::setting_card;
use crate::features::host::RuntimePage;
use gpui_kit::Context;
use gpui_kit::IntoElement;
use gpui_kit::ParentElement;
use gpui_kit::Styled;
use gpui_kit::component::Disableable;
use gpui_kit::component::IconName;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::Button;
use gpui_kit::component::h_flex;
use gpui_kit::component::v_flex;
use gpui_kit::div;
use gpui_kit::prelude::FluentBuilder;

impl RuntimePage {
    pub(in crate::features::settings) fn render_backup_card(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let recovering = self
            .mutations
            .active(crate::runtime_host::busy::MutationDomain::BackupRecovery);
        setting_card(zenclash_i18n::text("backup.local.title"), theme)
            .child(info_row(
                zenclash_i18n::text("backup.local.contents"),
                zenclash_i18n::text("settings_redesign.backup_contents"),
                theme,
            ))
            .when(
                self.application_commands.pending_backup_restore().is_some() || recovering,
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
                                        crate::runtime_host::busy::MutationDomain::Backup,
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
                    .min_h(gpui_kit::rems(4.5))
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
                                    .icon(crate::foundation::assets::AppIcon::SquareArrowRightExit)
                                    .label(zenclash_i18n::text("backup.local.export"))
                                    .small()
                                    .disabled(self.mutation_busy(
                                        crate::runtime_host::busy::MutationDomain::Backup,
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
                                        crate::runtime_host::busy::MutationDomain::Backup,
                                    ))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.choose_backup_import(cx);
                                    })),
                            ),
                    ),
            )
    }
}
