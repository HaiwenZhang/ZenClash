use super::test_support::{open_catalog, open_catalog_at};
use super::*;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, TestAppContext, size};

#[gpui_kit::test]
fn acknowledged_request_keeps_actual_member_until_delayed_confirmation(cx: &mut TestAppContext) {
    cx.foreground_executor().clone().block_test(async {
            use tokio::io::{AsyncWriteExt};
            cx.executor().allow_parking();
            let catalog = ProxyCatalog::from_group_nodes(vec![(ProxyGroup::new("wire/key")
                .with_display_name("Visible label").with_current_member("Old")
                .with_behavior(ProxyGroupBehavior::Selector),
                ["Old", "Requested", "Actual"].into_iter().map(|name| ProxyNode { name: name.into(), ..Default::default() }).collect())], 3);
            let (window, page, runtime) = open_catalog(cx, catalog);
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let (entered_tx, mut entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            let server = runtime.spawn(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let mut entered_tx = Some(entered_tx);
                let mut release_rx = Some(release_rx);
                for index in 0..2 {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let (text, _body) = zenclash_core::test_support::read_controller_request(&mut stream).await.unwrap().into_parts();
                    if index == 0 {
                        assert!(text.starts_with("PUT /proxies/wire%2Fkey "));
                        stream.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").await.unwrap();
                    } else {
                        assert!(text.starts_with("GET /proxies/wire%2Fkey "));
                        entered_tx.take().unwrap().send(()).unwrap();
                        release_rx.take().unwrap().await.unwrap();
                        let response = r#"{"now":"Actual","type":"Selector"}"#;
                        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
                    }
                }
            });
            let client = MihomoClient::new(zenclash_core::MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
            page.update(cx, |page, cx| { page.client = client; page.change_proxy("wire/key".into(), "Requested".into(), cx); });
            let mut entered = false;
            for _ in 0..200 {
                if entered_rx.try_recv().is_ok() { entered = true; break; }
                runtime.spawn(async { tokio::time::sleep(std::time::Duration::from_millis(5)).await; }).await.unwrap();
            }
            let before_confirmation = cx.update(|cx| {
                let state = page.read(cx);
                (state.catalog.as_ref().unwrap().groups()[0].now.clone(), state.switching.any_pending(), state.switching.status_text("wire/key"))
            });
            release_tx.send(()).unwrap();
            server.await.unwrap();
            for _ in 0..200 {
                if cx.update(|cx| page.read(cx).catalog.as_ref().unwrap().groups()[0].now == "Actual") { break; }
                runtime.spawn(async { tokio::time::sleep(std::time::Duration::from_millis(5)).await; }).await.unwrap();
            }
            assert!(entered, "confirmation request never entered the controlled readback gap");
            assert_eq!(before_confirmation.0, "Old");
            assert!(!before_confirmation.1, "acknowledgement waited for readback");
            assert!(before_confirmation.2.is_some());
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(page.read(cx).catalog.as_ref().unwrap().groups()[0].now, "Actual");
                assert!(page.read(cx).switching.status_text("wire/key").is_none());
                window.remove_window();
            }).unwrap();
        });
}

