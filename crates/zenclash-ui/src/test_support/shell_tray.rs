//! Shared isolated application services, native tray menu builders, and shell window helpers.

/// Builds isolated `AppServices` backed by `fixture` without touching OS user directories.
#[must_use]
pub fn app_test_services(
    fixture: &crate::test_support::runtime_host::RuntimePageFixture,
) -> crate::shell::app::AppServices {
    let services = fixture.services();
    crate::shell::app::AppServices {
        initializing: true,
        await_service_handoff: false,
        profile_store: services.profile_store,
        override_store: services.override_store,
        preferences_store: None,
        preferences: services.preferences.clone(),
        core_kind: services.core_kind,
        core_session: services.core_session,
        traffic_capture: services.traffic_capture,
        system_proxy_session: services.system_proxy_session,
        client: services.client,
        traffic_monitor: services.traffic_monitor,
        log_monitor: services.log_monitor,
        traffic_history_store: services.traffic_history_store,
        traffic_history_session: None,
        profile_path: services.profile_path,
        controlled_config_store: services.controlled_config_store,
        runtime: services.runtime,
        startup_notice: None,
        startup_error: None,
        restart_after_exit: Default::default(),
        safe_mode: zenclash_app::SafeMode::disabled(),
    }
}

// Owned loopback controller and real application-window construction for tray origin tests.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;
use zenclash_core::{CoreKind, CoreSession, MihomoClient, MihomoEndpoint};

/// Loopback mock Mihomo controller tailored for shell and native tray command tests.
pub struct TrayController {
    pub(crate) endpoint: String,
    pub(crate) requests: Arc<Mutex<Vec<String>>>,
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "used by internal white-box tray tests")
    )]
    pub(crate) started: Option<tokio::sync::oneshot::Receiver<()>>,
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "used by internal white-box tray tests")
    )]
    pub(crate) release: Option<tokio::sync::oneshot::Sender<()>>,
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "used by internal white-box tray tests")
    )]
    pub(crate) store: zenclash_core::ControlledConfigStore,
    fail_catalog_reads: Arc<std::sync::atomic::AtomicBool>,
    root: std::path::PathBuf,
    task: tokio::task::JoinHandle<()>,
    runtime: tokio::runtime::Handle,
}

