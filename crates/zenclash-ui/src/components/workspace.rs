use gpui_kit::component::{
    ActiveTheme, Selectable, Sizable,
    button::{Button, ButtonVariants},
    h_flex, v_flex,
};
use gpui_kit::{App, IntoElement, ParentElement, Styled, div, prelude::FluentBuilder};

use crate::pages::Page;

/// Keep diagnostics reachable from the unified Rules destination at every width.
pub(crate) fn diagnostics_navigation(page: Page) -> impl IntoElement {
    h_flex()
        .gap_2()
        .flex_wrap()
        .children(
            [Page::Rules, Page::Logs, Page::Network]
                .into_iter()
                .map(|destination| {
                    Button::new((
                        gpui_kit::ElementId::from("diagnostics-navigation"),
                        destination.route(),
                    ))
                    .label(destination.label())
                    .small()
                    .ghost()
                    .selected(page == destination)
                    .on_click(move |_, window, cx| {
                        crate::components::sidebar::dispatch_navigate(destination, window, cx);
                    })
                }),
        )
}

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
                .child(page.title()),
        )
        .when(page == Page::Settings, |this| {
            this.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(page.subtitle()),
            )
        })
}