#[gpui_kit::test]
fn duplicate_group_labels_keep_independent_navigation_identity(cx: &mut TestAppContext) {
    let catalog = ProxyCatalog::from_group_nodes(
        ["first-key", "second-key"]
            .into_iter()
            .map(|key| {
                (
                    ProxyGroup::new(key).with_display_name("Same visible label"),
                    Vec::new(),
                )
            })
            .collect(),
        0,
    );
    let (window, page, _runtime) = open_catalog(cx, catalog);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find((gpui_kit::ElementId::from("toggle-group"), "first-key"));
        window.find((gpui_kit::ElementId::from("toggle-group"), "second-key"));
        window.click(
            (gpui_kit::ElementId::from("toggle-group"), "second-key"),
            cx,
        );
        window.render_frame(cx);
        assert_eq!(page.read(cx).selected_group.as_deref(), Some("second-key"));
        assert!(
            page.read(cx)
                .catalog
                .as_ref()
                .unwrap()
                .groups()
                .iter()
                .all(|group| group.name == "Same visible label")
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn group_delay_failures_notify_once_after_the_last_node(cx: &mut TestAppContext) {
    use gpui_kit::component::WindowExt;
    use tokio::io::AsyncWriteExt;

    cx.foreground_executor().clone().block_test(async {
            cx.executor().allow_parking();
            let catalog = ProxyCatalog::from_group_nodes(
                vec![(
                    ProxyGroup::new("Proxy").with_kind("Selector"),
                    ["first", "last"].into_iter().map(|name| ProxyNode {
                        name: name.into(), kind: "Shadowsocks".into(), ..Default::default()
                    }).collect(),
                )],
                2,
            );
            let (window, page, runtime) = open_catalog(cx, catalog);
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let (release, wait) = tokio::sync::oneshot::channel();
            let server = runtime.spawn(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let mut wait = Some(wait);
                let mut replies = tokio::task::JoinSet::new();
                for _ in 0..2 {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let headers = zenclash_core::test_support::read_controller_request(&mut stream).await.unwrap().into_bytes();
                    let request = String::from_utf8(headers).unwrap();
                    let wait = if request.starts_with("GET /proxies/last/delay?") {
                        wait.take()
                    } else {
                        assert!(request.starts_with("GET /proxies/first/delay?"));
                        None
                    };
                    replies.spawn(async move {
                        if let Some(wait) = wait { wait.await.unwrap(); }
                        let body = r#"{"message":"Timeout"}"#;
                        stream.write_all(format!("HTTP/1.1 504 Gateway Timeout\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                    });
                }
                while let Some(result) = replies.join_next().await { result.unwrap(); }
            });
            cx.update_window(window, |_, window, cx| {
                page.update(cx, |page, cx| {
                    page.client = MihomoClient::new(zenclash_core::MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
                    cx.notify();
                });
                window.render_frame(cx);
                window.click((gpui_kit::ElementId::from("test-group"), "Proxy"), cx);
            }).unwrap();
            for _ in 0..200 {
                cx.executor().advance_clock(std::time::Duration::from_millis(50));
                if cx.update(|cx| page.read(cx).group_progress.get("Proxy") == Some(&(1, 2))) { break; }
                runtime.spawn(async { tokio::time::sleep(std::time::Duration::from_millis(5)).await; }).await.unwrap();
            }
            cx.update_window(window, |_, window, cx| {
                assert_eq!(page.read(cx).group_progress.get("Proxy"), Some(&(1, 2)));
                assert_eq!(page.read(cx).catalog_presentation.failures().len(), 1);
                assert!(page.read(cx).error.is_none(), "group failure feedback was published before all nodes finished");
                assert!(window.notifications(cx).is_empty());
            }).unwrap();
            release.send(()).unwrap();
            server.await.unwrap();
            for _ in 0..200 {
                cx.executor().advance_clock(std::time::Duration::from_millis(50));
                if cx.update(|cx| !page.read(cx).group_progress.contains_key("Proxy")) { break; }
                runtime.spawn(async { tokio::time::sleep(std::time::Duration::from_millis(5)).await; }).await.unwrap();
            }
            cx.update_window(window, |_, window, cx| {
                assert!(!page.read(cx).group_progress.contains_key("Proxy"));
                assert_eq!(page.read(cx).catalog_presentation.failures().len(), 2);
                let error = page.read(cx).error.clone().unwrap();
                assert_eq!(error, zenclash_i18n::text_with(
                    "proxies.errors.group_failed",
                    &[("group", "Proxy".into()), ("count", "2".into())],
                ));
                assert_eq!(window.notifications(cx).len(), 1);
                assert_eq!(page.read(cx).feedback_notifications.current_message("proxy-error"), Some(error));
                window.remove_window();
            }).unwrap();
        });
}

#[gpui_kit::test]
fn localized_search_placeholder_refresh_preserves_the_input_and_query(cx: &mut TestAppContext) {
    let (window, page, _runtime) = open_catalog(cx, ProxyCatalog::default());
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| page.ensure_search_input(window, cx));
        let input = page.read(cx).search_input.clone().unwrap();
        input.update(cx, |input, cx| {
            input.set_value("Tokyo", window, cx);
            input.set_placeholder("stale locale", window, cx);
        });
        page.update(cx, |page, cx| {
            page.refresh_localized_placeholders(window, cx)
        });
        assert_eq!(page.read(cx).search_input.as_ref().unwrap(), &input);
        assert_eq!(input.read(cx).value().as_str(), "Tokyo");
        assert_eq!(
            input.read(cx).presentation().placeholder().as_str(),
            zenclash_i18n::text("proxies.design.search")
        );
        page.update(cx, |page, _| page.suspend());
    })
    .unwrap();
}

#[gpui_kit::test]
fn refreshed_healthy_catalog_restores_a_previously_failed_search_result(cx: &mut TestAppContext) {
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        let healthy = ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup::new("Proxy"),
                vec![ProxyNode {
                    name: "Tokyo".into(),
                    history: vec![DelayHistory {
                        delay: 10,
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
            )],
            1,
        );
        let (_window, page, runtime) = open_catalog(cx, healthy.clone());
        page.update(cx, |page, cx| {
            let node = page.catalog.as_ref().unwrap().groups()[0].all[0].clone();
            page.record_node_delay(&node, 0, 0, Some(DelayTestFailure::Timeout));
            page.search_query = "tokyo".into();
            page.hide_unavailable = true;
            page.install_catalog(healthy, "rule".into(), vec![0]);
            page.prepare_search(cx);
        });
        for _ in 0..200 {
            if cx.update(|cx| page.read(cx).search_projection.is_some()) {
                break;
            }
            runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        cx.update(|cx| {
            let state = page.read(cx);
            assert!(state.catalog_presentation.failures().is_empty());
            assert!(state.search_projection.is_some());
            let catalog = state.catalog.as_ref().unwrap();
            assert_eq!(&*state.displayed_nodes(catalog, &catalog.groups()[0]), &[0]);
        });
        page.update(cx, |page, _| page.suspend());
    });
}

#[gpui_kit::test]
fn local_search_filters_nodes_and_clearing_restores_the_full_list(cx: &mut TestAppContext) {
    cx.foreground_executor().clone().block_test(async {
        cx.executor().allow_parking();
        let catalog = ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup::new("Proxy"),
                ["Tokyo", "London"]
                    .into_iter()
                    .map(|name| ProxyNode {
                        name: name.into(),
                        ..Default::default()
                    })
                    .collect(),
            )],
            2,
        );
        let (window, page, runtime) = open_catalog(cx, catalog);
        let search_guard = cx
            .update(|cx| page.read(cx).search_gate.clone())
            .try_lock_owned()
            .expect("search must be idle before input");
        let panel_top = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.click("proxy-node-search", cx);
                let panel_top = window.find("proxy-node-panel").bounds().top();
                window.input("TOKYO", cx);
                panel_top
            })
            .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(page.read(cx).search_projection.is_none());
            assert_eq!(window.find("proxy-node-panel").bounds().top(), panel_top);
            assert!(
                window.find("proxy-search-feedback").bounds().top() > panel_top,
                "search feedback must stay inside the node panel"
            );
            assert_eq!(
                Arc::strong_count(page.read(cx).catalog.as_ref().unwrap()),
                1
            );
        })
        .unwrap();
        drop(search_guard);
        for _ in 0..200 {
            if cx.update(|cx| page.read(cx).search_projection.is_some()) {
                break;
            }
            runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        cx.update(|cx| {
            let state = page.read(cx);
            assert_eq!(state.search_query, "tokyo");
            assert!(state.search_projection.is_some());
            let catalog = state.catalog.as_ref().unwrap();
            assert_eq!(&*state.displayed_nodes(catalog, &catalog.groups()[0]), &[0]);
        });
        cx.update_window(window, |_, window, cx| {
            window.press("secondary-a", cx);
            window.press("backspace", cx);
        })
        .unwrap();
        cx.update(|cx| {
            let state = page.read(cx);
            assert!(state.search_query.is_empty());
            let catalog = state.catalog.as_ref().unwrap();
            assert_eq!(
                &*state.displayed_nodes(catalog, &catalog.groups()[0]),
                &[0, 1]
            );
        });
        page.update(cx, |page, _| page.suspend());
    });
}

#[gpui_kit::test]
fn identical_node_labels_keep_independent_keyboard_selection_and_delay_controls(
    cx: &mut TestAppContext,
) {
    assert_provider_node_keyboard_controls(cx, size(px(1200.), px(1000.)), "HK");
}

#[gpui_kit::test]
fn narrow_window_keeps_long_node_labels_and_keyboard_switch_controls_inside_the_viewport(
    cx: &mut TestAppContext,
) {
    assert_provider_node_keyboard_controls(
        cx,
        size(px(900.), px(700.)),
        "Hong-Kong-Premium-High-Speed-Shadowsocks-Subscription-Node-With-A-Very-Long-English-Display-Name",
    );
}

