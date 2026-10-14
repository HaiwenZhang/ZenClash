use crate::pages::runtime::test_support::{
    RuntimePageFixture as Fixture, open_runtime_page_window as open,
};
use crate::pages::runtime::*;
use gpui_kit::component::WindowExt;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{TestAppContext, VisualTestContext};
use std::time::Duration;

#[gpui_kit::test]
fn network_target_form_cancel_preserves_input_and_restores_focus(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Network);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        cx.set_reduce_motion(true);
        page.update(cx, |page, cx| {
            page.invalidate_page_load();
            page.error = None;
            page.page_observation
                .replace_fixture_data(PageSnapshotData::Network {
                    config: RuntimeConfig::default(),
                    system: zenclash_core::SystemNetworkSnapshot::default(),
                });
            cx.notify();
        });
        window.render_frame(cx);
        assert!(window.try_find("cancel-network-target").is_none());
        window.click("add-network-latency-target", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        let input = page.read(cx).network_diagnostics.latency_name.clone();
        input.update(cx, |input, cx| {
            input.set_value("Retained target", window, cx)
        });
        window.focus(&input.focus_handle(cx), cx);
        window.render_frame(cx);
        assert!(!page.read(cx).mutation_busy(busy::MutationDomain::Network));
        window.click("cancel-network-target", cx);
        assert!(
            !window.has_active_dialog(cx),
            "cancel must close the target modal"
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.background_executor
        .advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(page.read(cx).focus_handle.is_focused(window));
        assert!(window.try_find("cancel-network-target").is_none());
        window.click("add-network-latency-target", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.try_find("cancel-network-target").is_some());
        assert_eq!(
            page.read(cx)
                .network_diagnostics
                .latency_name
                .read(cx)
                .value(),
            "Retained target"
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn cancelling_a_network_probe_aborts_the_owned_task_and_rejects_old_publication(
    cx: &mut TestAppContext,
) {
    cx.foreground_executor().clone().block_test(async {
        let fixture = Fixture::new();
        let (window, page) = open(cx, &fixture, Page::Network);
        fixture.settle(cx, &page, |page| !page.persistent_loading);
        let pending = fixture
            .runtime
            .as_ref()
            .unwrap()
            .spawn(std::future::pending::<()>());
        cx.update_window(window, |_, window, cx| {
            page.update(cx, |page, cx| {
                let old_request = page
                    .network_diagnostics
                    .query
                    .begin_pending_for_test(&pending);
                page.cancel_network_diagnostics();
                let current = fixture
                    .runtime
                    .as_ref()
                    .unwrap()
                    .spawn(std::future::pending::<()>());
                let current_request = page
                    .network_diagnostics
                    .query
                    .begin_pending_for_test(&current);
                let before = fixture.status.snapshot().path;
                page.complete_network_diagnostics(old_request, Err("old probe failed".into()), cx);
                assert_eq!(fixture.status.snapshot().path, before);
                assert!(page.network_diagnostics.query.is_loading());
                assert!(page.network_diagnostics.query.snapshot().is_none());
                page.complete_network_diagnostics(
                    current_request,
                    Err("current task failed".into()),
                    cx,
                );
                assert_eq!(fixture.status.snapshot().path, before);
                assert!(!page.network_diagnostics.query.is_loading());
                assert!(page.network_diagnostics.query.report().is_none());
                assert_eq!(
                    page.network_diagnostics
                        .query
                        .snapshot()
                        .unwrap()
                        .public_ip_error
                        .as_deref(),
                    Some("current task failed")
                );
                page.cancel_network_diagnostics();
            });
            window.remove_window();
        })
        .unwrap();
        assert!(pending.await.unwrap_err().is_cancelled());
    });
}

#[gpui_kit::test]
fn network_probe_continues_when_blurred_and_stops_when_hidden_during_a_mutation(
    cx: &mut TestAppContext,
) {
    cx.foreground_executor().clone().block_test(async {
        let fixture = Fixture::new();
        let _runtime_context = fixture.runtime.as_ref().unwrap().enter();
        let (window, page) = open(cx, &fixture, Page::Network);
        cx.update_window(window, |_, window, _| window.activate_window())
            .unwrap();
        fixture.settle(cx, &page, |page| {
            !page.persistent_loading && page.live_updates_enabled()
        });
        let probe = fixture
            .runtime
            .as_ref()
            .unwrap()
            .spawn(std::future::pending::<()>());
        cx.update_window(window, |_, _, cx| {
            page.update(cx, |page, _| {
                page.network_diagnostics
                    .query
                    .begin_pending_for_test(&probe);
            });
        })
        .unwrap();
        VisualTestContext::from_window(window, cx).deactivate_window();
        assert!(cx.update(|cx| page.read(cx).network_diagnostics.query.is_loading()));
        assert!(!probe.is_finished());
        cx.update_window(window, |_, _, cx| {
            page.update(cx, |page, cx| {
                let mutation = page.begin_mutation(Page::Network).unwrap();
                page.set_window_visible(false, cx);
                assert!(!page.network_diagnostics.query.is_loading());
                assert!(page.core_busy());
                page.finish_mutation(mutation);
            });
        })
        .unwrap();
        assert!(probe.await.unwrap_err().is_cancelled());
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
        drop(page);
        cx.run_until_parked();
    });
}
