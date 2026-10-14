use super::*;
use crate::pages::proxies::{
    ProxyCatalog, ProxyGroup, ProxyNode, ProxyNodeId, test_support::open_catalog,
};
use gpui_kit::{AppContext, TestAppContext};
use zenclash_core::{CoreKind, CoreSession, MihomoClient, MihomoEndpoint};

fn catalog() -> ProxyCatalog {
    ProxyCatalog::from_group_nodes(
        vec![(
            ProxyGroup::new("group"),
            vec![ProxyNode {
                name: "node".into(),
                ..Default::default()
            }],
        )],
        1,
    )
}

#[gpui_kit::test]
fn a_completed_delay_cannot_publish_after_its_runtime_changes(cx: &mut TestAppContext) {
    let (window, page, runtime) = open_catalog(cx, catalog());
    let (client, operations, token) = cx.update(|cx| {
        page.update(cx, |page, cx| {
            let operations = page.capture_catalog_operations(cx).unwrap();
            (
                page.client.clone(),
                operations,
                DelayTaskToken(page.delay_generation),
            )
        })
    });
    let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
    runtime
        .block_on(session.switch_to_direct(MihomoEndpoint::new("http://127.0.0.1:9", "")))
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, _| {
            let node = ProxyNodeId::new("node".to_owned(), None);
            assert!(!page.publish_delay_result(token, &operations, |page| {
                page.record_node_delay(&node, 99, 99, None)
            }));
            assert_eq!(
                page.catalog
                    .as_ref()
                    .unwrap()
                    .node(&node)
                    .unwrap()
                    .latest_delay(),
                None
            );
        });
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn replacing_a_catalog_rejects_old_delay_completion_and_keeps_new_testing_state(
    cx: &mut TestAppContext,
) {
    let (window, page, _runtime) = open_catalog(cx, catalog());
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            let operations = page.capture_catalog_operations(cx).unwrap();
            let token = DelayTaskToken(page.delay_generation);
            page.start_proxy_test("group", "node").unwrap();
            page.install_catalog(catalog(), "rule".into(), vec![0]);
            page.start_proxy_test("group", "node").unwrap();
            let node = ProxyNodeId::new("node".to_owned(), None);
            assert!(!page.publish_delay_result(token, &operations, |page| {
                page.record_node_delay(&node, 99, 99, None)
            }));
            assert!(page.testing["group"].contains(&node));
            assert_eq!(
                page.catalog
                    .as_ref()
                    .unwrap()
                    .node(&node)
                    .unwrap()
                    .latest_delay(),
                None
            );
        });
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn current_delay_updates_do_not_replace_catalog_query_identity(cx: &mut TestAppContext) {
    let (window, page, _runtime) = open_catalog(cx, catalog());
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            let operations = page.capture_catalog_operations(cx).unwrap();
            let token = DelayTaskToken(page.delay_generation);
            let node = ProxyNodeId::new("node".to_owned(), None);
            assert!(page.publish_delay_result(token, &operations, |page| {
                page.record_node_delay(&node, 42, 42, None)
            }));
            assert!(page.publish_delay_result(token, &operations, |page| {
                page.record_node_delay(&node, 77, 77, None)
            }));
            assert_eq!(
                page.catalog
                    .as_ref()
                    .unwrap()
                    .node(&node)
                    .unwrap()
                    .latest_delay(),
                Some(77)
            );
        });
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_late_group_completion_cannot_publish_its_old_runtime_diagnostic(cx: &mut TestAppContext) {
    let (window, page, runtime) = open_catalog(cx, catalog());
    let (client, operations, token) = cx.update(|cx| {
        page.update(cx, |page, cx| {
            let operations = page.capture_catalog_operations(cx).unwrap();
            page.group_progress.insert("group".into(), (1, 1));
            (
                page.client.clone(),
                operations,
                DelayTaskToken(page.delay_generation),
            )
        })
    });
    let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
    runtime
        .block_on(session.switch_to_direct(MihomoEndpoint::new("http://127.0.0.1:9", "")))
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.error = Some("current feedback".into());
            page.finish_group_measurement(
                token,
                &operations,
                "group",
                Some("old task failed".into()),
                cx,
            );
            assert_eq!(page.error.as_deref(), Some("current feedback"));
            assert!(!page.group_progress.contains_key("group"));
        });
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_running_group_batch_does_not_send_later_nodes_to_a_replacement_controller(
    cx: &mut TestAppContext,
) {
    use tokio::io::AsyncWriteExt;
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        let catalog = ProxyCatalog::from_group_nodes(vec![(ProxyGroup::new("group"),
            (0..18).map(|index| ProxyNode { name: format!("node-{index}"), kind: "Shadowsocks".into(), ..Default::default() }).collect())], 18);
        let (window, page, runtime) = open_catalog(cx, catalog);
        let original = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let original_url = format!("http://{}", original.local_addr().unwrap());
        original.set_nonblocking(true).unwrap();
        let replacement = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let replacement_url = format!("http://{}", replacement.local_addr().unwrap());
        replacement.set_nonblocking(true).unwrap();
        let (all_started, started) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::watch::channel(false);
        let server = runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(original).unwrap();
            let mut replies = tokio::task::JoinSet::new();
            for _ in 0..16 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let headers = zenclash_core::test_support::read_controller_request(&mut stream).await.unwrap().into_bytes();
                assert!(String::from_utf8_lossy(&headers).starts_with("GET /proxies/node"));
                assert!(String::from_utf8_lossy(&headers).contains("/delay?"));
                let mut released = released.clone();
                replies.spawn(async move {
                    while !*released.borrow_and_update() { released.changed().await.unwrap(); }
                    let body = "{\"delay\":42}";
                    let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
                });
            }
            all_started.send(()).unwrap();
            while let Some(reply) = replies.join_next().await { reply.unwrap(); }
        });
        let client = MihomoClient::new(MihomoEndpoint::new(original_url, "")).unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
        cx.update(|cx| page.update(cx, |page, cx| {
            page.client = client;
            page.test_group("group", cx);
        }));
        runtime.spawn(async move { tokio::time::timeout(std::time::Duration::from_secs(3), started).await.unwrap().unwrap(); }).await.unwrap();
        let replacement_server = runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(replacement).unwrap();
            let mut count = 0;
            while let Ok(Ok((mut stream, _))) = tokio::time::timeout(std::time::Duration::from_millis(300), listener.accept()).await {
                let _headers = zenclash_core::test_support::read_controller_request(&mut stream).await.unwrap().into_bytes();
                count += 1;
                let body = "{\"delay\":77}";
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
            count
        });
        runtime.spawn(async move { session.switch_to_direct(MihomoEndpoint::new(replacement_url, "")).await.unwrap(); }).await.unwrap();
        release.send(true).unwrap();
        server.await.unwrap();
        for _ in 0..200 {
            cx.executor().advance_clock(std::time::Duration::from_millis(50));
            if cx.update(|cx| page.read(cx).group_progress.is_empty()) { break; }
            runtime.spawn(async { tokio::time::sleep(std::time::Duration::from_millis(5)).await; }).await.unwrap();
        }
        let contacted = replacement_server.await.unwrap();
        cx.update_window(window, |_, window, cx| {
            assert!(page.read(cx).group_progress.is_empty());
            assert!(page.read(cx).testing.is_empty());
            let catalog = page.read(cx).catalog.as_ref().unwrap();
            assert!(catalog.groups()[0].all.iter().all(|id| catalog.node(id).unwrap().latest_delay().is_none()));
            window.remove_window();
        }).unwrap();
        assert_eq!(contacted, 0, "replacement received nodes from the old batch");
    });
}

