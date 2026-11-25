//! Retained GPUI input fields, edit baselines, and field construction factory.

use std::collections::HashMap;

use gpui_kit::base::input::{InputBaseState, InputModeKind};
use gpui_kit::component::input::{AnyInputState, InputState, TextareaState};
use gpui_kit::{AppContext, Context, Entity, Focusable, SharedString, Window};

#[derive(Default)]
pub(super) struct FieldBaseline(pub(super) String);

impl FieldBaseline {
    pub(super) fn accept(&mut self, current: &str, submitted: &str) {
        if current == submitted {
            self.0 = submitted.to_owned();
        }
    }

    pub(super) fn refresh(&mut self, current: &str, incoming: String) -> Option<String> {
        let replace = current != incoming && current == self.0;
        self.0 = incoming;
        replace.then(|| self.0.clone())
    }
}

pub(super) struct InputField {
    pub(super) input: AnyInputState,
    pub(super) baseline: FieldBaseline,
    pub(super) placeholder: SharedString,
}

pub(crate) struct InputFactory<'a> {
    pub(super) window: &'a mut Window,
    pub(super) cx: &'a mut gpui_kit::App,
    pub(super) fields: &'a mut HashMap<&'static str, InputField>,
}

impl InputFactory<'_> {
    pub(crate) fn single(
        &mut self,
        key: &'static str,
        value: String,
        placeholder: impl Into<SharedString>,
    ) -> Entity<InputState> {
        self.field(
            key,
            value,
            placeholder.into(),
            |state| state.as_input().cloned(),
            InputState::new,
        )
    }

    pub(super) fn multi(
        &mut self,
        key: &'static str,
        value: String,
        placeholder: impl Into<SharedString>,
    ) -> Entity<TextareaState> {
        self.field(
            key,
            value,
            placeholder.into(),
            |state| state.as_textarea().cloned(),
            |window, cx| TextareaState::new(window, cx).auto_grow(2, 7),
        )
    }

    fn field<M: InputModeKind>(
        &mut self,
        key: &'static str,
        value: String,
        placeholder: SharedString,
        cached: impl FnOnce(&AnyInputState) -> Option<Entity<InputBaseState<M>>>,
        create: impl FnOnce(&mut Window, &mut Context<InputBaseState<M>>) -> InputBaseState<M>,
    ) -> Entity<InputBaseState<M>>
    where
        AnyInputState: From<Entity<InputBaseState<M>>>,
    {
        if let Some(field) = self.fields.get_mut(key)
            && let Some(input) = cached(&field.input)
        {
            let current = input.read(self.cx).value();
            if let Some(value) = field.baseline.refresh(&current, value) {
                input.update(self.cx, |input, cx| {
                    let focused = input.focus_handle(cx).is_focused(self.window);
                    let cursor = input.cursor_position();
                    input.set_value(value, self.window, cx);
                    if focused {
                        input.set_cursor_position(cursor, self.window, cx);
                    }
                });
            }
            if field.placeholder != placeholder {
                input.update(self.cx, |input, cx| {
                    input.set_placeholder(placeholder.clone(), self.window, cx)
                });
                field.placeholder = placeholder;
            }
            return input;
        }
        let input = self.cx.new(|cx| {
            create(self.window, cx)
                .placeholder(placeholder.clone())
                .default_value(value.clone())
        });
        self.fields.insert(
            key,
            InputField {
                input: input.clone().into(),
                baseline: FieldBaseline(value),
                placeholder,
            },
        );
        input
    }
}
