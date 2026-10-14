//! Owned loopback controller and isolated store for proxy configuration observations.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::io::AsyncWriteExt;
use zenclash_core::{
    ControlledConfigStore, CoreKind, CoreSession, MihomoClient, MihomoEndpoint, ProxyGroupBehavior,
};

enum ProxyResponsePause {
    WorkspaceCatalog,
    SelectionReadback,
    SelectionAcknowledgement,
    GroupDelay,
}

impl ProxyResponsePause {
    fn matches(&self, request: &str) -> bool {
        match self {
            Self::WorkspaceCatalog => request.starts_with("GET /proxies "),
            Self::SelectionReadback => request.starts_with("GET /proxies/group "),
            Self::SelectionAcknowledgement => request.starts_with("PUT /proxies/group "),
            Self::GroupDelay => request.starts_with("GET /group/group/delay?"),
        }
    }
}

pub(super) struct ProxyConfigurationFixture {
    pub(super) runtime: tokio::runtime::Runtime,
    pub(super) session: CoreSession,
    pub(super) store: ControlledConfigStore,
    pub(super) paused_request_started: Option<tokio::sync::oneshot::Receiver<()>>,
    pub(super) release_response: Option<tokio::sync::oneshot::Sender<()>>,
    fail_selection_readback: Arc<AtomicBool>,
    fail_workspace_reads: Arc<AtomicBool>,
    server: tokio::task::JoinHandle<()>,
    root: PathBuf,
}

impl ProxyConfigurationFixture {
    pub(super) fn fail_workspace_reads(&self) {
        self.fail_workspace_reads.store(true, Ordering::Release);
    }

    pub(super) fn fail_selection_readback(&self) {
        self.fail_selection_readback.store(true, Ordering::Release);
    }

    pub(super) fn new() -> Self {
        Self::with_response_pause(None)
    }

    pub(super) fn with_first_catalog_paused() -> Self {
        Self::with_response_pause(Some(ProxyResponsePause::WorkspaceCatalog))
    }

    pub(super) fn with_first_selection_readback_paused() -> Self {
        Self::with_response_pause(Some(ProxyResponsePause::SelectionReadback))
    }

    pub(super) fn with_first_selection_acknowledgement_paused() -> Self {
        Self::with_response_pause(Some(ProxyResponsePause::SelectionAcknowledgement))
    }

    pub(super) fn automatic_group() -> Self {
        Self::with_controller_state(None, ProxyGroupBehavior::Automatic { fixed: true })
    }

    pub(super) fn with_group_delay_paused() -> Self {
        Self::with_controller_state(
            Some(ProxyResponsePause::GroupDelay),
            ProxyGroupBehavior::Automatic { fixed: true },
        )
    }

    fn with_response_pause(pause: Option<ProxyResponsePause>) -> Self {
        Self::with_controller_state(pause, ProxyGroupBehavior::Selector)
    }

    fn with_controller_state(
        pause: Option<ProxyResponsePause>,
        mut behavior: ProxyGroupBehavior,
    ) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "zenclash-proxy-config-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint =
            MihomoEndpoint::new(format!("http://{}", listener.local_addr().unwrap()), "");
        listener.set_nonblocking(true).unwrap();
        let (request_entered, paused_request_started) = tokio::sync::oneshot::channel();
        let (release_response, response_released) = tokio::sync::oneshot::channel();
        let fail_workspace_reads = Arc::new(AtomicBool::new(false));
        let fail_workspace = fail_workspace_reads.clone();
        let fail_selection_readback = Arc::new(AtomicBool::new(false));
        let fail_readback = fail_selection_readback.clone();
        let server = runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            let mut mode = "rule".to_owned();
            let mut group_member = if matches!(behavior, ProxyGroupBehavior::Automatic { .. }) { "Requested" } else { "node" };
            let mut response_pause = pause.map(|pause| (pause, request_entered, response_released));
            let mut replies = tokio::task::JoinSet::new();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let receipt = match zenclash_core::test_support::read_controller_request(&mut stream).await {
                    Ok(receipt) => receipt,
                    Err(error) if zenclash_core::test_support::is_controller_header_eof(&error) => continue,
                    Err(error) => panic!("proxy fixture request: {error}"),
                };
                let (headers, payload) = receipt.into_parts();
                let request = headers.lines().next().unwrap().to_owned();
                if fail_workspace.load(Ordering::Acquire) && request.starts_with("GET /proxies ") {
                    let _ = stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                    continue;
                }
                if fail_readback.load(Ordering::Acquire) && request.starts_with("GET /proxies/group ") {
                    let _ = stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                    continue;
                }
                let body = if request.starts_with("PATCH /configs ") {
                    let payload: serde_json::Value = serde_json::from_slice(&payload).unwrap();
                    mode = payload["mode"].as_str().unwrap().to_owned();
                    None
                } else if request.starts_with("GET /configs ") {
                    Some(serde_json::json!({"mode":mode,"mixed-port":7890}).to_string())
                } else if request.starts_with("GET /proxies ") {
                    let (kind, fixed) = match behavior {
                        ProxyGroupBehavior::Automatic { fixed } => ("URLTest", fixed),
                        _ => ("Selector", false),
                    };
                    Some(serde_json::json!({"proxies":{
                        "node":{"type":"Shadowsocks"},"Requested":{"type":"Shadowsocks"},"Actual":{"type":"Shadowsocks"},
                        "group":{"type":kind,"fixed":fixed,"now":group_member,"all":["node","Requested","Actual"]},
                        "other":{"type":"Selector","now":"node","all":["node","Requested","Actual"]}
                    }}).to_string())
                } else if request.starts_with("DELETE /proxies/group ") {
                    behavior = ProxyGroupBehavior::Automatic { fixed: false };
                    group_member = "Actual";
                    None
                } else if request.starts_with("GET /group/group/delay?") {
                    behavior = ProxyGroupBehavior::Automatic { fixed: false };
                    group_member = "Actual";
                    Some(r#"{"node":42,"Actual":24}"#.to_owned())
                } else if request.starts_with("PUT /proxies/group ") || request.starts_with("PUT /proxies/other ") {
                    assert_eq!(serde_json::from_slice::<serde_json::Value>(&payload).unwrap(), serde_json::json!({"name":"Requested"}));
                    None
                } else if request.starts_with("GET /proxies/group ") || request.starts_with("GET /proxies/other ") {
                    Some(r#"{"now":"Actual","type":"Selector"}"#.to_owned())
                } else if request.starts_with("GET /proxies/node/delay?") {
                    Some(r#"{"delay":42}"#.to_owned())
                } else { panic!("unexpected proxy fixture request: {request}"); };
                let response = body.map_or_else(
                    || "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned(),
                    |body| format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()),
                );
                if response_pause.as_ref().is_some_and(|(pause, _, _)| pause.matches(&request))
                    && let Some((_, entered, released)) = response_pause.take()
                {
                    entered.send(()).unwrap();
                    replies.spawn(async move {
                        if released.await.is_ok() {
                            let _ = stream.write_all(response.as_bytes()).await;
                        }
                    });
                } else {
                    let _ = stream.write_all(response.as_bytes()).await;
                }
            }
        });
        let session =
            CoreSession::open(CoreKind::Mihomo, MihomoClient::new(endpoint).unwrap()).unwrap();
        Self {
            runtime,
            session,
            store: ControlledConfigStore::new(root.join("controlled")),
            paused_request_started: Some(paused_request_started),
            release_response: Some(release_response),
            fail_selection_readback,
            fail_workspace_reads,
            server,
            root,
        }
    }
}

