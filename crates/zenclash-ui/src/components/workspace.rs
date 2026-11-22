use gpui_kit::InteractiveElement;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::{ActiveTheme, v_flex};
use gpui_kit::{App, IntoElement, ParentElement, Styled, div, prelude::FluentBuilder};

use crate::foundation::actions::Page;

/// Title and supporting description aligned with the workspace content inset.
pub(crate) fn title(page: Page, cx: &mut App) -> impl IntoElement {
    let descriptor = page.descriptor();
    v_flex()
        .min_w_0()
        .gap_1()
        .child(
            div()
                .id("workspace-page-title")
                .test_support()
                .text_2xl()
                .when(!descriptor.is_settings, |title| {
                    title
                        .text_size(gpui_kit::rems(1.75))
                        .line_height(gpui_kit::relative(1.25))
                })
                .font_weight(gpui_kit::FontWeight::BOLD)
                .child(if descriptor.use_label_as_title {
                    page.label()
                } else {
                    page.title()
                }),
        )
        .when(descriptor.show_subtitle, |this| {
            this.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(page.subtitle()),
            )
        })
}
