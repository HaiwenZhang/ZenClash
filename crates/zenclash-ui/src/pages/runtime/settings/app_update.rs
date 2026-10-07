use super::super::{RuntimePage, info_row, setting_card};
use gpui_kit::component::{
    Sizable, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
};
use gpui_kit::{Context, ParentElement, Styled};

impl RuntimePage {
    pub(super) fn render_app_update(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        setting_card(
            zenclash_i18n::text("settings_redesign.version_updates"),
            theme,
        )
        .child(info_row(
            zenclash_i18n::text("settings.app_update.current"),
            env!("ZENCLASH_BUILD_VERSION"),
            theme,
        ))
        .child(
            h_flex().justify_end().p_4().child(
                Button::new("check-app-update")
                    .icon(crate::assets::AppIcon::RefreshCw)
                    .label(zenclash_i18n::text("settings.app_update.check"))
                    .small()
                    .outline()
                    .on_click(cx.listener(|_, _, window, cx| {
                        window.open_dialog(cx, |dialog, _, _| {
                            dialog
                                .title(zenclash_i18n::text("settings.app_update.check"))
                                .child(zenclash_i18n::text(
                                    "settings_redesign.update_unimplemented",
                                ))
                                .footer(
                                    gpui_kit::component::h_flex().justify_end().child(
                                        Button::new("app-update-close")
                                            .primary()
                                            .label(zenclash_i18n::text("common.actions.close"))
                                            .on_click(|_, window, cx| window.close_dialog(cx)),
                                    ),
                                )
                        });
                    })),
            ),
        )
    }
}
