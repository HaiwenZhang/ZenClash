//! Focused editors stage a copy until the user accepts it into the page draft.
use super::choice::ChoicePopover;
use crate::pages::runtime::{Button, Disableable, RuntimePage, Sizable, div, h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme, WindowExt,
    dialog::DialogButtonProps,
    input::{AnyInputState, Input, InputState, Textarea, TextareaState},
};
use gpui_kit::{AppContext, Context, Entity, Focusable, IntoElement, ParentElement, Styled};

pub(in crate::pages::runtime) fn settings_dialog_footer(label: String) -> gpui_kit::Div {
    use gpui_kit::component::button::ButtonVariants;
    use gpui_kit::component::dialog::DialogAction;
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

pub(in crate::pages::runtime) fn settings_status<F>(
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

pub(in crate::pages::runtime) fn settings_input_row(
    label: impl Into<gpui_kit::SharedString>,
    _description: impl Into<gpui_kit::SharedString>,
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
        .child(div().flex_1().min_w_0().text_sm().child(label.into()))
        .child(div().flex_1().min_w_0().child(input))
        .into_any_element()
}

pub(in crate::pages::runtime) fn settings_switch<F>(
    label: impl Into<gpui_kit::SharedString>,
    _description: impl Into<gpui_kit::SharedString>,
    checked: bool,
    id: &'static str,
    theme: &gpui_kit::component::Theme,
    listener: F,
) -> gpui_kit::AnyElement
where
    F: Fn(&bool, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
{
    settings_switch_disabled(label, _description, checked, id, theme, false, listener)
}

pub(in crate::pages::runtime) fn settings_switch_disabled<F>(
    label: impl Into<gpui_kit::SharedString>,
    _description: impl Into<gpui_kit::SharedString>,
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
        .child(div().flex_1().min_w_0().text_sm().child(label.clone()))
        .child(
            crate::components::mint_switch::MintSwitch::new(id)
                .accessibility_label(label)
                .checked(checked)
                .disabled(disabled)
                .on_click(listener),
        )
        .into_any_element()
}

pub(in crate::pages::runtime) struct SettingsField {
    pub label: &'static str,
    pub input: AnyInputState,
    pub choices: &'static [&'static str],
}

impl RuntimePage {
    pub(in crate::pages::runtime) fn settings_choice(
        &self,
        id: &'static str,
        field: &Entity<gpui_kit::component::input::InputState>,
        choices: &'static [&'static str],
        _cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        ChoicePopover::new(id, field, choices, self.core_busy()).into_any_element()
    }

    pub(in crate::pages::runtime) fn settings_cancel(&self, cx: &mut Context<Self>) -> Button {
        Button::new("settings-cancel-draft")
            .label(zenclash_i18n::text("common.actions.cancel"))
            .outline()
            .disabled(self.core_busy())
            .on_click(cx.listener(|this, _, window, cx| {
                this.config_inputs.reset_page(this.page, window, cx);
                cx.notify();
            }))
    }

    pub(in crate::pages::runtime) fn settings_group_row(
        &self,
        key: &'static str,
        preview: String,
        fields: Vec<SettingsField>,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let fields = std::rc::Rc::new(fields);
        let title = zenclash_i18n::text(key);
        h_flex()
            .min_w_0()
            .gap_3()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().flex_1().text_sm().child(title.clone()))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(preview),
            )
            .child(
                Button::new(gpui_kit::SharedString::from(format!("settings-edit-{key}")))
                    .label(zenclash_i18n::text("settings_redesign.edit"))
                    .outline()
                    .small()
                    .disabled(self.core_busy())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if window.has_active_dialog(cx) {
                            return;
                        }
                        let drafts = fields
                            .iter()
                            .map(|field| {
                                let value = field.input.value(cx);
                                let input: AnyInputState = if field.input.as_input().is_some() {
                                    cx.new(|cx| InputState::new(window, cx).default_value(value))
                                        .into()
                                } else {
                                    cx.new(|cx| {
                                        TextareaState::new(window, cx)
                                            .auto_grow(2, 5)
                                            .default_value(value)
                                    })
                                    .into()
                                };
                                (field.label, field.input.clone(), input, field.choices)
                            })
                            .collect::<Vec<_>>();
                        let owner = cx.entity().downgrade();
                        let profile = this.profile_path.clone();
                        let page = this.page;
                        let generation = this.config_inputs_generation;
                        let navigation = this.navigation_generation;
                        let title = title.clone();
                        window.open_dialog(cx, move |dialog, window, cx| {
                            let mut content = v_flex().gap_3();
                            for (key, _, draft, choices) in &drafts {
                                let control = if let Some(field) = draft.as_input() {
                                    if choices.is_empty() {
                                        Input::new(field).into_any_element()
                                    } else {
                                        ChoicePopover::new(
                                            format!("settings-modal-{key}"),
                                            field,
                                            choices,
                                            false,
                                        )
                                        .into_any_element()
                                    }
                                } else {
                                    Textarea::new(draft.as_textarea().expect("editor field kind"))
                                        .into_any_element()
                                };
                                content = content.child(
                                    v_flex()
                                        .gap_2()
                                        .child(div().text_sm().child(zenclash_i18n::text(key)))
                                        .child(control),
                                );
                            }
                            let drafts = drafts.clone();
                            let owner = owner.clone();
                            let profile = profile.clone();
                            dialog
                                .title(title.clone())
                                .bg(cx.theme().group_box)
                                .width(window.rem_size() * 36.)
                                .margin_top(
                                    ((window.viewport_size().height - window.rem_size() * 32.)
                                        / 2.)
                                        .max(window.rem_size()),
                                )
                                .child(content)
                                .button_props(
                                    DialogButtonProps::default().show_cancel(true).ok_text(
                                        zenclash_i18n::text("settings_redesign.accept_draft"),
                                    ),
                                )
                                .footer(settings_dialog_footer(zenclash_i18n::text(
                                    "settings_redesign.accept_draft",
                                )))
                                .on_ok(move |_, window, cx| {
                                    owner
                                        .update(cx, |this, cx| {
                                            if this.page != page
                                                || this.profile_path != profile
                                                || this.config_inputs_generation != generation
                                                || this.navigation_generation != navigation
                                                || this.core_busy()
                                            {
                                                return false;
                                            }
                                            for (_, original, draft, _) in &drafts {
                                                let value = draft.value(cx);
                                                if let Some(field) = original.as_input() {
                                                    field.update(cx, |input, cx| {
                                                        input.set_value(value, window, cx)
                                                    });
                                                } else if let Some(field) = original.as_textarea() {
                                                    field.update(cx, |input, cx| {
                                                        input.set_value(value, window, cx)
                                                    });
                                                }
                                            }
                                            cx.notify();
                                            true
                                        })
                                        .unwrap_or(true)
                                })
                        });
                    })),
            )
            .into_any_element()
    }

    pub(in crate::pages::runtime) fn settings_list_row(
        &self,
        key: &'static str,
        input: &Entity<TextareaState>,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let value = input.read(cx).value().to_string();
        let preview = value
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("—")
            .to_owned();
        let field = input.clone();
        let title = zenclash_i18n::text(key);
        h_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().flex_1().min_w_0().text_sm().child(title.clone()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(preview),
            )
            .child(
                Button::new(gpui_kit::SharedString::from(format!("settings-edit-{key}")))
                    .label(zenclash_i18n::text("settings_redesign.edit"))
                    .small()
                    .outline()
                    .disabled(self.core_busy())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if window.has_active_dialog(cx) {
                            return;
                        }
                        let draft = cx.new(|cx| {
                            TextareaState::new(window, cx)
                                .auto_grow(8, 14)
                                .default_value(field.read(cx).value())
                        });
                        let owner = cx.entity().downgrade();
                        let field = field.clone();
                        let profile = this.profile_path.clone();
                        let page = this.page;
                        let generation = this.config_inputs_generation;
                        let navigation = this.navigation_generation;
                        let title = title.clone();
                        let focus = draft.read(cx).focus_handle(cx);
                        window.open_dialog(cx, move |dialog, window, cx| {
                            let draft_save = draft.clone();
                            let field = field.clone();
                            let owner = owner.clone();
                            let profile = profile.clone();
                            dialog
                                .title(title.clone())
                                .bg(cx.theme().group_box)
                                .width(window.rem_size() * 36.)
                                .margin_top(
                                    ((window.viewport_size().height - window.rem_size() * 28.)
                                        / 2.)
                                        .max(window.rem_size()),
                                )
                                .child(
                                    v_flex()
                                        .gap_3()
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(zenclash_i18n::text(
                                                    "settings_redesign.list_hint",
                                                )),
                                        )
                                        .child(Textarea::new(&draft)),
                                )
                                .button_props(
                                    DialogButtonProps::default().show_cancel(true).ok_text(
                                        zenclash_i18n::text("settings_redesign.accept_draft"),
                                    ),
                                )
                                .footer(settings_dialog_footer(zenclash_i18n::text(
                                    "settings_redesign.accept_draft",
                                )))
                                .on_ok(move |_, window, cx| {
                                    owner
                                        .update(cx, |this, cx| {
                                            if this.page != page
                                                || this.profile_path != profile
                                                || this.config_inputs_generation != generation
                                                || this.navigation_generation != navigation
                                                || this.core_busy()
                                            {
                                                return false;
                                            }
                                            let value = draft_save.read(cx).value();
                                            field.update(cx, |field, cx| {
                                                field.set_value(value, window, cx)
                                            });
                                            cx.notify();
                                            true
                                        })
                                        .unwrap_or(true)
                                })
                        });
                        focus.focus(window, cx);
                    })),
            )
            .into_any_element()
    }
}