fn assert_provider_node_keyboard_controls(
    cx: &mut TestAppContext,
    dimensions: gpui_kit::Size<gpui_kit::Pixels>,
    node_label: &'static str,
) {
    cx.foreground_executor().clone().block_test(async {
            use tokio::io::{AsyncWriteExt};

            cx.executor().allow_parking();
            let catalog_json = serde_json::json!({"proxies": {
                "a": {"name": node_label, "provider-name": "Airport A", "history": [{"delay":100}]},
                "b": {"name": node_label, "provider-name": "Airport B", "history": [{"delay":77}]},
                "Proxy": {"type":"Selector", "now":"a", "all":["a","b"], "test-url":"http://127.0.0.1/"}
            }}).to_string();
            let (window, page, runtime) = open_catalog_at(cx, ProxyCatalog::default(), dimensions);
            // These ASCII fixture labels contain only letters and hyphens; Mihomo's
            // client encodes every non-alphanumeric path byte, including '-'.
            let encoded_node_label = node_label.replace('-', "%2D");
            let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let server = runtime.spawn(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let mut selected = String::new();
                let mut measured = Vec::new();
                for _ in 0..5 {
                    let (mut stream, _) = tokio::time::timeout(
                        std::time::Duration::from_secs(5), listener.accept(),
                    ).await.unwrap().unwrap();
                    let (headers, payload) = zenclash_core::test_support::read_controller_request(&mut stream).await.unwrap().into_parts();
let body = if headers.starts_with("GET /proxies ") {
                        catalog_json.to_owned()
                    } else if headers.starts_with("PUT /proxies/Proxy ") {
                        selected = serde_json::from_slice::<serde_json::Value>(&payload).unwrap()["name"]
                            .as_str().unwrap().to_owned();
                        String::new()
                    } else if headers.starts_with("GET /proxies/Proxy ") {
                        serde_json::json!({"now": selected}).to_string()
                    } else {
                        let delay = if headers.starts_with(&format!("GET /providers/proxies/Airport%20A/{encoded_node_label}/healthcheck?")) {
                            measured.push("Airport A");
                            42
                        } else {
                            assert!(headers.starts_with(&format!("GET /providers/proxies/Airport%20B/{encoded_node_label}/healthcheck?")), "{headers}");
                            measured.push("Airport B");
                            77
                        };
                        serde_json::json!({"delay": delay, "meanDelay": delay}).to_string()
                    };
                    let status = if body.is_empty() { "204 No Content" } else { "200 OK" };
                    stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                }
                measured.sort_unstable();
                (selected, measured)
            });
            let client = MihomoClient::new(zenclash_core::MihomoEndpoint::new(
                format!("http://{address}"),
                "",
            ))
            .unwrap();
            let fetch_client = client.clone();
            let catalog = runtime
                .spawn(async move { fetch_client.proxy_catalog().await.unwrap() })
                .await
                .unwrap();
            let id = |action: &'static str, key: &str, provider: &str| {
                let group = gpui_kit::ElementId::from((gpui_kit::ElementId::from(action), "Proxy"));
                let node = gpui_kit::ElementId::from((group, key.to_owned()));
                gpui_kit::ElementId::from((node, provider.to_owned()))
            };
            let (select_b, test_a, test_b) = cx
                .update_window(window, |_, window, cx| {
                    page.update(cx, |page, cx| {
                        page.client = client;
                        let indices = presentation::visible_group_indices(&catalog, "rule", false);
                        page.install_catalog(catalog, "rule".into(), indices);
                        page.selected_group = Some("Proxy".into());
                        cx.notify();
                    });
                    let focus = page.read(cx).focus_handle.clone();
                    window.focus(&focus, cx);
                    window.render_frame(cx);
                    let control = |window: &mut Window, action: &'static str, key, provider| {
                        let expected = id(action, key, provider);
                        if window.try_find(expected.clone()).is_some() {
                            expected
                        } else {
                            // Exercise the previous controls as well: the regression
                            // must fail on the selected/requested node, not a missing ID.
                            let group =
                                gpui_kit::ElementId::from((gpui_kit::ElementId::from(action), "Proxy"));
                            gpui_kit::ElementId::from((group, node_label))
                        }
                    };
                    let select_b = control(window, "select-proxy", "b", "Airport B");
                    let test_a = control(window, "test-proxy", "a", "Airport A");
                    let test_b = control(window, "test-proxy", "b", "Airport B");
                    for target in [&select_b, &test_a, &test_b] {
                        let bounds = window.find(target.clone()).bounds();
                        assert!(bounds.right() <= dimensions.width, "control extends beyond the window: {bounds:?}");
                        assert!(bounds.bottom() <= dimensions.height, "control is vertically clipped: {bounds:?}");
                    }
                    for _ in 0..40 {
                        if window.find(select_b.clone()).focused() == Some(true) {
                            break;
                        }
                        window.press("tab", cx);
                    }
                    assert_eq!(window.find(select_b.clone()).focused(), Some(true));
                    window.press("left", cx);
                    assert_eq!(window.find(id("select-proxy", "a", "Airport A")).focused(), Some(true));
                    window.press("right", cx);
                    assert_eq!(window.find(select_b.clone()).focused(), Some(true));
                    window.press("enter", cx);
                    (select_b, test_a, test_b)
                })
                .unwrap();
            cx.run_until_parked();
            for _ in 0..1000 {
                if cx.update(|cx| page.read(cx).catalog.as_ref().unwrap().groups()[0].now == "b") {
                    break;
                }
                runtime
                    .spawn(async {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    })
                    .await
                    .unwrap();
            }
            cx.update_window(window, |_, window, cx| {
                assert_eq!(page.read(cx).catalog.as_ref().unwrap().groups()[0].now, "b");
                window.render_frame(cx);
                // Selection keeps the card and its keyboard focus stable.
                assert!(window.try_find(select_b).is_some());
                window.find(id("current-proxy", "b", "Airport B"));
                window.click(test_a, cx);
                window.render_frame(cx);
                window.click(test_b, cx);
                let page = page.read(cx);
                let group = &page.catalog.as_ref().unwrap().groups()[0];
                let testing = page.testing.get("Proxy");
                assert!(
                    group
                        .all
                        .iter()
                        .all(|node| testing.is_some_and(|testing| testing.contains(node))),
                    "the same-label controls did not start independent provider measurements"
                );
                assert_eq!(page.active_testing_groups["Proxy"], 2);
            })
            .unwrap();
            let (selected, measured) = server.await.unwrap();
            assert_eq!(selected, "b");
            assert_eq!(measured, ["Airport A", "Airport B"]);
            for _ in 0..1000 {
                if cx.update(|cx| page.read(cx).testing.is_empty()) {
                    break;
                }
                runtime
                    .spawn(async {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    })
                    .await
                    .unwrap();
            }
            cx.update_window(window, |_, window, cx| {
                let page = page.read(cx);
                assert!(page.testing.is_empty());
                let catalog = page.catalog.as_ref().unwrap();
                let delays = catalog.groups()[0]
                    .all
                    .iter()
                    .map(|node| catalog.node(node).unwrap().latest_delay())
                    .collect::<Vec<_>>();
                assert_eq!(delays, [Some(42), Some(77)]);
                window.remove_window();
            })
            .unwrap();
        });
}

