//! Shared isolated runtime-page services and bounded foreground-state waiting.
use crate::pages::runtime::RuntimePage;
use gpui_kit::Entity;
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);
use zenclash_core::MihomoLogLevel;

pub(in crate::pages::runtime) fn runtime_page_test_directory(name: &str) -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "zenclash-page-{name}-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    // Wall-clock resolution alone cannot distinguish concurrent fixtures.
    // Exclusive creation also rejects reuse instead of silently sharing files.
    std::fs::create_dir(&directory).unwrap();
    directory
}

pub(in crate::pages::runtime) fn runtime_page_services(
    runtime: &tokio::runtime::Runtime,
    directory: &std::path::Path,
    store: &zenclash_core::AppPreferencesStore,
    preferences: zenclash_core::AppPreferences,
) -> crate::pages::runtime::RuntimePageServices {
    use crate::pages::runtime::RuntimePageServices;
    use zenclash_core::{
        ControlledConfigStore, CoreKind, CoreSession, MihomoClient, MihomoEndpoint,
        OperationalStatus, TrafficCaptureSession, TrafficMonitor,
    };
    let endpoint = MihomoEndpoint::new("http://127.0.0.1:1", "");
    let client = MihomoClient::new(endpoint.clone()).unwrap();
    let core =
        CoreSession::open_with_config(CoreKind::Mihomo, client.clone(), None, Vec::new()).unwrap();
    let traffic = TrafficMonitor::start(runtime.handle(), endpoint.clone());
    let logs = zenclash_core::LogMonitor::start(runtime.handle(), endpoint, MihomoLogLevel::Info);
    let status = OperationalStatus::start(
        runtime.handle(),
        core.clone(),
        None,
        traffic.clone(),
        logs.clone(),
    );
    let controlled = ControlledConfigStore::new(directory.join("controlled"));
    RuntimePageServices {
        profile_store: None,
        override_store: None,
        core_kind: CoreKind::Mihomo,
        core_session: core.clone(),
        profile_service: crate::ApplicationCommands::new(core.clone(), None),
        client,
        runtime: runtime.handle().clone(),
        traffic_monitor: traffic,
        log_monitor: logs,
        operational_status: status,
        traffic_capture: TrafficCaptureSession::new(core, controlled.clone(), None, None),
        profile_path: None,
        controlled_config_store: controlled,
        preferences_store: Some(store.clone()),
        preferences,
        system_proxy_session: None,
        traffic_history_store: None,
        startup_notice: None,
        startup_error: None,
    }
}

#[expect(
    clippy::future_not_send,
    reason = "GPUI test contexts remain on the foreground thread"
)]
pub(in crate::pages::runtime) async fn wait_for_page_state(
    cx: &mut gpui_kit::TestAppContext,
    runtime: &tokio::runtime::Runtime,
    page: &Entity<RuntimePage>,
    predicate: impl Fn(&RuntimePage) -> bool,
) {
    for _ in 0..1000 {
        if cx.update(|cx| predicate(page.read(cx))) {
            return;
        }
        runtime
            .spawn(async {
                tokio::time::sleep(Duration::from_millis(5)).await;
            })
            .await
            .unwrap();
    }
    panic!("runtime page state did not settle");
}

use crate::pages::runtime::*;
use gpui_kit::component::Root;
use gpui_kit::{TestAppContext, size};
use std::fs;
use zenclash_core::ManagedCoreProcess;
pub(crate) struct RuntimePageFixture {
    pub(in crate::pages::runtime) root: PathBuf,
    pub(in crate::pages::runtime) managed_process: Option<Arc<ManagedCoreProcess>>,
    pub(in crate::pages::runtime) runtime: Option<tokio::runtime::Runtime>,
    pub(in crate::pages::runtime) profiles: ProfileStore,
    pub(in crate::pages::runtime) overrides: YamlOverrideStore,
    pub(in crate::pages::runtime) controlled: ControlledConfigStore,
    pub(in crate::pages::runtime) profile: PathBuf,
    pub(in crate::pages::runtime) status: Arc<OperationalStatus>,
    pub(in crate::pages::runtime) core: CoreSession,
    pub(in crate::pages::runtime) traffic: Arc<TrafficMonitor>,
    pub(in crate::pages::runtime) logs: Arc<LogMonitor>,
}

impl RuntimePageFixture {
    pub(crate) fn new() -> Self {
        Self::with_controller("http://127.0.0.1:1".to_owned())
    }