impl TrayController {
    /// Forces subsequent `GET /proxies` requests to return HTTP 503.
    pub fn fail_catalog_reads(&self) {
        self.fail_catalog_reads
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Returns all HTTP request lines received by this controller.
    ///
    /// # Panics
    ///
    /// Panics if the internal request log mutex is poisoned.
    #[must_use]
    pub fn recorded_requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    /// Spawns a loopback controller on `runtime`, optionally pausing the first matching request.
    ///
    /// # Panics
    ///
    /// Panics if creating the temporary fixture directory or binding localhost fails.
    #[must_use]
    pub fn new(runtime: &tokio::runtime::Handle, pause_prefix: Option<&'static str>) -> Self {
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

    /// Opens a `CoreSession` pointed at this controller's loopback endpoint.
    ///
    /// # Panics
    ///
    /// Panics if constructing the Mihomo client or session fails.
    #[must_use]
    pub fn session(&self) -> CoreSession {
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

/// Opens a full production `ZenClashApp` inside a headless `Root` window and waits for initial reads.
///
/// # Panics
///
/// Panics if window initialization fails or initial runtime reads do not settle.
pub fn open_app(
    cx: &mut gpui_kit::TestAppContext,
    fixture: &crate::test_support::runtime_host::RuntimePageFixture,
    session: CoreSession,
) -> (
    gpui_kit::AnyWindowHandle,
    gpui_kit::Entity<crate::shell::app::ZenClashApp>,
) {
    open_app_with_safe_mode(cx, fixture, session, zenclash_app::SafeMode::disabled())
}

/// Opens a full production `ZenClashApp` with an explicit `SafeMode` policy inside a headless window.
///
/// # Panics
///
/// Panics if window initialization fails or initial runtime reads do not settle.
pub fn open_app_with_safe_mode(
    cx: &mut gpui_kit::TestAppContext,
    fixture: &crate::test_support::runtime_host::RuntimePageFixture,
    session: CoreSession,
    safe_mode: zenclash_app::SafeMode,
) -> (
    gpui_kit::AnyWindowHandle,
    gpui_kit::Entity<crate::shell::app::ZenClashApp>,
) {
    use gpui_kit::{AppContext, component::Root, px, size};
    let mut services = app_test_services(fixture);
    services.client = session.client().clone();
    services.core_session = session;
    services.profile_path = None;
    services.safe_mode = safe_mode;
    cx.executor().allow_parking();
    cx.update(|cx| {
        crate::shell::app::init(cx);
        cx.set_reduce_motion(true);
    });
    let mut owner = None;
    let window = cx.open_window(size(px(1280.), px(820.)), |window, cx| {
        let preferences = services.preferences.clone();
        let app = cx.new(|cx| {
            crate::shell::app::ZenClashApp::new(services, None, None, None, preferences, window, cx)
        });
        owner = Some(app.clone());
        Root::new(app, window, cx)
    });
    let owner = owner.unwrap();
    let page = cx.update(|cx| owner.read(cx).runtime_page.clone());
    fixture.wait_for_initial_reads(cx, &page);
    (window.into(), owner)
}

/// Returns the active destination selected in `app`.
#[must_use]
pub fn app_current_page(
    cx: &mut gpui_kit::TestAppContext,
    app: &gpui_kit::Entity<crate::shell::app::ZenClashApp>,
) -> crate::foundation::actions::Page {
    cx.update(|cx| app.read(cx).current_page_for_test())
}

/// Returns whether the navigation sidebar is currently collapsed in `app`.
#[must_use]
pub fn app_sidebar_collapsed(
    cx: &mut gpui_kit::TestAppContext,
    app: &gpui_kit::Entity<crate::shell::app::ZenClashApp>,
) -> bool {
    cx.update(|cx| app.read(cx).sidebar_collapsed_for_test())
}

/// Returns the currently displayed outbound routing mode in `app`.
#[must_use]
pub fn app_outbound_mode(
    cx: &mut gpui_kit::TestAppContext,
    app: &gpui_kit::Entity<crate::shell::app::ZenClashApp>,
) -> crate::outbound_mode::OutboundMode {
    cx.update(|cx| app.read(cx).outbound_mode_for_test())
}

/// Builds the native tray menu from `state` and returns its generated menu-ID-to-command bindings.
///
/// # Errors
///
/// Returns an error if constructing any native tray menu item fails.
pub fn build_tray_menu_commands(
    state: &crate::shell::tray_native::TrayMenuState,
) -> Result<HashMap<String, crate::shell::tray_native::TrayCommand>, String> {
    crate::shell::tray_native::build_menu_commands_for_test(state)
}

/// Black-box test harness around the native tray menu command-to-observation bindings.
pub struct TrayCommandBindingHarness(crate::shell::tray_native::TrayCommandBindingsForTest);

impl TrayCommandBindingHarness {
    /// Creates a binding table associating `commands` with an optional observation identity.
    #[must_use]
    pub fn new(
        commands: HashMap<String, crate::shell::tray_native::TrayCommand>,
        observation: Option<zenclash_core::CoreObservationIdentity>,
    ) -> Self {
        Self(crate::shell::tray_native::TrayCommandBindingsForTest::new(
            commands,
            observation,
        ))
    }

    /// Replaces the observation identity attached to subsequent resolved tray commands.
    pub fn update_observation(
        &mut self,
        observation: Option<zenclash_core::CoreObservationIdentity>,
    ) {
        self.0.update_observation(observation);
    }

    /// Resolves a menu item identifier into its command and attached observation identity.
    #[must_use]
    pub fn resolve(
        &self,
        id: &str,
    ) -> Option<(
        crate::shell::tray_native::TrayCommand,
        Option<zenclash_core::CoreObservationIdentity>,
    )> {
        self.0.resolve_command(id)
    }
}

/// Applies staged menu preference previews onto `preferences` using `PreferencePreviews`.
pub fn apply_preference_previews(
    preferences: &mut zenclash_core::AppPreferences,
    appearance: Option<zenclash_core::AppearancePreference>,
    tray_visible: Option<bool>,
) {
    use crate::foundation::primitives::preference_previews::{
        MenuPreferenceChange, PreferencePreviews,
    };
    let mut changes = Vec::new();
    if let Some(value) = appearance {
        changes.push(MenuPreferenceChange::Appearance(value));
    }
    if let Some(value) = tray_visible {
        changes.push(MenuPreferenceChange::TrayVisibility(value));
    }
    PreferencePreviews::new(changes).apply(preferences);
}
