//! Create examples/pages/runtime, copy to examples/config_form_state.rs, then run the example.
//! Uses the production input model in a real GPUI window, with generated configuration only.
mod pages {
    pub mod runtime {
        use gpui::{
            AppContext, Application, Context, Focusable, IntoElement, ParentElement, Render,
            Window, WindowOptions,
        };
        use gpui_component::{Root, input::Input, v_flex};
        use serde_json::json;
        use std::path::Path;

        #[path = "../../../src/pages/runtime/config_inputs.rs"]
        mod config_inputs;

        struct RuntimePage {
            inputs: config_inputs::ConfigInputs,
        }

        impl Render for RuntimePage {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                v_flex()
                    .child(Input::new(&self.inputs.dns.nameserver))
                    .child(Input::new(&self.inputs.core.mixed_port))
            }
        }

        pub fn run() {
            Application::new().with_assets(zenclash_ui::assets::Assets).run(|cx| {
        gpui_component::init(cx);
        cx.spawn(async move |cx| {
            let mut page = None;
            let handle = cx.open_window(WindowOptions::default(), |window, cx| {
                window.activate_window();
                let view = cx.new(|cx| RuntimePage {
                    inputs: config_inputs::ConfigInputs::new(&json!({
                        "dns": { "nameserver": ["original"] }, "mixed-port": 7890
                    }), Some(Path::new("fixture-a.yaml")), window, cx),
                });
                page = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            }).expect("test window");
            let page = page.expect("test page");
            cx.background_executor().timer(std::time::Duration::from_millis(200)).await;
            handle.update(cx, |_, window, cx| {
                page.update(cx, |page, cx| {
                    let dns = page.inputs.dns.nameserver.clone();
                    let port = page.inputs.core.mixed_port.clone();
                    let original_id = dns.entity_id();
                    dns.update(cx, |input, cx| {
                        input.set_value("editing after submit", window, cx);
                        input.set_cursor_position(gpui_component::input::Position::new(0, 4), window, cx);
                    });
                    let cursor = dns.read(cx).cursor_position();
                    let incoming = json!({"dns": {"nameserver": ["submitted earlier"]}, "mixed-port": 7891});
                    page.inputs.refresh(&incoming, Some(Path::new("fixture-a.yaml")), window, cx);
                    assert_eq!(page.inputs.dns.nameserver.entity_id(), original_id);
                    assert_eq!(page.inputs.core.mixed_port.entity_id(), port.entity_id());
                    assert_eq!(dns.read(cx).value().as_str(), "editing after submit");
                    assert_eq!(port.read(cx).value().as_str(), "7891");
                    assert_eq!(dns.read(cx).cursor_position(), cursor);
                    assert!(dns.read(cx).focus_handle(cx).is_focused(window));
                    let acknowledged = json!({"dns": {"nameserver": ["editing after submit"]}});
                    page.inputs.refresh(&acknowledged, Some(Path::new("fixture-a.yaml")), window, cx);
                    page.inputs.refresh(&incoming, Some(Path::new("fixture-a.yaml")), window, cx);
                    assert_eq!(dns.read(cx).value().as_str(), "submitted earlier");
                    assert_eq!(dns.read(cx).cursor_position(), cursor);
                    assert!(dns.read(cx).focus_handle(cx).is_focused(window));
                    dns.update(cx, |input, cx| input.set_value("  canonical  ", window, cx));
                    let patch = json!({"dns": {"nameserver": ["canonical"]}});
                    let submitted = page.inputs.submitted(&patch, cx);
                    page.inputs.accept_submitted(submitted, cx);
                    page.inputs.refresh(&patch, Some(Path::new("fixture-a.yaml")), window, cx);
                    assert_eq!(dns.read(cx).value().as_str(), "canonical");
                    dns.update(cx, |input, cx| input.set_value("submitted", window, cx));
                    let submitted = page.inputs.submitted(&patch, cx);
                    dns.update(cx, |input, cx| input.set_value("typed later", window, cx));
                    page.inputs.accept_submitted(submitted, cx);
                    page.inputs.refresh(&patch, Some(Path::new("fixture-a.yaml")), window, cx);
                    assert_eq!(dns.read(cx).value().as_str(), "typed later");
                    dns.update(cx, |input, cx| input.set_value("unsaved", window, cx));
                    page.inputs.refresh(&json!({"dns": {"nameserver": ["B"]}}), Some(Path::new("fixture-b.yaml")), window, cx);
                    let dns = page.inputs.dns.nameserver.clone();
                    assert_eq!(dns.read(cx).value().as_str(), "B");
                    assert_ne!(dns.entity_id(), original_id);
                    assert!(dns.read(cx).focus_handle(cx).is_focused(window));
                    dns.update(cx, |input, cx| input.set_value("unsaved again", window, cx));
                    page.inputs.reset_on_next_refresh();
                    page.inputs.refresh(&json!({"dns": {"nameserver": ["restored"]}}), Some(Path::new("fixture-b.yaml")), window, cx);
                    assert_ne!(page.inputs.dns.nameserver.entity_id(), dns.entity_id());
                    assert_eq!(page.inputs.dns.nameserver.read(cx).value().as_str(), "restored");
                    eprintln!("config_form_state_passed: identity, dirty edits, save readback, focus, cursor, profile switch, backup reset");
                });
            }).expect("form assertions");
            cx.update(|cx| cx.quit()).expect("quit");
        }).detach();
    });
        }
    }
}
fn main() {
    pages::runtime::run();
}
