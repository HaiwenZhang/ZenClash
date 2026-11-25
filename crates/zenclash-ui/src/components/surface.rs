//! Shared surface, detail-row, status-pill, table-header, and copy primitives.
//!
//! All primitives consume semantic `gpui_kit::component::Theme` tokens and have
//! zero dependency on `crate::app` or `crate::pages`.

use gpui_kit::component::{
    IconName, Sizable, Theme, WindowExt, button::Button, h_flex, notification::Notification, v_flex,
};
use gpui_kit::{
    Anchor, ClipboardItem, Div, ElementId, IntoElement, ParentElement, Rems, SharedString, Styled,
    div, rems,
};

/// Semantic color tone for [`status_pill`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StatusTone {
    /// Neutral muted pill for secondary metadata.
    #[default]
    Neutral,
    /// Primary brand accent tone.
    Accent,
    /// Healthy, enabled, or connected status.
    Success,
    /// Attention or degraded status.
    Warning,
    /// Failure, timeout, or destructive status.
    Danger,
    /// Informational badge tone.
    Info,
}

/// Standard rounded surface card used across workspace panels and settings sections.
#[must_use]
pub fn surface_card(theme: &Theme) -> Div {
    v_flex()
        .min_w_0()
        .gap_3()
        .p_4()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.group_box)
}

/// Two-column key/value row aligned to a consistent label spine.
#[must_use]
pub fn detail_row(label: impl Into<SharedString>, value: impl IntoElement, theme: &Theme) -> Div {
    h_flex()
        .gap_3()
        .py_1()
        .items_start()
        .text_sm()
        .child(
            div()
                .w(rems(7.))
                .flex_shrink_0()
                .text_color(theme.muted_foreground)
                .child(label.into()),
        )
        .child(h_flex().flex_1().min_w_0().child(value))
}

/// Compact semantic status pill styled strictly from theme tokens.
#[must_use]
pub fn status_pill(label: impl Into<SharedString>, tone: StatusTone, theme: &Theme) -> Div {
    let (foreground, background, border) = match tone {
        StatusTone::Neutral => (theme.muted_foreground, theme.muted, theme.border),
        StatusTone::Accent => (
            theme.primary,
            theme.primary.opacity(0.12),
            theme.primary.opacity(0.28),
        ),
        StatusTone::Success => (
            theme.success,
            theme.success.opacity(0.12),
            theme.success.opacity(0.28),
        ),
        StatusTone::Warning => (
            theme.warning,
            theme.warning.opacity(0.14),
            theme.warning.opacity(0.32),
        ),
        StatusTone::Danger => (
            theme.danger,
            theme.danger.opacity(0.12),
            theme.danger.opacity(0.30),
        ),
        StatusTone::Info => (
            theme.info,
            theme.info.opacity(0.12),
            theme.info.opacity(0.28),
        ),
    };
    div()
        .flex()
        .items_center()
        .flex_shrink_0()
        .px_2()
        .py_0p5()
        .rounded(theme.radius)
        .border_1()
        .border_color(border)
        .bg(background)
        .text_xs()
        .text_color(foreground)
        .child(label.into())
}

/// Fixed-width non-shrinking table header cell keeping header and body columns aligned.
#[must_use]
pub fn table_fixed_header_cell(width: Rems, label: impl Into<SharedString>) -> Div {
    div()
        .w(width)
        .flex_shrink_0()
        .truncate()
        .text_sm()
        .child(label.into())
}

/// Outline copy button that copies `content` to the clipboard and shows a toast notification.
#[must_use]
pub fn copy_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    content: impl Into<String>,
    copied_message: impl Into<String>,
) -> Button {
    let label = label.into();
    let content = content.into();
    let copied_message = copied_message.into();
    Button::new(id)
        .icon(IconName::Copy)
        .label(label.clone())
        .accessibility_label(label)
        .small()
        .outline()
        .on_click(move |_, window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(content.clone()));
            window.push_notification(
                Notification::info(copied_message.clone()).placement(Anchor::BottomRight),
                cx,
            );
        })
}

#[cfg(test)]
mod tests {
    use super::{
        StatusTone, copy_button, detail_row, status_pill, surface_card, table_fixed_header_cell,
    };
    use gpui_kit::component::{ActiveTheme, Root};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        AppContext, Context, IntoElement, ParentElement, Render, Styled, Window, div, px, rems,
        size,
    };

    struct SurfaceHost;

    impl Render for SurfaceHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = cx.theme().clone();
            div().size_full().child(
                surface_card(&theme)
                    .child(detail_row(
                        "Status",
                        status_pill("Online", StatusTone::Success, &theme),
                        &theme,
                    ))
                    .child(table_fixed_header_cell(rems(6.), "Protocol"))
                    .child(copy_button(
                        "surface-copy-test",
                        "Copy",
                        "payload",
                        "Copied",
                    )),
            )
        }
    }

    #[gpui_kit::test]
    fn copy_button_writes_clipboard_and_renders_surface_primitives(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let handle = cx.open_window(size(px(480.), px(240.)), |window, cx| {
            let view = cx.new(|_| SurfaceHost);
            Root::new(view, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("surface-copy-test", cx);
            assert_eq!(
                cx.read_from_clipboard().and_then(|item| item.text()),
                Some("payload".to_owned())
            );
            window.remove_window();
        })
        .unwrap();
    }
}
