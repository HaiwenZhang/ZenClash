use crate::pages::runtime::test_support::{
    RuntimePageFixture as Fixture, open_runtime_page_window as open,
};
use crate::pages::runtime::*;
use gpui_kit::TestAppContext;

#[gpui_kit::test]
fn system_proxy_save_does_not_validate_or_apply_other_tab_drafts(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::SystemProxy);
    fixture.settle(cx, &page, |page| {
        !page.persistent_loading && !page.config_inputs.is_loading()
    });
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.config_inputs
                .core
                .mixed_port
                .update(cx, |input, cx| input.set_value("7899", window, cx));
            page.config_inputs.core.log_level.update(cx, |input, cx| {
                input.set_value("unfinished-level", window, cx)
            });
            page.config_inputs.core.redir_port.update(cx, |input, cx| {
                input.set_value("unfinished-port", window, cx)
            });
            let patch = page.config_inputs.core.listener_patch(cx).unwrap();
            assert_eq!(patch["mixed-port"], 7899);
            assert!(patch.get("log-level").is_none());
            assert!(patch.get("redir-port").is_none());
            page.config_inputs.reset_page(Page::SystemProxy, window, cx);
            assert_eq!(page.config_inputs.core.mixed_port.read(cx).value(), "7890");
            assert_eq!(
                page.config_inputs.core.log_level.read(cx).value(),
                "unfinished-level"
            );
        });
        window.remove_window();
    })
    .unwrap();
}
