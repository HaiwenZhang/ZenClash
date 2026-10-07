use gpui_kit::component::{ActiveTheme, v_flex};
use gpui_kit::{App, IntoElement, ParentElement, Styled, div, prelude::FluentBuilder};

use crate::pages::Page;

/// Title and supporting description aligned with the workspace content inset.
pub(crate) fn title(page: Page, cx: &mut App) -> impl IntoElement {
    v_flex()
        .min_w_0()
        .gap_1()
        .child(
            div()
                .text_2xl()
                .when(page != Page::Settings, |title| {
                    title
                        .text_size(gpui_kit::px(28.))
                        .line_height(gpui_kit::relative(1.25))
                })
                .font_weight(gpui_kit::FontWeight::BOLD)
                .child(if page == Page::Logs {
                    page.label()
                } else {
                    page.title()
                }),
        )
        .when(page == Page::Rules, |this| {
            this.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(page.subtitle()),
            )
        })
}