#[gpui_kit::test]
fn session_catalog_rejects_old_delays_after_mode_change_and_accepts_new_measurements(
    cx: &mut TestAppContext,
) {
    use crate::pages::proxies::test_support::ProxyConfigurationFixture;
    use gpui_kit::test::TestWindowExt;
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let fixture = ProxyConfigurationFixture::new();
        let (window, page) =
            crate::pages::proxies::test_support::open_session_page_window(cx, &fixture);
        page.update(cx, |page, cx| page.refresh(cx));
        for _ in 0..200 {
            if cx.update(|cx| page.read(cx).catalog.is_some()) {
                break;
            }
            fixture
                .runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        let (operations, token) = cx.update(|cx| {
            page.update(cx, |page, cx| {
                assert!(
                    page.catalog.is_some(),
                    "actual session page never loaded its workspace"
                );
                (
                    page.capture_catalog_operations(cx).unwrap(),
                    DelayTaskToken(page.delay_generation),
                )
            })
        });
        let original_descriptor = fixture.session.runtime_descriptor();
        fixture
            .runtime
            .block_on(fixture.session.set_mode(&fixture.store, "direct"))
            .unwrap();
        assert_eq!(
            fixture.session.runtime_descriptor().binding_generation(),
            original_descriptor.binding_generation()
        );
        page.update(cx, |page, cx| {
            let node = ProxyNodeId::new("node".to_owned(), None);
            assert!(!page.publish_delay_result(token, &operations, |page| {
                page.record_node_delay(&node, 99, 99, None)
            }));
            assert!(
                page.catalog
                    .as_ref()
                    .unwrap()
                    .node(&node)
                    .unwrap()
                    .latest_delay()
                    .is_none()
            );
            page.test_group("group", cx);
            assert!(
                page.testing.is_empty(),
                "old catalog started a new configuration measurement"
            );
            assert!(page.error.is_some());
        });
        fixture
            .runtime
            .block_on(fixture.session.set_mode(&fixture.store, "rule"))
            .unwrap();
        page.update(cx, |page, cx| {
            page.synchronize_outbound_mode("rule", false, cx)
        });
        for _ in 0..200 {
            if cx.update(|cx| !page.read(cx).loading && page.read(cx).outbound_mode == "rule") {
                break;
            }
            fixture
                .runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        cx.update_window(window, |_, window, cx| {
            assert_eq!(page.read(cx).outbound_mode, "rule");
            page.update(cx, |page, cx| {
                if page.selected_group.as_deref() != Some("group") {
                    page.toggle_group("group", cx);
                }
            });
            window.render_frame(cx);
            window.click(
                crate::pages::proxies::view::proxy_element_id(
                    "test-proxy",
                    "group",
                    &ProxyNodeId::new("node".into(), None),
                ),
                cx,
            );
        })
        .unwrap();
        let node = ProxyNodeId::new("node".into(), None);
        for _ in 0..200 {
            if cx.update(|cx| {
                page.read(cx)
                    .catalog
                    .as_ref()
                    .unwrap()
                    .node(&node)
                    .unwrap()
                    .latest_delay()
                    == Some(42)
            }) {
                break;
            }
            fixture
                .runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        cx.update_window(window, |_, window, cx| {
            assert_eq!(
                page.read(cx)
                    .catalog
                    .as_ref()
                    .unwrap()
                    .node(&node)
                    .unwrap()
                    .latest_delay(),
                Some(42)
            );
            page.update(cx, |page, cx| {
                page.suspend();
                page.synchronize_outbound_mode("direct", false, cx);
                assert!(
                    !page.loading,
                    "mode synchronization reloaded an inactive page"
                );
                assert!(page.catalog.is_none());
            });
            window.remove_window();
        })
        .unwrap();
    });
}

#[gpui_kit::test]
fn session_page_cannot_start_operations_from_an_unobserved_catalog(cx: &mut TestAppContext) {
    use crate::pages::proxies::test_support::ProxyConfigurationFixture;
    let fixture = ProxyConfigurationFixture::new();
    cx.update(gpui_kit::init);
    let page = cx.new(|cx| {
        ProxiesPage::new_with_session(
            fixture.session.clone(),
            fixture.runtime.handle().clone(),
            cx,
        )
    });
    page.update(cx, |page, cx| {
        page.install_catalog(catalog(), "rule".into(), vec![0]);
        page.test_group("group", cx);
        assert!(page.testing.is_empty());
        assert!(page.error.is_some());
    });
}

#[gpui_kit::test]
fn mode_completion_restarts_an_active_first_workspace_read(cx: &mut TestAppContext) {
    use crate::pages::proxies::test_support::ProxyConfigurationFixture;
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let mut fixture = ProxyConfigurationFixture::with_first_catalog_paused();
        let (window, page) =
            crate::pages::proxies::test_support::open_session_page_window(cx, &fixture);
        page.update(cx, |page, cx| page.refresh(cx));
        let started = fixture.paused_request_started.take().unwrap();
        fixture
            .runtime
            .spawn(async move {
                tokio::time::timeout(std::time::Duration::from_secs(2), started)
                    .await
                    .unwrap()
                    .unwrap();
            })
            .await
            .unwrap();
        cx.update(|cx| {
            assert!(page.read(cx).loading);
            assert!(page.read(cx).catalog.is_none());
        });
        fixture
            .runtime
            .block_on(fixture.session.set_mode(&fixture.store, "direct"))
            .unwrap();
        page.update(cx, |page, cx| {
            page.synchronize_outbound_mode("direct", false, cx)
        });
        fixture.release_response.take().unwrap().send(()).unwrap();
        for _ in 0..200 {
            if cx.update(|cx| !page.read(cx).loading) {
                break;
            }
            fixture
                .runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        cx.update_window(window, |_, window, cx| {
            assert!(!page.read(cx).loading);
            assert!(
                page.read(cx).catalog.is_some(),
                "first active read stayed empty after mode completion"
            );
            assert_eq!(page.read(cx).outbound_mode, "direct");
            assert!(
                page.read(cx)
                    .catalog_operations
                    .as_ref()
                    .unwrap()
                    .pin_current_runtime()
                    .is_ok()
            );
            assert!(page.read(cx).error.is_none());
            window.remove_window();
        })
        .unwrap();
    });
}

