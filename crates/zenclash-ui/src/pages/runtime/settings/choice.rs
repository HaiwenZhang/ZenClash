//! Small enum pickers share a search-free, keyboard-accessible popover list.
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, IndexPath,
    button::Button,
    input::InputState,
    list::{List, ListDelegate, ListItem, ListState},
    popover::{Popover, PopoverState},
};
use gpui_kit::{
    App, Context, Entity, Focusable, IntoElement, ParentElement, RenderOnce, SharedString, Styled,
    WeakEntity, Window, div,
};

#[derive(IntoElement)]
pub(super) struct ChoicePopover {
    id: SharedString,
    field: Entity<InputState>,
    choices: &'static [&'static str],
    disabled: bool,
}

impl ChoicePopover {
    pub(super) fn new(
        id: impl Into<SharedString>,
        field: &Entity<InputState>,
        choices: &'static [&'static str],
        disabled: bool,
    ) -> Self {
        Self {
            id: id.into(),
            field: field.clone(),
            choices,
            disabled,
        }
    }
}

struct ChoiceDelegate {
    field: Entity<InputState>,
    choices: &'static [&'static str],
    selected: Option<IndexPath>,
    popover: Option<WeakEntity<PopoverState>>,
}

impl ChoiceDelegate {
    fn dismiss(&self, window: &mut Window, cx: &mut App) {
        if let Some(popover) = &self.popover {
            let _ = popover.update(cx, |state, cx| state.dismiss(window, cx));
        }
    }
}

impl ListDelegate for ChoiceDelegate {
    type Item = ListItem;

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.choices.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let choice = *self.choices.get(ix.row)?;
        Some(
            ListItem::new(SharedString::from(choice))
                .accessibility_label(choice)
                .confirmed(self.field.read(cx).value() == choice)
                .child(choice),
        )
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix;
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        if let Some(choice) = self.selected.and_then(|ix| self.choices.get(ix.row)) {
            self.field
                .update(cx, |field, cx| field.set_value(*choice, window, cx));
            self.dismiss(window, cx);
        }
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        self.dismiss(window, cx);
    }
}

impl RenderOnce for ChoicePopover {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let field = self.field.clone();
        let choices = self.choices;
        let list = window.use_keyed_state(
            (gpui_kit::ElementId::from(self.id.clone()), "choice-list"),
            cx,
            |window, cx| {
                ListState::new(
                    ChoiceDelegate {
                        field,
                        choices,
                        selected: None,
                        popover: None,
                    },
                    window,
                    cx,
                )
                .searchable(false)
            },
        );
        list.update(cx, |list, _| list.delegate_mut().field = self.field.clone());
        let selected = self.field.read(cx).value();
        let focus = list.read(cx).focus_handle(cx);
        let opened_list = list.clone();
        let mut popover = Popover::new((gpui_kit::ElementId::from(self.id.clone()), "popover"))
            .track_focus(&focus)
            .trigger_style(gpui_kit::StyleRefinement::default().w_full())
            .trigger(
                Button::new(self.id)
                    .outline()
                    .w_full()
                    .disabled(self.disabled)
                    .child(
                        gpui_kit::component::h_flex()
                            .w_full()
                            .justify_between()
                            .gap_2()
                            .child(if selected.is_empty() {
                                SharedString::from("—")
                            } else {
                                selected
                            })
                            .child(Icon::new(IconName::ChevronDown).size_4()),
                    ),
            )
            .on_open_change(move |open, window, cx| {
                if *open {
                    opened_list.update(cx, |list, cx| {
                        let delegate = list.delegate();
                        let value = delegate.field.read(cx).value();
                        let selected = delegate
                            .choices
                            .iter()
                            .position(|choice| value == *choice)
                            .map(IndexPath::new);
                        list.set_selected_index(selected, window, cx);
                        list.focus(window, cx);
                    });
                }
            })
            .content(move |_, _, cx| {
                let popover = cx.entity().downgrade();
                list.update(cx, |list, _| list.delegate_mut().popover = Some(popover));
                div()
                    .w_48()
                    .text_color(cx.theme().popover_foreground)
                    .child(
                        List::new(&list)
                            .max_h(gpui_kit::rems(14.))
                            .scrollbar_visible(false),
                    )
            });
        if self.disabled {
            popover = popover.open(false);
        }
        popover
    }
}
