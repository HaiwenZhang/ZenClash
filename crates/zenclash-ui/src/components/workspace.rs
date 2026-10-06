use gpui_kit::component::{
    ActiveTheme, IconName, Sizable,
    button::{Button, ButtonVariants},
    h_flex, v_flex,
};
use gpui_kit::{App, IntoElement, ParentElement, Styled, div, prelude::FluentBuilder};

use crate::{
    app::{SetDarkTheme, SetLightTheme},
    pages::Page,
};

/// Shared breadcrumb and appearance command for every workspace page.
pub(crate) fn breadcrumb(page: Page, cx: &mut App) -> impl IntoElement {
    let dark = cx.theme().mode.is_dark();
    h_flex()
        .w_full()
        .justify_between()
        .child(
            h_flex()
                .gap_3()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(zenclash_i18n::text("runtime.design.workspace"))
                .child("/")
                .child(div().text_color(cx.theme().foreground).child(page.label())),
        )
        .child(
            Button::new("workspace-appearance")
                .icon(if dark { IconName::Moon } else { IconName::Sun })
                .accessibility_label(zenclash_i18n::text(if dark {
                    "settings.appearance.light"
                } else {
                    "settings.appearance.dark"
                }))
                .tooltip(zenclash_i18n::text(if dark {
                    "settings.appearance.light"
                } else {
                    "settings.appearance.dark"
                }))
                .small()
                .ghost()
                .on_click(move |_, window, cx| {
                    if dark {
                        window.dispatch_action(Box::new(SetLightTheme), cx);
                    } else {
                        window.dispatch_action(Box::new(SetDarkTheme), cx);
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::{Root, ThemeMode};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        AppContext, Context, FocusHandle, InteractiveElement, Render, TestAppContext, Window, px,
        size,
    };

    struct WorkspaceHeader {
        focus: FocusHandle,
    }

    impl Render for WorkspaceHeader {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .track_focus(&self.focus)
                .on_action(|_: &SetLightTheme, window, cx| {
                    crate::design::apply_zen_theme(ThemeMode::Light, Some(window), cx);
                })
                .on_action(|_: &SetDarkTheme, window, cx| {
                    crate::design::apply_zen_theme(ThemeMode::Dark, Some(window), cx);
                })
                .child(breadcrumb(Page::Home, cx))
        }
    }

    #[gpui_kit::test]
    fn appearance_command_changes_theme_with_pointer_and_keyboard(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| crate::design::apply_zen_theme(ThemeMode::Light, None, cx));
        let mut header = None;
        let window = cx.open_window(size(px(600.), px(120.)), |window, cx| {
            let view = cx.new(|cx| WorkspaceHeader {
                focus: cx.focus_handle(),
            });
            header = Some(view.clone());
            Root::new(view, window, cx)
        });
        cx.update_window(window.into(), |_, window, cx| {
            let focus = header.as_ref().unwrap().read(cx).focus.clone();
            window.focus(&focus, cx);
            window.render_frame(cx);
            assert_eq!(
                window.find("workspace-appearance").label(),
                Some(zenclash_i18n::text("settings.appearance.dark").as_str())
            );
            window.click("workspace-appearance", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            assert!(cx.theme().mode.is_dark());
            window.render_frame(cx);
            assert_eq!(
                window.find("workspace-appearance").label(),
                Some(zenclash_i18n::text("settings.appearance.light").as_str())
            );
            let focus = header.as_ref().unwrap().read(cx).focus.clone();
            window.focus(&focus, cx);
            window.press("tab", cx);
            assert_eq!(window.find("workspace-appearance").focused(), Some(true));
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            assert!(!cx.theme().mode.is_dark());
            window.remove_window();
        })
        .unwrap();
    }
}
