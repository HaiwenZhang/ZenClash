use crate::pages::runtime::test_support::{
    RuntimePageFixture as Fixture, open_runtime_page_window as open,
};
use crate::pages::runtime::*;
use gpui_kit::TestAppContext;
use std::fs;

#[gpui_kit::test]
fn repeated_config_invalidations_finish_with_the_latest_form_values(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| {
        !page.persistent_loading && !page.config_inputs.is_loading()
    });
    cx.update(|cx| {
        page.update(cx, |page, cx| {
            page.invalidate_config_inputs(cx);
            assert!(page.config_inputs.is_loading());
            fs::write(
                &fixture.profile,
                "mixed-port: 7899\nrules: [MATCH,DIRECT]\n",
            )
            .unwrap();
            page.invalidate_config_inputs(cx);
            page.invalidate_config_inputs(cx);
            assert!(page.config_inputs.is_loading());
            assert!(!page.persistent_loading);
        })
    });
    fixture.settle(cx, &page, |page| !page.config_inputs.is_loading());
    cx.update(|cx| {
        assert_eq!(
            page.read(cx).config_inputs.core.mixed_port.read(cx).value(),
            "7899"
        );
    });
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn missing_network_modes_display_core_defaults_and_keep_explicit_values(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Dns);
    fixture.settle(cx, &page, |page| {
        !page.persistent_loading && !page.config_inputs.is_loading()
    });
    cx.update_window(window, |_, window, cx| {
        let mut inputs = ConfigInputs::new(&serde_json::json!({}), None, window, cx);
        assert_eq!(inputs.tun.stack.read(cx).value(), "gvisor");
        assert_eq!(inputs.dns.enhanced_mode.read(cx).value(), "redir-host");
        inputs.refresh(
            &serde_json::json!({"tun":{"stack":"mixed"},"dns":{"enhanced-mode":"fake-ip"}}),
            None,
            window,
            cx,
        );
        assert_eq!(inputs.tun.stack.read(cx).value(), "mixed");
        assert_eq!(inputs.dns.enhanced_mode.read(cx).value(), "fake-ip");
        window.remove_window();
    })
    .unwrap();
}
