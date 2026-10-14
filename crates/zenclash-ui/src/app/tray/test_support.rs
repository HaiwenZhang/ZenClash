//! Owned loopback controller and real application-window construction for tray origin tests.
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;
use zenclash_core::{CoreKind, CoreSession, MihomoClient, MihomoEndpoint};

pub(super) struct TrayController {
    pub(super) endpoint: String,
    pub(super) requests: Arc<Mutex<Vec<String>>>,
    pub(super) started: Option<tokio::sync::oneshot::Receiver<()>>,
    pub(super) release: Option<tokio::sync::oneshot::Sender<()>>,
    pub(super) store: zenclash_core::ControlledConfigStore,
    fail_catalog_reads: Arc<std::sync::atomic::AtomicBool>,
    root: std::path::PathBuf,
    task: tokio::task::JoinHandle<()>,
    runtime: tokio::runtime::Handle,
}

impl TrayController {
    pub(super) fn fail_catalog_reads(&self) {
        self.fail_catalog_reads
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub(super) fn new(
        runtime: &tokio::runtime::Handle,
        pause_prefix: Option<&'static str>,
    ) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "zenclash-tray-controller-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let fail_catalog_reads = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fail_catalog = fail_catalog_reads.clone();
        let task = runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            let mut pause = pause_prefix.map(|prefix| (prefix, entered, released));
            let mut replies = tokio::task::JoinSet::new();
            let mut mode = "rule".to_owned();
            let mut member = "node".to_owned();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let receipt = match zenclash_core::test_support::read_controller_request(&mut stream).await {
                    Ok(receipt) => receipt,
                    Err(error) if zenclash_core::test_support::is_controller_header_eof(&error) => continue,
                    Err(error) => panic!("tray fixture request: {error}"),
                };
                let (headers, payload) = receipt.into_parts();
                let request = headers.lines().next().unwrap().to_owned();
                recorded.lock().unwrap().push(request.clone());
                if fail_catalog.load(std::sync::atomic::Ordering::Acquire) && request.starts_with("GET /proxies ") {
                    let _ = stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                    continue;
                }
                let body = if request.starts_with("PATCH /configs ") {
                    let value: serde_json::Value = serde_json::from_slice(&payload).unwrap();
                    mode = value["mode"].as_str().unwrap().to_owned();
                    None
                } else if request.starts_with("GET /configs ") {
                    Some(serde_json::json!({"mode":mode,"mixed-port":7890}).to_string())
                } else if request.starts_with("GET /proxies ") {
                    Some(serde_json::json!({"proxies":{"group":{"type":"Selector","now":member,"all":["node","Requested","Second"]},"node":{"type":"Shadowsocks"},"Requested":{"type":"Shadowsocks"},"Second":{"type":"Shadowsocks"}}}).to_string())
                } else if request.starts_with("PUT /proxies/group ") {
                    let value: serde_json::Value = serde_json::from_slice(&payload).unwrap();
                    member = value["name"].as_str().unwrap().to_owned();
                    None
                } else if request.starts_with("GET /proxies/group ") {
                    Some(serde_json::json!({"type":"Selector","now":member}).to_string())
                } else if request.starts_with("GET /group/group/delay?") {
                    Some(r#"{"node":42}"#.to_owned())
                } else if request.starts_with("GET /version ") {
                    Some(r#"{"meta":true,"version":"fixture"}"#.to_owned())
                } else {
                    let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                    continue;
                };
                let response = body.map_or_else(
                    || "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned(),
                    |body| format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()),
                );
                if pause.as_ref().is_some_and(|(prefix, _, _)| request.starts_with(prefix))
                    && let Some((_, entered, released)) = pause.take()
                {
                    entered.send(()).unwrap();
                    replies.spawn(async move {
                        if released.await.is_ok() { let _ = stream.write_all(response.as_bytes()).await; }
                    });
                } else { let _ = stream.write_all(response.as_bytes()).await; }
            }
        });
        Self {
            endpoint,
            requests,
            fail_catalog_reads,
            started: Some(started),
            release: Some(release),
            task,
            runtime: runtime.clone(),
            store: zenclash_core::ControlledConfigStore::new(root.join("controlled")),
            root,
        }
    }

    pub(super) fn session(&self) -> CoreSession {
        CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(MihomoEndpoint::new(&self.endpoint, "")).unwrap(),
        )
        .unwrap()
    }
}

impl Drop for TrayController {
    fn drop(&mut self) {
        self.task.abort();
        let _ = self.runtime.block_on(&mut self.task);
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

pub(super) fn open_app(
    cx: &mut gpui_kit::TestAppContext,
    fixture: &crate::pages::runtime::test_support::RuntimePageFixture,
    session: CoreSession,
) -> (
    gpui_kit::AnyWindowHandle,
    gpui_kit::Entity<crate::app::ZenClashApp>,
) {
    use gpui_kit::{AppContext, component::Root, px, size};
    let mut services = crate::app::test_support::app_test_services(fixture);
    services.client = session.client().clone();
    services.core_session = session;
    services.profile_path = None;
    cx.executor().allow_parking();
    cx.update(crate::app::init);
    let mut owner = None;
    let window = cx.open_window(size(px(1280.), px(820.)), |window, cx| {
        let preferences = services.preferences.clone();
        let app = cx.new(|cx| {
            crate::app::ZenClashApp::new(services, None, None, None, preferences, window, cx)
        });
        owner = Some(app.clone());
        Root::new(app, window, cx)
    });
    let owner = owner.unwrap();
    let page = cx.update(|cx| owner.read(cx).runtime_page.clone());
    fixture.wait_for_initial_reads(cx, &page);
    (window.into(), owner)
}
