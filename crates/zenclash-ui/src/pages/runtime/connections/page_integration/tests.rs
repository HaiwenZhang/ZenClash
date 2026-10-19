use super::*;
use crate::pages::runtime::test_support::{RuntimePageFixture, open_runtime_page_window};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::AsyncWriteExt;

struct Controller {
    endpoint: zenclash_core::MihomoEndpoint,
    readback_started: Arc<AtomicBool>,
    release: Arc<tokio::sync::Notify>,
    deletes: Arc<parking_lot::Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Controller {
    fn new(runtime: &tokio::runtime::Runtime, delayed_readback: bool) -> Self {
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let endpoint = zenclash_core::MihomoEndpoint::new(
            format!("http://{}", listener.local_addr().unwrap()),
            "",
        );
        let readback_started = Arc::new(AtomicBool::new(false));
        let release = Arc::new(tokio::sync::Notify::new());
        let deletes = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let (started, gate, recorded) =
            (readback_started.clone(), release.clone(), deletes.clone());
        let task = runtime.spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let Ok(request) = zenclash_core::test_support::read_controller_request(&mut stream).await else { continue; };
                let (headers, _) = request.into_parts();
                let first = headers.lines().next().unwrap();
                let response = if first.starts_with("DELETE /connections") {
                    recorded.lock().push(first.to_owned());
                    "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned()
                } else if first.starts_with("GET /connections ") {
                    started.store(true, Ordering::Release);
                    if delayed_readback { gate.notified().await; }
                    let body = r#"{"connections":[{"id":"readback","metadata":{},"upload":0,"download":0}],"uploadTotal":0,"downloadTotal":0,"memory":0}"#;
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                } else {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
                };
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        Self {
            endpoint,
            readback_started,
            release,
            deletes,
            task,
        }
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn snapshot(id: &str) -> PageSnapshotData {
    PageSnapshotData::Connections(Arc::new(zenclash_core::ConnectionsSnapshot {
        connections: vec![zenclash_core::Connection {
            id: id.into(),
            ..Default::default()
        }],
        ..Default::default()
    }))
}

#[gpui_kit::test]
fn connection_close_reads_its_original_runtime_and_resumes_the_display(
    cx: &mut gpui_kit::TestAppContext,
) {
    for id in [None, Some("one".to_owned())] {
        let fixture = RuntimePageFixture::new();
        let (window, page) = open_runtime_page_window(cx, &fixture, Page::Connections);
        fixture.wait_for_initial_reads(cx, &page);
        let runtime = fixture.runtime.as_ref().unwrap();
        let controller = Controller::new(runtime, false);
        runtime
            .block_on(fixture.core.switch_to_direct(controller.endpoint.clone()))
            .unwrap();
        let source = runtime.block_on(fixture.core.begin_observation()).unwrap();
        cx.update(|cx| {
            page.update(cx, |page, cx| {
                page.invalidate_page_load();
                page.page_observation
                    .adopt(snapshot("one"), source.identity().clone());
                page.update_connection_presentation(cx);
                page.connections
                    .update(cx, |view, cx| view.set_close_allowed(true, cx));
                page.close_connections(id.clone(), cx);
            })
        });
        fixture.settle(cx, &page, |page| matches!(page.page_observation.data(), PageSnapshotData::Connections(data) if data.connections[0].id == "readback"));
        assert_eq!(
            *controller.deletes.lock(),
            [id.as_ref()
                .map_or("DELETE /connections HTTP/1.1".to_owned(), |id| format!(
                    "DELETE /connections/{id} HTTP/1.1"
                ))]
        );
        cx.update(|cx| {
            let page = page.read(cx);
            assert!(!page.core_busy());
            assert!(
                page.page_observation
                    .source()
                    .as_ref()
                    .unwrap()
                    .belongs_to(&fixture.core)
            );
        });
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
    }
}

#[gpui_kit::test]
fn old_connection_close_readback_cannot_overwrite_a_new_query_or_runtime(
    cx: &mut gpui_kit::TestAppContext,
) {
    for replace_runtime in [false, true] {
        let fixture = RuntimePageFixture::new();
        let (window, page) = open_runtime_page_window(cx, &fixture, Page::Connections);
        fixture.wait_for_initial_reads(cx, &page);
        let runtime = fixture.runtime.as_ref().unwrap();
        let controller = Controller::new(runtime, true);
        runtime
            .block_on(fixture.core.switch_to_direct(controller.endpoint.clone()))
            .unwrap();
        let source = runtime.block_on(fixture.core.begin_observation()).unwrap();
        cx.update(|cx| {
            page.update(cx, |page, cx| {
                page.invalidate_page_load();
                page.page_observation
                    .adopt(snapshot("one"), source.identity().clone());
                page.close_connections(None, cx);
            })
        });
        fixture.settle(cx, &page, |_| {
            controller.readback_started.load(Ordering::Acquire)
        });
        if replace_runtime {
            runtime
                .block_on(
                    fixture
                        .core
                        .switch_to_direct(zenclash_core::MihomoEndpoint::new(
                            "http://127.0.0.1:1",
                            "",
                        )),
                )
                .unwrap();
        }
        let newer = runtime.block_on(fixture.core.begin_observation()).unwrap();
        cx.update(|cx| {
            page.update(cx, |page, _| {
                page.page_observation
                    .begin(Page::Connections, page.navigation_generation);
                page.page_observation
                    .adopt(snapshot("newer"), newer.identity().clone());
                page.notice = Some("new notice".into());
                page.error = Some("new error".into());
            })
        });
        controller.release.notify_one();
        fixture.settle(cx, &page, |page| !page.core_busy());
        cx.update(|cx| {
            let page = page.read(cx);
            assert!(matches!(page.page_observation.data(), PageSnapshotData::Connections(data) if data.connections[0].id == "newer"));
            assert_eq!(page.notice.as_deref(), Some("new notice"));
            assert_eq!(page.error.as_deref(), Some("new error"));
        });
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
    }
}