    pub(in crate::pages::runtime) fn with_controller(controller: String) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        Self::with_controller_and_runtime(controller, runtime)
    }

    pub(in crate::pages::runtime) fn with_controller_and_runtime(
        controller: String,
        runtime: tokio::runtime::Runtime,
    ) -> Self {
        let root = runtime_page_test_directory("review");
        let profiles = ProfileStore::new(root.join("profiles")).unwrap();
        let source = root.join("source.yaml");
        fs::write(&source, "mixed-port: 7890\nrules: [MATCH,DIRECT]\n").unwrap();
        let record = profiles.import_local(source).unwrap();
        let profile = profiles.activate(&record.id).unwrap();
        let overrides = YamlOverrideStore::new(root.join("overrides")).unwrap();
        let controlled = ControlledConfigStore::new(root.join("controlled"));
        let endpoint = zenclash_core::MihomoEndpoint::new("http://127.0.0.1:1", "");
        let client = MihomoClient::new(zenclash_core::MihomoEndpoint::new(controller, "")).unwrap();
        let core = CoreSession::open_with_config(
            CoreKind::Mihomo,
            client,
            Some(profile.clone()),
            Vec::new(),
        )
        .unwrap();
        let traffic = TrafficMonitor::start(runtime.handle(), endpoint.clone());
        let logs = LogMonitor::start(runtime.handle(), endpoint, MihomoLogLevel::Info);
        let status = OperationalStatus::start(
            runtime.handle(),
            core.clone(),
            None,
            traffic.clone(),
            logs.clone(),
        );
        Self {
            root,
            managed_process: None,
            runtime: Some(runtime),
            profiles,
            overrides,
            controlled,
            profile,
            status,
            core,
            traffic,
            logs,
        }
    }

    pub(crate) fn services(&self) -> RuntimePageServices {
        RuntimePageServices {
            profile_store: Some(self.profiles.clone()),
            override_store: Some(self.overrides.clone()),
            core_kind: CoreKind::Mihomo,
            core_session: self.core.clone(),
            profile_service: crate::ApplicationCommands::new(
                self.core.clone(),
                Some(self.overrides.clone()),
            ),
            client: self.core.client().clone(),
            runtime: self.runtime.as_ref().unwrap().handle().clone(),
            traffic_monitor: self.traffic.clone(),
            log_monitor: self.logs.clone(),
            operational_status: self.status.clone(),
            traffic_capture: TrafficCaptureSession::new(
                self.core.clone(),
                self.controlled.clone(),
                None,
                Some(self.profile.clone()),
            ),
            profile_path: Some(self.profile.clone()),
            controlled_config_store: self.controlled.clone(),
            preferences_store: None,
            preferences: AppPreferences::default(),
            system_proxy_session: None,
            traffic_history_store: Some(TrafficHistoryStore::new(self.root.join("history.sqlite"))),
            startup_notice: None,
            startup_error: None,
        }
    }

    pub(in crate::pages::runtime) fn enter_runtime(&self) -> tokio::runtime::EnterGuard<'_> {
        self.runtime.as_ref().unwrap().enter()
    }

    pub(crate) fn wait_for_initial_reads(
        &self,
        cx: &mut TestAppContext,
        page: &Entity<RuntimePage>,
    ) {
        self.settle(cx, page, |page| {
            !page.loading && !page.persistent_loading && !page.config_inputs.is_loading()
        });
    }

    pub(in crate::pages::runtime) fn settle(
        &self,
        cx: &mut TestAppContext,
        page: &Entity<RuntimePage>,
        predicate: impl Fn(&RuntimePage) -> bool,
    ) {
        self.settle_with_context(cx, page, |page, _| predicate(page));
    }

    pub(in crate::pages::runtime) fn settle_connections(
        &self,
        cx: &mut TestAppContext,
        page: &Entity<RuntimePage>,
    ) {
        self.settle_with_context(cx, page, |page, cx| page.connections.read(cx).is_idle());
    }

    pub(in crate::pages::runtime) fn settle_with_context(
        &self,
        cx: &mut TestAppContext,
        page: &Entity<RuntimePage>,
        predicate: impl Fn(&RuntimePage, &App) -> bool,
    ) {
        cx.foreground_executor().clone().block_test(async {
            for _ in 0..1_000 {
                if cx.update(|cx| predicate(page.read(cx), cx)) {
                    return;
                }
                self.runtime
                    .as_ref()
                    .unwrap()
                    .spawn(async {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    })
                    .await
                    .unwrap();
            }
            panic!("UI operation did not complete");
        });
    }
}

impl Drop for RuntimePageFixture {
    fn drop(&mut self) {
        self.status.stop();
        if let Some(process) = self.managed_process.take() {
            process
                .stop()
                .expect("stop isolated core before removing private data");
        }
        self.runtime
            .take()
            .unwrap()
            .shutdown_timeout(Duration::from_secs(1));
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub(in crate::pages::runtime) fn open_runtime_page_window(
    cx: &mut TestAppContext,
    fixture: &RuntimePageFixture,
    initial: Page,
) -> (AnyWindowHandle, Entity<RuntimePage>) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
    });
    let mut page = None;
    let height = if initial == Page::Settings {
        2600.
    } else {
        1000.
    };
    let handle = cx.open_window(size(px(1200.), px(height)), |window, cx| {
        let view = cx.new(|cx| RuntimePage::new(initial, fixture.services(), window, cx));
        page = Some(view.clone());
        Root::new(view, window, cx)
    });
    (handle.into(), page.unwrap())
}
