use std::rc::Rc;

use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    App, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString,
    Styled, Window, div, rems,
};

type ChangeHandler = Rc<dyn Fn(&bool, &mut Window, &mut App)>;

/// Product presentation over the framework's controlled switch behavior.
#[derive(IntoElement)]
pub(crate) struct MintSwitch {
    id: ElementId,
    label: SharedString,
    checked: bool,
    disabled: bool,
    on_change: Option<ChangeHandler>,
}

impl MintSwitch {
    pub(crate) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            label: "".into(),
            checked: false,
            disabled: false,
            on_change: None,
        }
    }

    pub(crate) fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = label.into();
        self
    }

    pub(crate) fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }
    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for MintSwitch {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let checked = self.checked;
        let handler = self.on_change;
        gpui_kit::base::Switch::new(self.id)
            .accessibility_label(self.label)
            .checked(checked)
            .disabled(self.disabled)
            .relative()
            .w(rems(2.75))
            .h(rems(1.5))
            .flex_shrink_0()
            .rounded_full()
            .bg(if checked { theme.chart_3 } else { theme.border })
            .styles(|styles| styles.disabled(|style| style.opacity(0.5)))
            .border_1()
            .border_color(theme.transparent)
            .focus_visible(|style| style.border_color(theme.ring))
            .child(
                div()
                    .absolute()
                    .top(rems(0.125))
                    .left(if checked { rems(1.375) } else { rems(0.125) })
                    .size(rems(1.25))
                    .rounded_full()
                    .bg(theme.secondary),
            )
            .on_change(move |next, _, window, cx| {
                if let Some(handler) = &handler {
                    handler(&next, window, cx);
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AppContext, Context, Render, TestAppContext, px, size};

    struct Owner {
        checked: bool,
        disabled: bool,
        requests: usize,
    }

    impl Render for Owner {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            MintSwitch::new("mint-switch")
                .accessibility_label("Enabled")
                .checked(self.checked)
                .disabled(self.disabled)
                .on_click(cx.listener(|this, next, _, cx| {
                    this.checked = *next;
                    this.requests += 1;
                    cx.notify();
                }))
        }
    }

    #[gpui_kit::test]
    fn pointer_and_keyboard_update_owner_while_disabled_rejects_activation(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let mut owner = None;
        let window = cx.open_window(size(px(300.), px(120.)), |window, cx| {
            let view = cx.new(|_| Owner {
                checked: false,
                disabled: false,
                requests: 0,
            });
            owner = Some(view.clone());
            Root::new(view, window, cx)
        });
        let owner = owner.unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("mint-switch", cx);
            assert!(owner.read(cx).checked);
            window.press("space", cx);
            assert!(!owner.read(cx).checked);
            assert_eq!(owner.read(cx).requests, 2);
            owner.update(cx, |owner, cx| {
                owner.disabled = true;
                cx.notify();
            });
            window.render_frame(cx);
            window.click("mint-switch", cx);
            window.press("enter", cx);
            assert_eq!(owner.read(cx).requests, 2);
        })
        .unwrap();
    }
}
