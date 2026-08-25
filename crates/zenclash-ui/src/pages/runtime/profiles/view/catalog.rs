use zenclash_core::{ProfileRecord, ProfileSource};

use super::super::super::{
    compact_text, div, empty_state, format_bytes, format_profile_age, h_flex, px, setting_card,
    v_flex, Button, ButtonVariants, Context, Disableable, FluentBuilder, IconName, ParentElement,
    RuntimePage, Sizable, Styled,
};

impl RuntimePage {
    pub(super) fn render_managed_profiles(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mut card = setting_card("配置仓库", theme);
        if self.profile_catalog.profiles.is_empty() {
            return card.child(empty_state(
                "还没有托管配置；可下载在线订阅或导入本地 YAML",
                theme,
            ));
        }

        for (index, profile) in self.profile_catalog.profiles.iter().enumerate() {
            card = card.child(self.render_managed_profile(index, profile, theme, cx));
        }
        card
    }

    fn render_managed_profile(
        &self,
        index: usize,
        profile: &ProfileRecord,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let active = self.profile_catalog.active.as_deref() == Some(profile.id.as_str());
        let activate_id = profile.id.clone();
        let update_id = profile.id.clone();
        let delete_id = profile.id.clone();
        let source = profile_source(&profile.source);

        v_flex()
            .px_4()
            .py_3()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(profile_heading(profile, active, theme))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(source),
            )
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme.muted_foreground)
                            .child(format!("更新于 {}", format_profile_age(profile.updated_at))),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .when(profile.is_remote(), |this| {
                                this.child(
                                    Button::new(("update-profile", index))
                                        .icon(IconName::Redo2)
                                        .label("更新")
                                        .small()
                                        .outline()
                                        .disabled(self.mutating)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.update_managed_profile(update_id.clone(), cx);
                                        })),
                                )
                            })
                            .child(
                                Button::new(("activate-profile", index))
                                    .icon(IconName::ArrowRight)
                                    .label(if active { "使用中" } else { "切换" })
                                    .small()
                                    .primary()
                                    .disabled(active || self.mutating)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.activate_managed_profile(activate_id.clone(), cx);
                                    })),
                            )
                            .child(
                                Button::new(("delete-profile", index))
                                    .icon(IconName::Delete)
                                    .small()
                                    .ghost()
                                    .danger()
                                    .disabled(active || self.mutating)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.delete_managed_profile(delete_id.clone(), cx);
                                    })),
                            ),
                    ),
            )
    }
}

fn profile_source(source: &ProfileSource) -> String {
    match source {
        ProfileSource::Local { original_path } => compact_text(original_path, 76),
        ProfileSource::Remote { url, user_agent } => {
            format!("{} · UA {user_agent}", compact_text(url, 62))
        }
    }
}

fn profile_heading(
    profile: &ProfileRecord,
    active: bool,
    theme: &gpui_component::Theme,
) -> gpui::Div {
    h_flex()
        .gap_2()
        .child(
            div()
                .size_2()
                .rounded_full()
                .bg(if active { theme.success } else { theme.primary }),
        )
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(profile.name.clone()),
        )
        .child(
            div()
                .px_2()
                .py_0p5()
                .rounded_full()
                .bg(if active {
                    theme.success.opacity(0.14)
                } else {
                    theme.muted.opacity(0.5)
                })
                .text_size(px(10.))
                .text_color(if active {
                    theme.success
                } else {
                    theme.muted_foreground
                })
                .child(if active {
                    "当前使用"
                } else {
                    profile.source_label()
                }),
        )
        .child(div().flex_1())
        .child(
            div()
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format_bytes(profile.size_bytes)),
        )
}