#[gpui_kit::test]
fn delay_admission_refreshes_alias_groups_without_invalidating_a_display_named_group(
    cx: &mut TestAppContext,
) {
    let shared = ProxyNode {
        name: "shared".into(),
        history: vec![DelayHistory {
            delay: 100,
            ..Default::default()
        }],
        ..Default::default()
    };
    let faster = ProxyNode {
        name: "faster".into(),
        history: vec![DelayHistory {
            delay: 50,
            ..Default::default()
        }],
        ..Default::default()
    };
    let catalog = ProxyCatalog::from_group_nodes(
        vec![
            (
                ProxyGroup::new("first-key").with_display_name("same label"),
                vec![shared.clone(), faster.clone()],
            ),
            (
                ProxyGroup::new("second-key").with_display_name("same label"),
                vec![shared, faster],
            ),
            (
                ProxyGroup::new("same label"),
                vec![ProxyNode {
                    name: "unrelated".into(),
                    ..Default::default()
                }],
            ),
        ],
        3,
    );
    let (window, page, _runtime) = open_catalog(cx, catalog);
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, _| {
            page.sort_by_latency = true;
            page.hide_unavailable = true;
            let previous = Arc::clone(page.catalog.as_ref().unwrap());
            let shared_id = previous.groups()[0].all[0].clone();
            let prepared = previous
                .groups()
                .iter()
                .map(|group| page.displayed_nodes(&previous, group))
                .collect::<Vec<_>>();
            assert_eq!(prepared[0].as_ref(), &[1, 0]);
            assert_eq!(prepared[1].as_ref(), &[1, 0]);
            page.record_node_delay(&shared_id, 20, 20, None);
            let catalog = page.catalog.as_ref().unwrap();
            for group in &catalog.groups()[..2] {
                assert_eq!(page.displayed_nodes(catalog, group).as_ref(), &[0, 1]);
            }
            assert!(std::rc::Rc::ptr_eq(
                &prepared[2],
                &page.displayed_nodes(catalog, &catalog.groups()[2])
            ));
            assert_eq!(previous.node(&shared_id).unwrap().latest_delay(), Some(100));
            page.record_node_delay(&shared_id, 0, 0, Some(DelayTestFailure::Timeout));
            let catalog = page.catalog.as_ref().unwrap();
            for group in &catalog.groups()[..2] {
                assert_eq!(page.displayed_nodes(catalog, group).as_ref(), &[1]);
            }
            let searched = page
                .catalog_presentation
                .search_index()
                .as_ref()
                .unwrap()
                .orders_cancellable("shared", true, true, || true)
                .unwrap();
            assert!(searched["first-key"].is_empty());
            assert!(searched["second-key"].is_empty());
            page.record_node_delay(&shared_id, 30, 30, None);
            let catalog = page.catalog.as_ref().unwrap();
            for group in &catalog.groups()[..2] {
                assert_eq!(page.displayed_nodes(catalog, group).as_ref(), &[0, 1]);
            }
            let searched = page
                .catalog_presentation
                .search_index()
                .as_ref()
                .unwrap()
                .orders_cancellable("shared", true, true, || true)
                .unwrap();
            assert_eq!(searched["first-key"], [0]);
            assert_eq!(searched["second-key"], [0]);
            assert!(
                !page
                    .catalog_presentation
                    .failures()
                    .contains_key(&shared_id)
            );
            assert!(std::rc::Rc::ptr_eq(
                &prepared[2],
                &page.displayed_nodes(catalog, &catalog.groups()[2])
            ));
        });
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn shared_node_delay_updates_all_groups_without_changing_another_provider(cx: &mut TestAppContext) {
    let shared = ProxyNode {
        name: "HK".into(),
        provider_name: Some("Airport A".into()),
        history: vec![DelayHistory {
            delay: 100,
            ..Default::default()
        }],
        ..Default::default()
    };
    let other = ProxyNode {
        provider_name: Some("Airport B".into()),
        history: vec![DelayHistory {
            delay: 77,
            ..Default::default()
        }],
        ..shared.clone()
    };
    let (window, page, _runtime) = open_catalog(
        cx,
        ProxyCatalog::from_group_nodes(
            vec![
                (ProxyGroup::new("first"), vec![shared.clone()]),
                (ProxyGroup::new("second"), vec![shared]),
                (ProxyGroup::new("other-provider"), vec![other]),
            ],
            2,
        ),
    );
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, _| {
            let previous = Arc::clone(page.catalog.as_ref().unwrap());
            let prepared = previous
                .groups()
                .iter()
                .map(|group| {
                    page.catalog_presentation
                        .order(&previous, group, true, true)
                })
                .collect::<Vec<_>>();
            page.record_delay("first", "HK", 42, 42);
            let delays = page
                .catalog
                .as_ref()
                .unwrap()
                .groups()
                .iter()
                .map(|group| {
                    page.catalog
                        .as_ref()
                        .unwrap()
                        .node(&group.all[0])
                        .unwrap()
                        .latest_delay()
                })
                .collect::<Vec<_>>();
            assert_eq!(
                delays,
                [Some(42), Some(42), Some(77)],
                "delay results diverged across references to the same provider node"
            );
            let catalog = page.catalog.as_ref().unwrap();
            for (index, group) in catalog.groups().iter().enumerate() {
                let current = page.catalog_presentation.order(catalog, group, true, true);
                assert_eq!(std::rc::Rc::ptr_eq(&prepared[index], &current), index == 2);
            }
            let shared_id = catalog.groups()[0].all[0].clone();
            let other_id = catalog.groups()[2].all[0].clone();
            assert_eq!(previous.node(&shared_id).unwrap().latest_delay(), Some(100));
            assert!(std::ptr::eq(
                previous.node(&other_id).unwrap(),
                catalog.node(&other_id).unwrap()
            ));
            page.record_node_delay(&shared_id, 0, 0, Some(DelayTestFailure::Timeout));
            let catalog = page.catalog.as_ref().unwrap();
            let visible = catalog
                .groups()
                .iter()
                .map(|group| page.catalog_presentation.order(catalog, group, true, true))
                .map(|indices| indices.to_vec())
                .collect::<Vec<_>>();
            assert_eq!(visible, [vec![], vec![], vec![0]]);
            assert!(!page.catalog_presentation.failures().contains_key(&other_id));
        });
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn node_details_and_current_node_open_the_same_modal_without_switching(cx: &mut TestAppContext) {
    let (window, page, _runtime) = open_catalog(
        cx,
        ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup::new("group").with_current_member("current"),
                vec![
                    ProxyNode {
                        name: "current".into(),
                        kind: "Trojan".into(),
                        ..Default::default()
                    },
                    ProxyNode {
                        name: "other".into(),
                        kind: "Shadowsocks".into(),
                        ..Default::default()
                    },
                ],
            )],
            2,
        ),
    );
    let current = ProxyNodeId::new("current".into(), None);
    let other = ProxyNodeId::new("other".into(), None);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("proxy-node-inspector").is_none());
        assert!(window.try_find("proxy-details").is_none());
        assert!(
            window
                .try_find(view::proxy_element_id("proxy-menu", "group", &other))
                .is_none()
        );
        assert!(
            window
                .find(view::proxy_element_id("test-proxy", "group", &other))
                .visible()
        );
    })
    .unwrap();
    for (trigger, shown) in [
        (
            view::proxy_element_id("proxy-node-details", "group", &other),
            other,
        ),
        (
            view::proxy_element_id("select-proxy", "group", &current),
            current,
        ),
    ] {
        cx.update_window(window, |_, window, cx| {
            window.click(trigger.clone(), cx);
            window.render_frame(cx);
            assert!(window.find("proxy-node-inspector").visible());
            window.find(view::proxy_element_id(
                "inspector-node-name",
                "group",
                &shown,
            ));
            assert_eq!(
                page.read(cx).catalog.as_ref().unwrap().groups()[0].now,
                "current"
            );
            assert!(!page.read(cx).operation_pending());
            window.press("escape", cx);
            window.render_frame(cx);
            assert!(window.try_find("proxy-node-inspector").is_none());
        })
        .unwrap();
        // Dialog restores focus after its exit animation completes.
        cx.run_until_parked();
        cx.background_executor
            .advance_clock(std::time::Duration::from_millis(500));
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find(trigger.clone()).focused(),
                Some(true),
                "{trigger:?}"
            );
        })
        .unwrap();
    }
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn node_sort_menu_cancels_and_applies_keyboard_selection(cx: &mut TestAppContext) {
    let (window, page, _runtime) = open_catalog(
        cx,
        ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup::new("group"),
                [("z-node", 20), ("a-node", 10), ("m-node", 30)]
                    .into_iter()
                    .map(|(name, delay)| ProxyNode {
                        name: name.into(),
                        history: vec![DelayHistory {
                            delay,
                            ..Default::default()
                        }],
                        ..Default::default()
                    })
                    .collect(),
            )],
            3,
        ),
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("sort-proxies-by-latency", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.press("escape", cx);
        assert!(!page.read(cx).sort_by_latency);
        let state = page.read(cx);
        let catalog = state.catalog.as_ref().unwrap();
        assert_eq!(
            &*state.displayed_nodes(catalog, &catalog.groups()[0]),
            &[0, 1, 2]
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("sort-proxies-by-latency", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.press("down", cx);
        window.press("down", cx);
        window.press("enter", cx);
        assert!(page.read(cx).sort_by_latency);
        let state = page.read(cx);
        let catalog = state.catalog.as_ref().unwrap();
        assert_eq!(
            &*state.displayed_nodes(catalog, &catalog.groups()[0]),
            &[1, 0, 2]
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("sort-proxies-by-latency", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.press("down", cx);
        window.press("enter", cx);
        let state = page.read(cx);
        assert!(!state.sort_by_latency);
        let catalog = state.catalog.as_ref().unwrap();
        assert_eq!(
            &*state.displayed_nodes(catalog, &catalog.groups()[0]),
            &[0, 1, 2]
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn all_node_members_are_available_without_pagination(cx: &mut TestAppContext) {
    let (window, page, _runtime) = open_catalog(
        cx,
        ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup::new("group").with_behavior(ProxyGroupBehavior::Selector),
                (0..27)
                    .map(|index| ProxyNode {
                        name: format!("node-{index}"),
                        ..Default::default()
                    })
                    .collect(),
            )],
            27,
        ),
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(page.read(cx).node_focus.len(), 27);
        for index in 0..27 {
            let id = ProxyNodeId::new(format!("node-{index}"), None);
            window.find(super::view::proxy_element_id("select-proxy", "group", &id));
        }
        assert!(window.try_find("proxy-node-pagination").is_none());
        window.remove_window();
    })
    .unwrap();
}

fn assert_delayed_catalog_mode(
    cx: &mut TestAppContext,
    controller_mode: &str,
    subsequent_modes: &[&str],
    expected_mode: &str,
    expected_group: &str,
) {
    cx.foreground_executor().clone().block_test(async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            cx.executor().allow_parking();
            let (window, page, runtime) = open_catalog(cx, ProxyCatalog::default());
            let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let (requested, request_received) = tokio::sync::oneshot::channel();
            let (release, response_released) = tokio::sync::oneshot::channel();
            let config = serde_json::json!({"mode": controller_mode}).to_string();
            let server = runtime.spawn(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let mut requested = Some(requested);
                let mut response_released = Some(response_released);
                let mut replies = tokio::task::JoinSet::new();
                for _ in 0..2 {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut request = Vec::new();
                    loop {
                        let mut chunk = [0_u8; 1024];
                        let read = stream.read(&mut chunk).await.unwrap();
                        assert!(read > 0, "controller request closed before its headers");
                        request.extend_from_slice(&chunk[..read]);
                        if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                            break;
                        }
                        assert!(request.len() <= 8192, "unexpected controller request size");
                    }
                    let request = String::from_utf8(request).unwrap();
                    let (body, ready, wait) = if request.starts_with("GET /configs ") {
                        (config.clone(), requested.take(), response_released.take())
                    } else {
                        assert!(request.starts_with("GET /proxies "), "{request}");
                        (r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Proxy":{"name":"Proxy","type":"Selector","now":"DIRECT","all":["DIRECT"]},"GLOBAL":{"name":"GLOBAL","type":"Selector","now":"DIRECT","all":["DIRECT"]}}}"#.into(), None, None)
                    };
                    replies.spawn(async move {
                        if let Some(ready) = ready {
                            ready.send(()).unwrap();
                            wait.unwrap().await.unwrap();
                        }
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        stream.write_all(response.as_bytes()).await.unwrap();
                    });
                }
                while let Some(result) = replies.join_next().await {
                    result.unwrap();
                }
            });
            cx.update_window(window, |_, _, cx| {
                page.update(cx, |page, cx| {
                    page.client = MihomoClient::new(zenclash_core::MihomoEndpoint::new(
                        format!("http://{address}"),
                        "",
                    ))
                    .unwrap();
                    page.reload(cx);
                });
            })
            .unwrap();
            runtime
                .spawn(async move {
                    tokio::time::timeout(std::time::Duration::from_secs(5), request_received)
                        .await
                        .unwrap()
                        .unwrap();
                })
                .await
                .unwrap();
            cx.update_window(window, |_, _, cx| {
                page.update(cx, |page, cx| {
                    assert!(page.loading);
                    for mode in subsequent_modes {
                        page.set_outbound_mode(mode, cx);
                    }
                });
            })
            .unwrap();
            release.send(()).unwrap();
            server.await.unwrap();
            for _ in 0..1000 {
                if cx.update(|cx| !page.read(cx).loading) {
                    break;
                }
                runtime
                    .spawn(async {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    })
                    .await
                    .unwrap();
            }
            cx.update_window(window, |_, window, cx| {
                assert!(
                    !page.read(cx).loading,
                    "delayed controller response was not committed"
                );
                assert_eq!(page.read(cx).error, None);
                assert_eq!(page.read(cx).outbound_mode, expected_mode);
                window.render_frame(cx);
                for group in ["GLOBAL", "Proxy"] {
                    assert_eq!(
                        window
                            .try_find((gpui_kit::ElementId::from("toggle-group"), group))
                            .is_some(),
                        group == expected_group,
                        "wrong group presentation after controller response"
                    );
                }
                window.remove_window();
            })
            .unwrap();
        });
}