#[gpui_kit::test]
fn settled_mode_reloads_a_presented_page_after_its_first_stale_read_finishes(
    cx: &mut TestAppContext,
) {
    use crate::pages::proxies::test_support::ProxyConfigurationFixture;
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let mut fixture = ProxyConfigurationFixture::with_first_catalog_paused();
        let (window, page) =
            crate::pages::proxies::test_support::open_session_page_window(cx, &fixture);
        page.update(cx, |page, cx| page.refresh(cx));
        let started = fixture.paused_request_started.take().unwrap();
        fixture
            .runtime
            .spawn(async move {
                tokio::time::timeout(std::time::Duration::from_secs(2), started)
                    .await
                    .unwrap()
                    .unwrap();
            })
            .await
            .unwrap();
        fixture
            .runtime
            .block_on(fixture.session.set_mode(&fixture.store, "direct"))
            .unwrap();
        page.update(cx, |page, cx| {
            page.synchronize_outbound_mode("direct", true, cx)
        });
        fixture.release_response.take().unwrap().send(()).unwrap();
        for _ in 0..200 {
            if cx.update(|cx| !page.read(cx).loading) {
                break;
            }
            fixture
                .runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        cx.update(|cx| {
            assert!(!page.read(cx).loading);
            assert!(page.read(cx).catalog.is_none());
            assert!(
                page.read(cx).error.is_some(),
                "old read never reached stale completion"
            );
        });
        page.update(cx, |page, cx| {
            page.synchronize_outbound_mode("direct", false, cx)
        });
        for _ in 0..200 {
            if cx.update(|cx| !page.read(cx).loading) {
                break;
            }
            fixture
                .runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        cx.update_window(window, |_, window, cx| {
            assert!(!page.read(cx).loading);
            assert!(
                page.read(cx).catalog.is_some(),
                "settled mode left a presented page empty"
            );
            assert_eq!(page.read(cx).outbound_mode, "direct");
            assert!(page.read(cx).error.is_none());
            window.remove_window();
        })
        .unwrap();
    });
}