impl Drop for ProxyConfigurationFixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = self.runtime.block_on(&mut self.server);
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

pub(super) fn open_session_page_window(
    cx: &mut gpui_kit::TestAppContext,
    fixture: &ProxyConfigurationFixture,
) -> (
    gpui_kit::AnyWindowHandle,
    gpui_kit::Entity<super::ProxiesPage>,
) {
    use gpui_kit::{AppContext, component::Root, px, size};
    let mut page = None;
    let window = cx.open_window(size(px(1200.), px(1000.)), |window, cx| {
        let view = cx.new(|cx| {
            super::ProxiesPage::new_with_session(
                fixture.session.clone(),
                fixture.runtime.handle().clone(),
                cx,
            )
        });
        page = Some(view.clone());
        Root::new(view, window, cx)
    });
    (window.into(), page.unwrap())
}

#[expect(
    clippy::future_not_send,
    reason = "GPUI state waiting retains its foreground-thread context"
)]
pub(super) async fn wait_for_page_state(
    cx: &mut gpui_kit::TestAppContext,
    fixture: &ProxyConfigurationFixture,
    page: &gpui_kit::Entity<super::ProxiesPage>,
    predicate: impl Fn(&super::ProxiesPage) -> bool,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if cx.update(|cx| predicate(page.read(cx))) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "proxy page state did not settle within 10 seconds"
        );
        fixture
            .runtime
            .spawn(async {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            })
            .await
            .unwrap();
    }
}

#[expect(
    clippy::future_not_send,
    reason = "GPUI test window setup retains its foreground-thread context"
)]
pub(super) async fn open_ready_session_page_window(
    cx: &mut gpui_kit::TestAppContext,
    fixture: &ProxyConfigurationFixture,
) -> (
    gpui_kit::AnyWindowHandle,
    gpui_kit::Entity<super::ProxiesPage>,
) {
    let (window, page) = open_session_page_window(cx, fixture);
    page.update(cx, |page, cx| page.refresh(cx));
    wait_for_page_state(cx, fixture, &page, |page| !page.loading).await;
    cx.update(|cx| {
        assert!(page.read(cx).catalog.is_some());
        assert!(page.read(cx).error.is_none());
    });
    (window, page)
}

use super::*;
use gpui_kit::component::Root;
use gpui_kit::{AnyWindowHandle, AppContext, Entity, TestAppContext, size};
pub(super) fn open_catalog(
    cx: &mut TestAppContext,
    catalog: ProxyCatalog,
) -> (
    AnyWindowHandle,
    Entity<ProxiesPage>,
    tokio::runtime::Runtime,
) {
    open_catalog_at(cx, catalog, size(px(1200.), px(1000.)))
}

pub(super) fn open_catalog_at(
    cx: &mut TestAppContext,
    catalog: ProxyCatalog,
    dimensions: gpui_kit::Size<gpui_kit::Pixels>,
) -> (
    AnyWindowHandle,
    Entity<ProxiesPage>,
    tokio::runtime::Runtime,
) {
    cx.update(gpui_kit::init);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client =
        MihomoClient::new(zenclash_core::MihomoEndpoint::new("http://127.0.0.1:1", "")).unwrap();
    let mut page = None;
    let window = cx.open_window(dimensions, |window, cx| {
        let view = cx.new(|cx| {
            let mut page = ProxiesPage::new(client, runtime.handle().clone(), cx);
            let indices = presentation::visible_group_indices(&catalog, "rule", false);
            page.install_catalog(catalog, "rule".into(), indices);
            page
        });
        page = Some(view.clone());
        Root::new(view, window, cx)
    });
    (window.into(), page.unwrap(), runtime)
}
