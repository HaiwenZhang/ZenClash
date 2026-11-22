use std::path::Path;

use gpui_kit::component::{
    Root,
    input::{Input, Textarea},
    v_flex,
};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AppContext, Context, IntoElement, ParentElement, Render, TestAppContext, Window, px, size,
};
use serde_json::json;

use super::ConfigInputs;

struct Form {
    inputs: ConfigInputs,
}

impl Render for Form {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .child(Textarea::new(&self.inputs.dns.nameserver))
            .child(Input::new(&self.inputs.core.mixed_port).id("mixed-port"))
    }
}

#[gpui_kit::test]
fn background_refresh_preserves_multiline_edits_focus_and_cursor(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let mut form = None;
    let handle = cx.open_window(size(px(800.), px(600.)), |window, cx| {
        let view = cx.new(|cx| Form {
            inputs: ConfigInputs::new(
                &json!({"dns": {"nameserver": ["original"]}, "mixed-port": 7890}),
                Some(Path::new("a.yaml")),
                window,
                cx,
            ),
        });
        form = Some(view.clone());
        Root::new(view, window, cx)
    });
    let form = form.unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(
            ("input", form.read(cx).inputs.dns.nameserver.entity_id()),
            cx,
        );
        window.press("secondary-a", cx);
        window.input("first", cx);
        window.press("enter", cx);
        window.input("第二行", cx);
        let input = form.read(cx).inputs.dns.nameserver.clone();
        let cursor = input.read(cx).cursor_position();
        form.update(cx, |form, cx| {
            form.inputs.refresh(
                &json!({"dns": {"nameserver": ["remote"]}, "mixed-port": 7891}),
                Some(Path::new("a.yaml")),
                window,
                cx,
            );
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(
            window
                .find(("input", form.read(cx).inputs.dns.nameserver.entity_id()))
                .value(),
            Some("first\n第二行")
        );
        assert_eq!(
            window
                .find(("input", form.read(cx).inputs.dns.nameserver.entity_id()))
                .focused(),
            Some(true)
        );
        assert_eq!(
            form.read(cx).inputs.dns.nameserver.entity_id(),
            input.entity_id()
        );
        assert_eq!(input.read(cx).cursor_position(), cursor);
        assert_eq!(
            form.read(cx).inputs.core.mixed_port.read(cx).value(),
            "7891"
        );
        assert_eq!(
            form.read(cx).inputs.dns.patch(cx).unwrap()["dns"]["nameserver"],
            json!(["first", "第二行"])
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn switching_profiles_and_resetting_replace_the_buffer_and_restore_focus(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let mut form = None;
    let handle = cx.open_window(size(px(800.), px(600.)), |window, cx| {
        let view = cx.new(|cx| Form {
            inputs: ConfigInputs::new(
                &json!({"dns": {"nameserver": ["A"]}}),
                Some(Path::new("a.yaml")),
                window,
                cx,
            ),
        });
        form = Some(view.clone());
        Root::new(view, window, cx)
    });
    let form = form.unwrap();
    let old_id = cx
        .update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click(
                ("input", form.read(cx).inputs.dns.nameserver.entity_id()),
                cx,
            );
            window.input("unsaved", cx);
            form.read(cx).inputs.dns.nameserver.entity_id()
        })
        .unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        form.update(cx, |form, cx| {
            form.inputs.refresh(
                &json!({"dns": {"nameserver": ["B"]}}),
                Some(Path::new("b.yaml")),
                window,
                cx,
            );
            cx.notify();
        });
    })
    .unwrap();
    cx.run_until_parked();
    let switched_id = cx
        .update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let input = form.read(cx).inputs.dns.nameserver.clone();
            let id = ("input", input.entity_id());
            assert_eq!(window.find(id).value(), Some("B"));
            assert_eq!(window.find(id).focused(), Some(true));
            assert_ne!(input.entity_id(), old_id);
            window.press("end", cx);
            window.input("unsaved again", cx);
            assert_eq!(input.read(cx).value(), "Bunsaved again");
            input.entity_id()
        })
        .unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        form.update(cx, |form, cx| {
            form.inputs.reset_on_next_refresh();
            form.inputs.refresh(
                &json!({"dns": {"nameserver": ["restored"]}}),
                Some(Path::new("b.yaml")),
                window,
                cx,
            );
            cx.notify();
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let input = form.read(cx).inputs.dns.nameserver.clone();
        let id = ("input", input.entity_id());
        assert_eq!(window.find(id).value(), Some("restored"));
        assert_eq!(window.find(id).focused(), Some(true));
        assert_ne!(input.entity_id(), switched_id);
        window.press("end", cx);
        window.input("typed after reset", cx);
        assert_eq!(input.read(cx).value(), "restoredtyped after reset");
    })
    .unwrap();
}