#[gpui_kit::test]
fn an_initial_catalog_refresh_http_failure_remains_visible(cx: &mut TestAppContext) {
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let fixture = crate::pages::proxies::test_support::ProxyConfigurationFixture::new();
        fixture.fail_workspace_reads();
        let (window, page) =
            crate::pages::proxies::test_support::open_session_page_window(cx, &fixture);
        page.update(cx, |page, cx| page.refresh(cx));
        for _ in 0..200 {
            if cx.update(|cx| !page.read(cx).loading) {
                break;
            }
            fixture
                .runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        cx.update_window(window, |_, window, cx| {
            assert!(!page.read(cx).loading);
            assert!(page.read(cx).catalog.is_none());
            assert!(page.read(cx).error.as_deref().unwrap().contains("503"));
            window.remove_window();
        })
        .unwrap();
    });
}

#[gpui_kit::test]
fn catalog_refresh_failures_cannot_cross_configuration_or_query_replacement(
    cx: &mut TestAppContext,
) {
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let fixture = crate::pages::proxies::test_support::ProxyConfigurationFixture::new();
        let (window, page) =
            crate::pages::proxies::test_support::open_ready_session_page_window(cx, &fixture).await;
        let source = ProxyCatalogReadSource::default();
        let operations = fixture
            .runtime
            .block_on(source.capture(
                Some(fixture.session.clone()),
                fixture.session.client().clone(),
            ))
            .unwrap();
        fixture.fail_workspace_reads();
        let error = fixture
            .runtime
            .block_on(operations.observe_workspace(zenclash_core::ProxyVisibility::VisibleOnly))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("503"));
        let token = page.update(cx, |page, _| page.begin_catalog_operation());
        fixture
            .runtime
            .block_on(fixture.session.set_mode(&fixture.store, "rule"))
            .unwrap();
        page.update(cx, |page, _| {
            page.error = Some("current configuration feedback".into());
            assert!(!page.publish_catalog_refresh_error(token, &source, error.clone()));
            assert_eq!(
                page.error.as_deref(),
                Some("current configuration feedback")
            );
        });
        let current_source = ProxyCatalogReadSource::default();
        let current_operations = fixture
            .runtime
            .block_on(current_source.capture(
                Some(fixture.session.clone()),
                fixture.session.client().clone(),
            ))
            .unwrap();
        let current_error = fixture
            .runtime
            .block_on(
                current_operations.observe_workspace(zenclash_core::ProxyVisibility::VisibleOnly),
            )
            .err()
            .unwrap()
            .to_string();
        page.update(cx, |page, _| {
            assert!(page.publish_catalog_refresh_error(
                token,
                &current_source,
                current_error.clone()
            ));
            assert!(page.error.as_deref().unwrap().contains("503"));
            page.begin_catalog_operation();
            page.error = Some("new query feedback".into());
            assert!(!page.publish_catalog_refresh_error(token, &current_source, current_error));
            assert_eq!(page.error.as_deref(), Some("new query feedback"));
        });
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
    });
}

