//! Offline access to the actual licenses and retained Fork notices.

use super::super::{RuntimePage, setting_card};
use gpui_kit::component::{WindowExt, button::Button, scroll::ScrollableElement, v_flex};
use gpui_kit::{Context, InteractiveElement, ParentElement, Styled, TestSupportExt, div};

const LEGAL_TEXT: &str = concat!(
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../NOTICE.md")),
    "\n\n",
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../zenclash-service/NOTICE.md"
    )),
    "\n\n",
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../zenclash-service-integration/NOTICE.md"
    )),
    "\n\n",
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../LICENSE")),
);

impl RuntimePage {
    pub(super) fn render_license_info(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        setting_card(zenclash_i18n::text("settings.legal.title"), theme).child(
            v_flex()
                .px_4()
                .pb_4()
                .gap_2()
                .child(div().text_sm().child("GPL-3.0-only"))
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(zenclash_i18n::text("settings.legal.rights")),
                )
                .child(
                    Button::new("settings-license-notices")
                        .label(zenclash_i18n::text("settings.legal.read"))
                        .outline()
                        .on_click(cx.listener(|this, _, window, cx| {
                            if window.has_active_dialog(cx) {
                                return;
                            }
                            if window.focused(cx).is_none() {
                                window.focus(&this.focus_handle, cx);
                            }
                            window.open_dialog(cx, |dialog, _, _| {
                                dialog
                                    .title(zenclash_i18n::text("settings.legal.title"))
                                    .child(
                                        div().id("license-notices-scroll").test_support().child(
                                            v_flex()
                                                .max_h(gpui_kit::rems(24.))
                                                .min_w_0()
                                                .gap_3()
                                                .children(
                                                    LEGAL_TEXT
                                                        .split("\n\n")
                                                        .filter(|paragraph| !paragraph.is_empty())
                                                        .map(|paragraph| {
                                                            div()
                                                                .text_sm()
                                                                .min_w_0()
                                                                .child(paragraph.to_owned())
                                                        }),
                                                )
                                                .overflow_y_scrollbar()
                                                .id("license-notices-area"),
                                        ),
                                    )
                            });
                        })),
                ),
        )
    }
}
