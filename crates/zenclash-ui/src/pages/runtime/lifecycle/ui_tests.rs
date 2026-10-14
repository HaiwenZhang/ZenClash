use crate::pages::runtime::test_support::{
    RuntimePageFixture as Fixture, open_runtime_page_window as open,
};
use crate::pages::runtime::*;
use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt;

#[gpui_kit::test]
fn returning_to_a_tab_renders_its_snapshot_while_controller_is_slow(cx: &mut TestAppContext) {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let fixture = Fixture::with_controller(format!("http://{}", listener.local_addr().unwrap()));
    let _runtime_context = fixture.runtime.as_ref().unwrap().enter();
    let (window, page) = open(cx, &fixture, Page::Dns);
    cx.update_window(window, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    fixture.settle(cx, &page, |page| {
        !page.persistent_loading && !page.config_inputs.is_loading()
    });
    let observation = fixture
        .runtime
        .as_ref()
        .unwrap()
        .block_on(fixture.core.begin_observation())
        .unwrap();
    let identity = observation.identity().clone();
    drop(observation);
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            let token = page.page_task_token_for(Page::Dns);
            page.replace_page_data(
                token,
                PageSnapshotData::Config(RuntimeConfig::default()),
                cx,
            );
            page.page_observation
                .set_fixture_source(Some(identity.clone()));
            page.switch_to(Page::Sniffer, cx);
            assert!(matches!(
                page.page_observation.data(),
                PageSnapshotData::Config(_)
            ));
            page.switch_to(Page::Dns, cx);
            assert!(page.loading);
            assert!(matches!(
                page.page_observation.data(),
                PageSnapshotData::Config(_)
            ));
        });
        window.render_frame(cx);
        window.find("save-dns-advanced");
        page.update(cx, |page, cx| {
            page.set_presented(false, cx);
            page.set_presented(true, cx);
            assert!(matches!(
                page.page_observation.data(),
                PageSnapshotData::Config(_)
            ));
        });
        window.render_frame(cx);
        window.find("save-dns-advanced");
        page.update(cx, |page, cx| page.set_presented(false, cx));
        window.remove_window();
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn switching_pages_restores_focus_after_a_subscription_input_disappears(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-add-subscription", cx);
        let input = page
            .read(cx)
            .profiles
            .forms
            .subscription_name
            .focus_handle(cx);
        window.focus(&input, cx);
        window.render_frame(cx);
        assert!(input.is_focused(window));
        page.update(cx, |page, cx| page.switch_to(Page::Settings, cx));
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(page.read(cx).focus_handle.is_focused(window));
        window.press("tab", cx);
        assert!(window.focused(cx).is_some());
    })
    .unwrap();
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}