#[gpui_kit::test]
fn a_panicked_catalog_worker_keeps_its_source_for_failure_publication(cx: &mut TestAppContext) {
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let fixture = crate::pages::proxies::test_support::ProxyConfigurationFixture::new();
        let (window, page) =
            crate::pages::proxies::test_support::open_ready_session_page_window(cx, &fixture).await;
        let source = std::sync::Arc::new(ProxyCatalogReadSource::default());
        let submitted = source.clone();
        let session = fixture.session.clone();
        let failure = fixture
            .runtime
            .block_on(fixture.runtime.spawn(async move {
                let operations = submitted
                    .capture(Some(session.clone()), session.client().clone())
                    .await
                    .unwrap();
                operations
                    .observe_workspace(zenclash_core::ProxyVisibility::VisibleOnly)
                    .await
                    .unwrap();
                panic!("fixture catalog worker failed after its controller read");
            }))
            .unwrap_err()
            .to_string();
        let token = page.update(cx, |page, _| page.begin_catalog_operation());
        page.update(cx, |page, _| {
            assert!(page.publish_catalog_refresh_error(token, &source, failure.clone()));
            assert!(
                page.error
                    .as_deref()
                    .unwrap()
                    .contains("fixture catalog worker")
            );
        });
        fixture
            .runtime
            .block_on(fixture.session.set_mode(&fixture.store, "rule"))
            .unwrap();
        cx.update_window(window, |_, window, cx| {
            page.update(cx, |page, _| {
                page.error = Some("current configuration feedback".into());
                assert!(!page.publish_catalog_refresh_error(token, &source, failure));
                assert_eq!(
                    page.error.as_deref(),
                    Some("current configuration feedback")
                );
            });
            window.remove_window();
        })
        .unwrap();
    });
}

#[gpui_kit::test]
fn a_foreign_catalog_read_cannot_publish_equal_counter_errors(cx: &mut TestAppContext) {
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let fixture = crate::pages::proxies::test_support::ProxyConfigurationFixture::new();
        let (window, page) =
            crate::pages::proxies::test_support::open_ready_session_page_window(cx, &fixture).await;
        let foreign =
            CoreSession::open(CoreKind::Mihomo, fixture.session.client().clone()).unwrap();
        assert_eq!(fixture.session.generation(), foreign.generation());
        let source = ProxyCatalogReadSource::default();
        let operations = fixture
            .runtime
            .block_on(source.capture(Some(foreign.clone()), foreign.client().clone()))
            .unwrap();
        fixture.fail_workspace_reads();
        let error = fixture
            .runtime
            .block_on(operations.observe_workspace(zenclash_core::ProxyVisibility::VisibleOnly))
            .err()
            .unwrap()
            .to_string();
        cx.update_window(window, |_, window, cx| {
            page.update(cx, |page, _| {
                let token = page.begin_catalog_operation();
                page.error = Some("actual owner feedback".into());
                assert!(!page.publish_catalog_refresh_error(token, &source, error));
                assert_eq!(page.error.as_deref(), Some("actual owner feedback"));
            });
            window.remove_window();
        })
        .unwrap();
    });
}
