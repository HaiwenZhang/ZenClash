use super::super::super::{
    div, h_flex, info_row, px, setting_card, v_flex, Button, Context, Disableable, IconName,
    IntoElement, ParentElement, RuntimePage, Sizable, Styled,
};

impl RuntimePage {
    pub(in crate::pages::runtime::settings) fn render_backup_card(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        setting_card("本地备份与恢复", theme)
            .child(info_row(
                "备份内容",
                "应用偏好 · 受控覆写 · 订阅与本地 YAML",
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
                    .child(format!(
                        "导入前会校验 ZIP 路径、文件白名单、SHA-256 和全部 Clash YAML；{} 拒绝时自动恢复原数据。",
                        self.core_kind.display_name()
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
                            .child(div().text_sm().child("完整本地快照"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("恢复成功后立即应用主题、状态栏可见性和活动配置"),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("backup-export")
                                    .icon(IconName::File)
                                    .label("导出 ZIP")
                                    .small()
                                    .disabled(self.mutating)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.choose_backup_export(cx);
                                    })),
                            )
                            .child(
                                Button::new("backup-import")
                                    .icon(IconName::FolderOpen)
                                    .label("导入 ZIP")
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
