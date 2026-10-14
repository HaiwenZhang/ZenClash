use crate::pages::proxies::{
    ProxyGroupBehavior, ProxyNodeId,
    test_support::{
        ProxyConfigurationFixture, open_ready_session_page_window, wait_for_page_state,
    },
};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, TestAppContext};
use std::time::Duration;

#[gpui_kit::test]
fn automatic_restore_button_adopts_original_observed_controller_facts(cx: &mut TestAppContext) {
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let fixture = ProxyConfigurationFixture::automatic_group();
        let (window, page) = open_ready_session_page_window(cx, &fixture).await;
        cx.update_window(window, |_, window, cx| {
            // Keep dialog geometry fixed throughout the simulated pointer events.
            cx.set_reduce_motion(true);
            window.render_frame(cx);
            window.click(
                crate::pages::proxies::view::proxy_element_id(
                    "proxy-node-details",
                    "group",
                    &ProxyNodeId::new("Requested".into(), None),
                ),
                cx,
            );
            window.render_frame(cx);
            window.click(
                (
                    gpui_kit::ElementId::from("restore-auto"),
                    "group".to_owned(),
                ),
                cx,
            );
            assert_eq!(page.read(cx).restoring_auto.as_deref(), Some("group"));
        })
        .unwrap();
        wait_for_page_state(cx, &fixture, &page, |page| page.restoring_auto.is_none()).await;
        cx.update_window(window, |_, window, cx| {
            let state = page.read(cx);
            assert_eq!(
                state.catalog.as_ref().unwrap().groups()[0].behavior,
                ProxyGroupBehavior::Automatic { fixed: false }
            );
            assert_eq!(state.catalog.as_ref().unwrap().groups()[0].now, "Actual");
            assert!(
                state
                    .catalog_operations
                    .as_ref()
                    .unwrap()
                    .observation_identity()
                    .unwrap()
                    .belongs_to(&fixture.session)
            );
            assert!(state.notice.is_some());
            assert!(state.error.is_none());
            window.remove_window();
        })
        .unwrap();
    });
}

#[gpui_kit::test]
fn automatic_group_measurement_button_adopts_delays_and_actual_automatic_selection(
    cx: &mut TestAppContext,
) {
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let fixture = ProxyConfigurationFixture::automatic_group();
        let (window, page) = open_ready_session_page_window(cx, &fixture).await;
        cx.update_window(window, |_, window, cx| {
            // Keep dialog geometry fixed throughout the simulated pointer events.
            cx.set_reduce_motion(true);
            window.render_frame(cx);
            window.click(
                crate::pages::proxies::view::proxy_element_id(
                    "proxy-node-details",
                    "group",
                    &ProxyNodeId::new("Requested".into(), None),
                ),
                cx,
            );
            window.render_frame(cx);
            window.click(
                (
                    gpui_kit::ElementId::from("measure-restore-auto"),
                    "group".to_owned(),
                ),
                cx,
            );
            assert_eq!(
                page.read(cx).measuring_and_restoring_auto.as_deref(),
                Some("group")
            );
        })
        .unwrap();
        wait_for_page_state(cx, &fixture, &page, |page| {
            page.measuring_and_restoring_auto.is_none()
        })
        .await;
        cx.update_window(window, |_, window, cx| {
            let state = page.read(cx);
            assert_eq!(
                state.catalog.as_ref().unwrap().groups()[0].behavior,
                ProxyGroupBehavior::Automatic { fixed: false }
            );
            assert_eq!(state.catalog.as_ref().unwrap().groups()[0].now, "Actual");
            assert_eq!(
                state
                    .catalog
                    .as_ref()
                    .unwrap()
                    .node(&ProxyNodeId::new("node".into(), None))
                    .unwrap()
                    .latest_delay(),
                Some(42)
            );
            assert_eq!(
                state
                    .catalog
                    .as_ref()
                    .unwrap()
                    .node(&ProxyNodeId::new("Actual".into(), None))
                    .unwrap()
                    .latest_delay(),
                Some(24)
            );
            assert!(state.notice.is_some());
            assert!(state.error.is_none());
            window.remove_window();
        })
        .unwrap();
    });
}

#[gpui_kit::test]
fn hiding_a_page_during_admitted_group_measurement_preserves_business_completion(
    cx: &mut TestAppContext,
) {
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let mut fixture = ProxyConfigurationFixture::with_group_delay_paused();
        let (window, page) = open_ready_session_page_window(cx, &fixture).await;
        page.update(cx, |page, cx| {
            page.measure_group_and_restore_auto("group".into(), None, cx)
        });
        let started = fixture.paused_request_started.take().unwrap();
        fixture
            .runtime
            .spawn(async move {
                tokio::time::timeout(Duration::from_secs(2), started)
                    .await
                    .unwrap()
                    .unwrap();
            })
            .await
            .unwrap();
        page.update(cx, |page, _| page.suspend());
        fixture.release_response.take().unwrap().send(()).unwrap();
        let workspace = fixture.runtime.block_on(async {
            let observation =
                tokio::time::timeout(Duration::from_secs(2), fixture.session.begin_observation())
                    .await
                    .unwrap()
                    .unwrap();
            zenclash_core::ProxyOperations::from_observation(&observation)
                .observe_workspace(zenclash_core::ProxyVisibility::VisibleOnly)
                .await
                .unwrap()
        });
        assert_eq!(
            workspace.catalog().groups()[0].behavior,
            ProxyGroupBehavior::Automatic { fixed: false }
        );
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            assert!(page.read(cx).catalog.is_none());
            assert!(page.read(cx).notice.is_none());
            assert!(!page.read(cx).operation_pending());
            window.remove_window();
        })
        .unwrap();
    });
}

#[gpui_kit::test]
fn a_completed_group_measurement_does_not_publish_after_a_same_runtime_config_change(
    cx: &mut TestAppContext,
) {
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let mut fixture = ProxyConfigurationFixture::with_group_delay_paused();
        let (window, page) = open_ready_session_page_window(cx, &fixture).await;
        page.update(cx, |page, cx| {
            page.measure_group_and_restore_auto("group".into(), None, cx)
        });
        let started = fixture.paused_request_started.take().unwrap();
        fixture
            .runtime
            .spawn(async move {
                tokio::time::timeout(Duration::from_secs(2), started)
                    .await
                    .unwrap()
                    .unwrap();
            })
            .await
            .unwrap();
        fixture.release_response.take().unwrap().send(()).unwrap();
        // Keep the foreground callback parked while the business task completes and configuration changes.
        fixture
            .runtime
            .block_on(fixture.session.begin_observation())
            .unwrap();
        fixture
            .runtime
            .block_on(fixture.session.set_mode(&fixture.store, "rule"))
            .unwrap();
        wait_for_page_state(cx, &fixture, &page, |page| {
            page.measuring_and_restoring_auto.is_none()
        })
        .await;
        cx.update_window(window, |_, window, cx| {
            let state = page.read(cx);
            assert_eq!(
                state.catalog.as_ref().unwrap().groups()[0].behavior,
                ProxyGroupBehavior::Automatic { fixed: true }
            );
            assert!(
                state
                    .catalog
                    .as_ref()
                    .unwrap()
                    .node(&ProxyNodeId::new("node".into(), None))
                    .unwrap()
                    .latest_delay()
                    .is_none()
            );
            assert!(state.notice.is_none());
            assert!(state.error.is_some());
            window.remove_window();
        })
        .unwrap();
    });
}