#[gpui_kit::test]
fn delayed_catalog_response_discovers_controller_mode_without_new_input(cx: &mut TestAppContext) {
    assert_delayed_catalog_mode(cx, "global", &[], "global", "GLOBAL");
}

#[gpui_kit::test]
fn delayed_catalog_response_preserves_a_newer_mode(cx: &mut TestAppContext) {
    assert_delayed_catalog_mode(cx, "rule", &["global"], "global", "GLOBAL");
}

#[gpui_kit::test]
fn delayed_catalog_response_preserves_same_mode_reaffirmation(cx: &mut TestAppContext) {
    assert_delayed_catalog_mode(cx, "global", &["rule", "RULE"], "rule", "Proxy");
}

#[gpui_kit::test]
fn delayed_catalog_response_preserves_mode_after_aba_changes(cx: &mut TestAppContext) {
    assert_delayed_catalog_mode(cx, "global", &["global", "rule"], "rule", "Proxy");
}

#[gpui_kit::test]
fn every_proxy_group_is_available_without_pagination(cx: &mut TestAppContext) {
    let (window, page, _runtime) = open_catalog_at(
        cx,
        ProxyCatalog::from_group_nodes(
            (0..17)
                .map(|index| (ProxyGroup::new(format!("group-{index}")), Vec::new()))
                .collect(),
            17,
        ),
        size(px(1400.), px(1600.)),
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        for index in 0..17 {
            assert!(
                window
                    .find((
                        gpui_kit::ElementId::from("toggle-group"),
                        format!("group-{index}")
                    ))
                    .visible()
            );
        }
        assert!(window.try_find("next-proxy-group-page").is_none());
        assert!(window.try_find("previous-proxy-group-page").is_none());
        assert!(window.try_find("proxies-show-hidden").is_none());
        window.click((gpui_kit::ElementId::from("toggle-group"), "group-16"), cx);
        window.render_frame(cx);
        assert!(page.read(cx).selected_group.as_deref() == Some("group-16"));
        window.find((gpui_kit::ElementId::from("test-group"), "group-16"));
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn hidden_groups_remain_hidden_without_a_visibility_toggle(cx: &mut TestAppContext) {
    let (window, page, _runtime) = open_catalog(
        cx,
        ProxyCatalog::from_group_nodes(
            vec![(ProxyGroup::new("hidden").with_hidden(true), Vec::new())],
            1,
        ),
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(page.read(cx).visible_group_indices.is_empty());
        assert!(window.try_find("proxies-show-hidden").is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn replacing_and_filtering_group_catalogs_preserves_mode_and_suspend_releases_them(
    cx: &mut TestAppContext,
) {
    let (window, page, _runtime) = open_catalog(
        cx,
        ProxyCatalog::from_group_nodes(
            (0..17)
                .map(|index| (ProxyGroup::new(format!("old-{index}")), Vec::new()))
                .collect(),
            17,
        ),
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find((gpui_kit::ElementId::from("toggle-group"), "old-16"));
        page.update(cx, |page, cx| {
            let catalog = ProxyCatalog::from_group_nodes(
                vec![
                    (
                        ProxyGroup::new("replacement").with_current_member("HK"),
                        Vec::new(),
                    ),
                    (ProxyGroup::new("hidden").with_hidden(true), Vec::new()),
                    (ProxyGroup::new("GLOBAL"), Vec::new()),
                ],
                3,
            );
            let indices = presentation::visible_group_indices(&catalog, "rule", false);
            page.install_catalog(catalog, "rule".into(), indices);
            assert_eq!(page.catalog.as_ref().unwrap().groups()[0].now, "HK");
            cx.notify();
        });
        window.render_frame(cx);
        window.find((gpui_kit::ElementId::from("toggle-group"), "replacement"));
        assert!(
            window
                .try_find((gpui_kit::ElementId::from("toggle-group"), "old-16"))
                .is_none()
        );
        assert!(
            window
                .try_find((gpui_kit::ElementId::from("toggle-group"), "hidden"))
                .is_none()
        );
        assert!(window.try_find("next-proxy-group-page").is_none());

        page.update(cx, |page, cx| page.set_outbound_mode("GLOBAL", cx));
        window.render_frame(cx);
        window.find((gpui_kit::ElementId::from("toggle-group"), "GLOBAL"));
        assert!(
            window
                .try_find((gpui_kit::ElementId::from("toggle-group"), "replacement"))
                .is_none()
        );
        assert!(
            window
                .try_find((gpui_kit::ElementId::from("toggle-group"), "hidden"))
                .is_none()
        );
        page.update(cx, |page, cx| page.set_outbound_mode("direct", cx));
        window.render_frame(cx);
        assert!(
            window
                .try_find((gpui_kit::ElementId::from("toggle-group"), "GLOBAL"))
                .is_none()
        );
        assert!(page.read(cx).visible_group_indices.is_empty());

        page.update(cx, |page, cx| {
            page.set_outbound_mode("rule", cx);
            assert_eq!(page.visible_group_indices, [0]);
            page.suspend();
            assert!(page.catalog.is_none());
            assert!(page.visible_group_indices.is_empty());
            cx.notify();
        });
        window.render_frame(cx);
        assert!(
            window
                .try_find((gpui_kit::ElementId::from("toggle-group"), "replacement"))
                .is_none()
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn proxy_group_headers_remain_testing_until_each_groups_last_result(cx: &mut TestAppContext) {
    let (window, page, _runtime) = open_catalog(
        cx,
        ProxyCatalog::from_group_nodes(
            ["Proxy", "Proxy Auto"]
                .into_iter()
                .map(|name| {
                    (
                        ProxyGroup::new(name),
                        ["HK", "JP"]
                            .into_iter()
                            .map(|name| ProxyNode {
                                name: name.into(),
                                ..Default::default()
                            })
                            .collect(),
                    )
                })
                .collect(),
            4,
        ),
    );
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            assert!(page.start_proxy_test("Proxy", "HK").is_some());
            assert!(page.start_proxy_test("Proxy", "HK").is_none());
            let group = &page.catalog.as_ref().unwrap().groups()[0];
            assert_eq!(
                take_untested_group_proxies(
                    &mut page.testing,
                    &mut page.active_testing_groups,
                    page.catalog.as_deref().unwrap(),
                    group
                )
                .len(),
                1
            );
            assert!(
                take_untested_group_proxies(
                    &mut page.testing,
                    &mut page.active_testing_groups,
                    page.catalog.as_deref().unwrap(),
                    group
                )
                .is_empty()
            );
            page.start_proxy_test("Proxy Auto", "HK").unwrap();
            cx.notify();
        });
        window.render_frame(cx);
        for group in ["Proxy", "Proxy Auto"] {
            window.click((gpui_kit::ElementId::from("toggle-group"), group), cx);
            assert_eq!(
                window
                    .find((gpui_kit::ElementId::from("test-group"), group))
                    .label(),
                Some(zenclash_i18n::text("proxies.actions.testing").as_str())
            );
        }
        for (group, node, expected) in [
            ("Proxy", "HK", "proxies.actions.testing"),
            ("Proxy", "JP", "proxies.actions.test_selected_group"),
            ("Proxy Auto", "HK", "proxies.actions.test_selected_group"),
        ] {
            page.update(cx, |page, cx| {
                let token = DelayTaskToken(page.delay_generation);
                assert!(page.finish_proxy_test(token, group, &test_key(group, node)));
                assert!(page.finish_proxy_test(token, group, &test_key(group, node)));
                cx.notify();
            });
            window.render_frame(cx);
            window.click((gpui_kit::ElementId::from("toggle-group"), group), cx);
            assert_eq!(
                window
                    .find((gpui_kit::ElementId::from("test-group"), group))
                    .label(),
                Some(zenclash_i18n::text(expected).as_str())
            );
            if group == "Proxy" {
                window.click(
                    (gpui_kit::ElementId::from("toggle-group"), "Proxy Auto"),
                    cx,
                );
                assert_eq!(
                    window
                        .find((gpui_kit::ElementId::from("test-group"), "Proxy Auto"))
                        .label(),
                    Some(zenclash_i18n::text("proxies.actions.testing").as_str())
                );
            }
        }
        page.update(cx, |page, cx| {
            let stale = DelayTaskToken(page.delay_generation);
            let key = page.start_proxy_test("Proxy", "HK").unwrap();
            let catalog = page.catalog.take().unwrap();
            page.suspend();
            let indices = presentation::visible_group_indices(&catalog, "rule", false);
            page.install_catalog(Arc::unwrap_or_clone(catalog), "rule".into(), indices);
            page.start_proxy_test("Proxy", "HK").unwrap();
            assert!(!page.finish_proxy_test(stale, "Proxy", &key));
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(
            window
                .find((gpui_kit::ElementId::from("test-group"), "Proxy"))
                .label(),
            Some(zenclash_i18n::text("proxies.actions.testing").as_str())
        );
        page.update(cx, |page, cx| {
            assert!(page.finish_proxy_test(
                DelayTaskToken(page.delay_generation),
                "Proxy",
                &test_key("Proxy", "HK")
            ));
            page.start_proxy_test("Proxy", "JP").unwrap();
            let stale = DelayTaskToken(page.delay_generation);
            page.reload(cx);
            assert!(!page.finish_proxy_test(stale, "Proxy", &test_key("Proxy", "JP")));
        });
        window.render_frame(cx);
        assert_eq!(
            window
                .find((gpui_kit::ElementId::from("test-group"), "Proxy"))
                .label(),
            Some(zenclash_i18n::text("proxies.actions.test_selected_group").as_str())
        );
        page.update(cx, |page, _| page.suspend());
        window.remove_window();
    })
    .unwrap();
}

#[test]
fn group_test_selects_only_nodes_without_an_inflight_test() {
    let catalog = ProxyCatalog::from_group_nodes(
        vec![(
            ProxyGroup::new("Proxy"),
            ["HK", "US"]
                .into_iter()
                .map(|name| ProxyNode {
                    name: name.into(),
                    ..Default::default()
                })
                .collect(),
        )],
        2,
    );
    let group = &catalog.groups()[0];
    let mut testing = HashMap::from([("Proxy".into(), HashSet::from([group.all[0].clone()]))]);
    let selected = take_untested_proxies(&mut testing, &catalog, group);
    assert_eq!(
        selected
            .iter()
            .map(|(_, proxy)| proxy.name.as_str())
            .collect::<Vec<_>>(),
        vec!["US"]
    );
    assert_eq!(selected[0].0, group.all[1]);
}

#[test]
fn local_delay_history_discards_oldest_samples() {
    let mut proxy = ProxyNode::default();
    let sample_count = u32::try_from(MAX_LOCAL_DELAY_HISTORY + 1).expect("small test limit");
    for delay in 1..=sample_count {
        append_delay(&mut proxy, delay, delay);
    }

    assert_eq!(proxy.history.len(), MAX_LOCAL_DELAY_HISTORY);
    assert_eq!(proxy.history.first().map(|sample| sample.delay), Some(2));
    assert_eq!(proxy.latest_delay(), Some(21));
}

#[test]
fn load_balance_group_has_no_manual_selection_or_unique_current() {
    assert!(!group_allows_manual_selection(
        &ProxyGroupBehavior::LoadBalance
    ));
    assert!(!group_has_unique_current(&ProxyGroupBehavior::LoadBalance));
}

#[test]
fn selector_and_automatic_groups_allow_manual_selection() {
    assert!(group_allows_manual_selection(&ProxyGroupBehavior::Selector));
    assert!(group_allows_manual_selection(
        &ProxyGroupBehavior::Automatic { fixed: false }
    ));
}

#[test]
fn catalog_task_token_rejects_results_from_an_older_profile_catalog() {
    let token = CatalogTaskToken(4);

    assert!(!token.is_current(5));
}

#[test]
fn catalog_task_token_accepts_the_current_catalog_generation() {
    let token = CatalogTaskToken(7);

    assert!(token.is_current(7));
}

#[test]
fn proxy_selection_state_tracks_different_groups_independently() {
    let mut state = ProxySelectionUiState::default();

    let proxy_request = state.start("Proxy".into(), "HK".into()).unwrap();
    state.start("Streaming".into(), "US".into()).unwrap();
    state.reject(&proxy_request);

    assert!(!state.group_pending("Proxy"));
    assert!(state.group_pending("Streaming"));
}

#[test]
fn stale_proxy_selection_cannot_complete_a_newer_request() {
    let mut state = ProxySelectionUiState::default();

    let stale = state.start("Proxy".into(), "HK".into()).unwrap();
    state.clear();
    state.start("Proxy".into(), "US".into()).unwrap();

    assert!(!state.reject(&stale));
    assert!(state.proxy_pending("Proxy", "US"));
}

#[test]
fn independent_group_requests_can_both_finish_after_acknowledgement() {
    let mut state = ProxySelectionUiState::default();
    let first = state.start("first".into(), "HK".into()).unwrap();
    assert!(state.acknowledge(&first));
    let second = state.start("second".into(), "US".into()).unwrap();
    assert!(state.is_current(&first));
    assert!(state.is_current(&second));
}

#[test]
fn acknowledged_request_does_not_change_the_last_observed_catalog_member() {
    let catalog = ProxyCatalog::from_group_nodes(
        vec![(
            ProxyGroup::new("Proxy").with_current_member("HK"),
            Vec::new(),
        )],
        0,
    );
    let mut state = ProxySelectionUiState::default();
    let request = state.start("Proxy".into(), "US".into()).unwrap();
    assert!(state.acknowledge(&request));
    assert_eq!(catalog.groups()[0].now, "HK");
    assert!(!state.group_pending("Proxy"));
    assert!(state.status_text("Proxy").is_some());
}

#[test]
fn delay_failures_distinguish_timeout_from_transport_failure() {
    assert_eq!(
        DelayTestFailure::from_error("Mihomo API returned HTTP 504: Timeout"),
        DelayTestFailure::Timeout
    );
    assert_eq!(
        DelayTestFailure::from_error("Mihomo API returned HTTP 503: transport error"),
        DelayTestFailure::Failed
    );
}

#[test]
fn expanding_another_group_collapses_the_previous_group() {
    let mut selected_group = Some("Proxy".to_owned());

    toggle_group_selection(&mut selected_group, "Streaming");

    assert_eq!(selected_group.as_deref(), Some("Streaming"));
}

#[test]
fn group_testing_state_matches_the_complete_group_name() {
    let mut testing = HashMap::new();
    let mut groups = HashMap::new();
    insert_inflight_test(
        &mut testing,
        &mut groups,
        "Proxy Auto",
        test_key("Proxy Auto", "HK"),
    );
    assert!(groups.contains_key("Proxy Auto"));
    assert!(!groups.contains_key("Proxy"));
}

#[test]
fn group_stays_testing_until_its_last_measurement_finishes() {
    let mut testing = HashMap::new();
    let mut groups = HashMap::new();
    for (group, node) in [("Proxy", "HK"), ("Proxy", "JP"), ("Auto", "HK")] {
        insert_inflight_test(&mut testing, &mut groups, group, test_key(group, node));
    }
    remove_inflight_test(&mut testing, &mut groups, "Proxy", &test_key("Proxy", "HK"));
    remove_inflight_test(&mut testing, &mut groups, "Proxy", &test_key("Proxy", "HK"));
    assert_eq!(
        groups.keys().map(String::as_str).collect::<HashSet<_>>(),
        HashSet::from(["Proxy", "Auto"])
    );
    remove_inflight_test(&mut testing, &mut groups, "Proxy", &test_key("Proxy", "JP"));
    assert_eq!(
        groups.keys().map(String::as_str).collect::<HashSet<_>>(),
        HashSet::from(["Auto"])
    );
    remove_inflight_test(&mut testing, &mut groups, "Auto", &test_key("Auto", "HK"));
    assert!(groups.is_empty());
    assert!(testing.is_empty());
}

#[gpui_kit::test]
fn proxy_filter_geometry_and_keyboard_menu_scale_with_the_window_rem(cx: &mut TestAppContext) {
    let (window, page, _runtime) = open_catalog(
        cx,
        ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup::new("zoom group").with_current_member("Hong Kong 节点"),
                vec![ProxyNode {
                    name: "Hong Kong 节点".into(),
                    ..Default::default()
                }],
            )],
            0,
        ),
    );
    for font in [12., 16., 20.] {
        cx.update(|cx| gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(font)));
        for dimensions in [size(px(720.), px(560.)), size(px(1280.), px(820.))] {
            cx.simulate_window_resize(window, dimensions);
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(window.rem_size(), px(font));
                let filter = window.find("sort-proxies-by-latency").bounds();
                assert!((f32::from(filter.size.height) - font * 2.75).abs() <= 1.);
                assert!(filter.left() >= px(0.) && filter.right() <= window.viewport_size().width);
                assert!(window.find("sort-proxies-by-latency").visible());
                let focus = page.read(cx).focus_handle.clone();
                window.focus(&focus, cx);
                window.click("sort-proxies-by-latency", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.press("down", cx);
                window.press("down", cx);
                window.press("enter", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                assert!(page.read(cx).sort_by_latency);
                assert!(page.read(cx).focus_handle.is_focused(window));
                window.click("sort-proxies-by-latency", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.press("escape", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                assert!(page.read(cx).sort_by_latency);
                assert!(page.read(cx).focus_handle.is_focused(window));
            })
            .unwrap();
        }
    }
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}
