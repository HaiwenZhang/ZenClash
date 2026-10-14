use crate::pages::proxies::{
    ProxyNodeId,
    test_support::{ProxyConfigurationFixture, open_ready_session_page_window},
    view::proxy_element_id,
};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, TestAppContext};
use std::time::Duration;

#[gpui_kit::test]
fn session_selection_button_confirms_actual_member_without_optimistic_replacement(
    cx: &mut TestAppContext,
) {
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let mut fixture = ProxyConfigurationFixture::with_first_selection_readback_paused();
        let (window, page) = open_ready_session_page_window(cx, &fixture).await;
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click(
                proxy_element_id(
                    "select-proxy",
                    "group",
                    &ProxyNodeId::new("Requested".into(), None),
                ),
                cx,
            );
        })
        .unwrap();
        let started = fixture.paused_request_started.take().unwrap();
        for _ in 0..200 {
            if cx.update(|cx| {
                !page.read(cx).switching.any_pending()
                    && page.read(cx).switching.status_text("group").is_some()
            }) {
                break;
            }
            fixture
                .runtime
                .spawn(async {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
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
        cx.update(|cx| {
            assert_eq!(
                page.read(cx).catalog.as_ref().unwrap().groups()[0].now,
                "node"
            );
            assert!(!page.read(cx).switching.any_pending());
        });
        fixture.release_response.take().unwrap().send(()).unwrap();
        for _ in 0..200 {
            if cx.update(|cx| page.read(cx).catalog.as_ref().unwrap().groups()[0].now == "Actual") {
                break;
            }
            fixture
                .runtime
                .spawn(async {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        cx.update_window(window, |_, window, cx| {
            assert_eq!(
                page.read(cx).catalog.as_ref().unwrap().groups()[0].now,
                "Actual"
            );
            assert!(page.read(cx).switching.status_text("group").is_none());
            window.remove_window();
        })
        .unwrap();
    });
}

#[gpui_kit::test]
fn selection_readback_cannot_overwrite_a_new_catalog_in_the_same_configuration(
    cx: &mut TestAppContext,
) {
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let mut fixture = ProxyConfigurationFixture::with_first_selection_readback_paused();
        let (window, page) = open_ready_session_page_window(cx, &fixture).await;
        page.update(cx, |page, cx| {
            page.change_proxy("group".into(), "Requested".into(), cx)
        });
        let started = fixture.paused_request_started.take().unwrap();
        for _ in 0..200 {
            if cx.update(|cx| {
                !page.read(cx).switching.any_pending()
                    && page.read(cx).switching.status_text("group").is_some()
            }) {
                break;
            }
            fixture
                .runtime
                .spawn(async {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
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
        let generation = fixture.session.generation();
        page.update(cx, |page, cx| page.reload(cx));
        for _ in 0..200 {
            if cx.update(|cx| !page.read(cx).loading) {
                break;
            }
            fixture
                .runtime
                .spawn(async {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        fixture.release_response.take().unwrap().send(()).unwrap();
        for _ in 0..200 {
            if cx.update(|cx| page.read(cx).switching.status_text("group").is_none()) {
                break;
            }
            fixture
                .runtime
                .spawn(async {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        assert_eq!(fixture.session.generation(), generation);
        cx.update_window(window, |_, window, cx| {
            assert_eq!(
                page.read(cx).catalog.as_ref().unwrap().groups()[0].now,
                "node"
            );
            assert!(page.read(cx).switching.status_text("group").is_none());
            window.remove_window();
        })
        .unwrap();
    });
}

#[gpui_kit::test]
fn selection_readback_cannot_confirm_the_same_runtime_after_configuration_change(
    cx: &mut TestAppContext,
) {
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let mut fixture = ProxyConfigurationFixture::with_first_selection_readback_paused();
        let (window, page) = open_ready_session_page_window(cx, &fixture).await;
        page.update(cx, |page, cx| {
            page.change_proxy("group".into(), "Requested".into(), cx)
        });
        let started = fixture.paused_request_started.take().unwrap();
        for _ in 0..200 {
            if cx.update(|cx| {
                !page.read(cx).switching.any_pending()
                    && page.read(cx).switching.status_text("group").is_some()
            }) {
                break;
            }
            fixture
                .runtime
                .spawn(async {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
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
        fixture
            .runtime
            .block_on(fixture.session.set_mode(&fixture.store, "rule"))
            .unwrap();
        fixture.release_response.take().unwrap().send(()).unwrap();
        for _ in 0..200 {
            if cx.update(|cx| {
                page.read(cx)
                    .switching
                    .status_text("group")
                    .is_some_and(|text| {
                        text.contains(&zenclash_i18n::text("proxies.selection.runtime_changed"))
                    })
            }) {
                break;
            }
            fixture
                .runtime
                .spawn(async {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        cx.update_window(window, |_, window, cx| {
            assert_eq!(
                page.read(cx).catalog.as_ref().unwrap().groups()[0].now,
                "node"
            );
            assert!(
                page.read(cx)
                    .switching
                    .status_text("group")
                    .unwrap()
                    .contains(&zenclash_i18n::text("proxies.selection.runtime_changed"))
            );
            window.remove_window();
        })
        .unwrap();
    });
}

#[gpui_kit::test]
fn a_second_group_can_queue_while_the_first_admitted_selection_waits_for_acknowledgement(
    cx: &mut TestAppContext,
) {
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let mut fixture = ProxyConfigurationFixture::with_first_selection_acknowledgement_paused();
        let (window, page) = open_ready_session_page_window(cx, &fixture).await;
        page.update(cx, |page, cx| {
            page.change_proxy("group".into(), "Requested".into(), cx)
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
        page.update(cx, |page, cx| {
            assert!(page.switching.group_pending("group"));
            page.change_proxy("other".into(), "Requested".into(), cx);
            assert!(
                page.switching.group_pending("other"),
                "the second group was rejected instead of queued"
            );
            assert!(page.error.is_none());
        });
        fixture.release_response.take().unwrap().send(()).unwrap();
        for _ in 0..200 {
            if cx.update(|cx| {
                page.read(cx)
                    .catalog
                    .as_ref()
                    .unwrap()
                    .groups()
                    .iter()
                    .all(|group| group.now == "Actual")
            }) {
                break;
            }
            fixture
                .runtime
                .spawn(async {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        cx.update_window(window, |_, window, cx| {
            assert!(
                page.read(cx)
                    .catalog
                    .as_ref()
                    .unwrap()
                    .groups()
                    .iter()
                    .all(|group| group.now == "Actual")
            );
            assert!(page.read(cx).switching.status_text("group").is_none());
            assert!(page.read(cx).switching.status_text("other").is_none());
            assert!(page.read(cx).error.is_none());
            window.remove_window();
        })
        .unwrap();
    });
}

#[gpui_kit::test]
fn a_selection_error_cannot_replace_feedback_after_configuration_change(cx: &mut TestAppContext) {
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let fixture = ProxyConfigurationFixture::new();
        let (window, page) = open_ready_session_page_window(cx, &fixture).await;
        page.update(cx, |page, cx| {
            page.change_proxy("group".into(), String::new(), cx)
        });
        fixture
            .runtime
            .block_on(fixture.session.set_mode(&fixture.store, "rule"))
            .unwrap();
        page.update(cx, |page, _| {
            page.error = Some("current configuration feedback".into())
        });
        for _ in 0..200 {
            if cx.update(|cx| !page.read(cx).switching.any_pending()) {
                break;
            }
            fixture
                .runtime
                .spawn(async {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        cx.update_window(window, |_, window, cx| {
            assert!(!page.read(cx).switching.any_pending());
            assert_eq!(
                page.read(cx).error.as_deref(),
                Some("current configuration feedback")
            );
            window.remove_window();
        })
        .unwrap();
    });
}

#[gpui_kit::test]
fn a_readback_error_is_preserved_when_current_and_rejected_after_configuration_change(
    cx: &mut TestAppContext,
) {
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let fixture = ProxyConfigurationFixture::new();
        let (window, page) = open_ready_session_page_window(cx, &fixture).await;
        let (operations, request) = page.update(cx, |page, cx| {
            let operations = page.capture_catalog_mutation_source(cx).unwrap();
            let request = page
                .switching
                .start("group".into(), "Requested".into())
                .unwrap();
            (operations, request)
        });
        let accepted = fixture
            .runtime
            .block_on(fixture.session.accept_observed_proxy_selection(
                operations.observation_identity().unwrap(),
                "group",
                "Requested",
                zenclash_core::ConnectionPolicy::KeepExisting,
            ))
            .unwrap();
        fixture.fail_selection_readback();
        let readback = fixture.runtime.block_on(accepted.read_actual());
        assert!(matches!(
            readback,
            zenclash_core::ProxySelectionReadback::Unavailable(_)
        ));
        page.update(cx, |page, _| {
            assert!(page.switching.acknowledge(&request));
            assert!(page.switching.observe(
                &request,
                &accepted,
                readback,
                page.catalog.as_mut().map(std::sync::Arc::make_mut)
            ));
            assert!(page.switching.status_text("group").unwrap().contains("503"));
        });
        let old_error = fixture.runtime.block_on(accepted.read_actual());
        fixture
            .runtime
            .block_on(fixture.session.set_mode(&fixture.store, "rule"))
            .unwrap();
        cx.update_window(window, |_, window, cx| {
            page.update(cx, |page, _| {
                assert!(page.switching.observe(
                    &request,
                    &accepted,
                    old_error,
                    page.catalog.as_mut().map(std::sync::Arc::make_mut)
                ));
                let status = page.switching.status_text("group").unwrap();
                assert!(status.contains(&zenclash_i18n::text("proxies.selection.runtime_changed")));
                assert!(!status.contains("503"));
                assert!(
                    page.switching
                        .start("group".into(), "Requested".into())
                        .is_some()
                );
                assert_eq!(page.catalog.as_ref().unwrap().groups()[0].now, "node");
            });
            window.remove_window();
        })
        .unwrap();
    });
}
