//! Stateless form row, status card, dialog footer, and choice popover building blocks.

mod choice;
pub(crate) use choice::ChoicePopover;

use gpui_kit::component::{
    WindowExt,
    button::{Button, ButtonVariants},
    dialog::DialogAction,
    h_flex,
    input::AnyInputState,
    v_flex,
};
use gpui_kit::{IntoElement, ParentElement, Styled, div};

pub(crate) fn settings_dialog_footer(label: String) -> gpui_kit::Div {
    h_flex()
        .justify_end()
        .gap_2()
        .child(
            Button::new("cancel")
                .outline()
                .label(zenclash_i18n::text("common.actions.cancel"))
                .on_click(|_, window, cx| window.close_dialog(cx)),
        )
        .child(div().child(DialogAction::new().child(Button::new("ok").primary().label(label))))
}

pub(crate) fn settings_status<F>(
    title: impl Into<gpui_kit::SharedString>,
    description: impl Into<gpui_kit::SharedString>,
    checked: bool,
    id: &'static str,
    theme: &gpui_kit::component::Theme,
    listener: F,
) -> gpui_kit::Div
where
    F: Fn(&bool, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
{
    let title = title.into();
    v_flex()
        .p_5()
        .gap_3()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .child(
            h_flex()
                .gap_4()
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(title.clone()),
                )
                .child(
                    crate::components::mint_switch::MintSwitch::new(id)
                        .accessibility_label(title)
                        .checked(checked)
                        .on_click(listener),
                ),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(description.into()),
        )
}

fn visible_description(description: gpui_kit::SharedString) -> Option<gpui_kit::SharedString> {
    (!description.trim().is_empty()).then_some(description)
}

fn settings_row_heading(
    label: gpui_kit::SharedString,
    description: gpui_kit::SharedString,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    let mut heading = v_flex()
        .flex_1()
        .min_w_0()
        .gap_1()
        .child(div().text_sm().child(label));
    if let Some(description) = visible_description(description) {
        heading = heading.child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(description),
        );
    }
    heading
}

pub(crate) fn settings_input_row(
    label: impl Into<gpui_kit::SharedString>,
    description: impl Into<gpui_kit::SharedString>,
    input: impl IntoElement,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::AnyElement {
    h_flex()
        .min_w_0()
        .min_h_12()
        .px_4()
        .py_2()
        .gap_4()
        .border_b_1()
        .border_color(theme.border)
        .child(settings_row_heading(
            label.into(),
            description.into(),
            theme,
        ))
        .child(div().flex_1().min_w_0().child(input))
        .into_any_element()
}

pub(crate) fn settings_switch<F>(
    label: impl Into<gpui_kit::SharedString>,
    description: impl Into<gpui_kit::SharedString>,
    checked: bool,
    id: &'static str,
    theme: &gpui_kit::component::Theme,
    listener: F,
) -> gpui_kit::AnyElement
where
    F: Fn(&bool, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
{
    settings_switch_disabled(label, description, checked, id, theme, false, listener)
}

pub(crate) fn settings_switch_disabled<F>(
    label: impl Into<gpui_kit::SharedString>,
    description: impl Into<gpui_kit::SharedString>,
    checked: bool,
    id: &'static str,
    theme: &gpui_kit::component::Theme,
    disabled: bool,
    listener: F,
) -> gpui_kit::AnyElement
where
    F: Fn(&bool, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
{
    let label = label.into();
    h_flex()
        .min_w_0()
        .min_h_12()
        .px_4()
        .py_2()
        .gap_4()
        .border_b_1()
        .border_color(theme.border)
        .child(settings_row_heading(
            label.clone(),
            description.into(),
            theme,
        ))
        .child(
            crate::components::mint_switch::MintSwitch::new(id)
                .accessibility_label(label)
                .checked(checked)
                .disabled(disabled)
                .on_click(listener),
        )
        .into_any_element()
}

pub(crate) struct SettingsField {
    pub label: &'static str,
    pub input: AnyInputState,
    pub choices: &'static [&'static str],
}

#[cfg(test)]
mod tests;
