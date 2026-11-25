//! Shared isolated runtime-page services and bounded foreground-state waiting.
use crate::features::host::RuntimePage;
use gpui_kit::Entity;
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);
use zenclash_core::MihomoLogLevel;

pub(crate) fn runtime_page_test_directory(name: &str) -> std::path::PathBuf {
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

#[cfg(test)]
pub(crate) fn runtime_page_services(
    runtime: &tokio::runtime::Runtime,
    directory: &std::path::Path,
    store: &zenclash_core::AppPreferencesStore,
    preferences: zenclash_core::AppPreferences,
) -> crate::runtime_host::services::RuntimePageServices {
    use crate::runtime_host::services::RuntimePageServices;
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

#[cfg(test)]
#[expect(
    clippy::future_not_send,
    reason = "GPUI test contexts remain on the foreground thread"
)]
pub(crate) async fn wait_for_page_state(
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

use crate::foundation::actions::Page;
use crate::runtime_host::services::RuntimePageServices;
use gpui_kit::component::Root;
use gpui_kit::{AnyWindowHandle, App, AppContext, px};
use gpui_kit::{TestAppContext, size};
use std::fs;
use std::{path::PathBuf, sync::Arc};
use zenclash_core::ManagedCoreProcess;
use zenclash_core::{
    AppPreferences, ControlledConfigStore, CoreKind, CoreSession, LogMonitor, MihomoClient,
    OperationalStatus, ProfileStore, TrafficCaptureSession, TrafficHistoryStore, TrafficMonitor,
    YamlOverrideStore,
};
/// Isolated `RuntimePage` fixture with private temp stores, Tokio runtime, and mockable core session.
pub struct RuntimePageFixture {
    pub(crate) root: PathBuf,
    pub(crate) managed_process: Option<Arc<ManagedCoreProcess>>,
    pub(crate) runtime: Option<tokio::runtime::Runtime>,
    pub(crate) profiles: ProfileStore,
    pub(crate) overrides: YamlOverrideStore,
    pub(crate) controlled: ControlledConfigStore,
    pub(crate) profile: PathBuf,
    pub(crate) status: Arc<OperationalStatus>,
    pub(crate) core: CoreSession,
    pub(crate) traffic: Arc<TrafficMonitor>,
    pub(crate) logs: Arc<LogMonitor>,
}

impl Default for RuntimePageFixture {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimePageFixture {
    /// Creates an isolated fixture pointing at an offline loopback controller address.
    ///
    /// # Panics
    ///
    /// Panics if constructing the temporary directories, profile store, or Tokio runtime fails.
    #[must_use]
    pub fn new() -> Self {
        Self::with_controller("http://127.0.0.1:1".to_owned())
    }

    /// Creates an isolated fixture wired to the supplied loopback controller URL.
    ///
    /// # Panics
    ///
    /// Panics if constructing the Tokio runtime or temporary fixture stores fails.
    #[must_use]
    pub fn with_controller(controller: String) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        Self::with_controller_and_runtime(controller, runtime)
    }

    /// Creates an isolated fixture using the caller-provided Tokio runtime and controller URL.
    ///
    /// # Panics
    ///
    /// Panics if initializing the temporary profile or override stores fails.
    #[must_use]
    pub fn with_controller_and_runtime(
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

    /// Returns the isolated temporary root directory owned by this fixture.
    #[must_use]
    pub fn root_dir(&self) -> &std::path::Path {
        &self.root
    }

    /// Returns the isolated profile store owned by this fixture.
    #[must_use]
    pub fn profiles(&self) -> &ProfileStore {
        &self.profiles
    }

    /// Returns the isolated YAML override store owned by this fixture.
    #[must_use]
    pub fn overrides(&self) -> &YamlOverrideStore {
        &self.overrides
    }

    /// Returns the isolated controlled-config store owned by this fixture.
    #[must_use]
    pub fn controlled_config(&self) -> &ControlledConfigStore {
        &self.controlled
    }

    /// Returns a handle to the Tokio runtime owned by this fixture.
    ///
    /// # Panics
    ///
    /// Panics if called after the fixture has been dropped.
    #[must_use]
    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.runtime.as_ref().unwrap().handle().clone()
    }

    /// Builds `RuntimePageServices` bound to this fixture's isolated stores and runtime.
    ///
    /// # Panics
    ///
    /// Panics if called after the fixture has been dropped.
    #[must_use]
    pub fn services(&self) -> RuntimePageServices {
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

    #[cfg(test)]
    pub(crate) fn enter_runtime(&self) -> tokio::runtime::EnterGuard<'_> {
        self.runtime.as_ref().unwrap().enter()
    }

    /// Waits until initial page loading, persistent store loading, and config inputs have settled.
    ///
    /// # Panics
    ///
    /// Panics if the page does not settle within the bounded test deadline.
    pub fn wait_for_initial_reads(&self, cx: &mut TestAppContext, page: &Entity<RuntimePage>) {
        self.settle(cx, page, |page| {
            !page.loading && !page.persistent_loading && !page.config_inputs.is_loading()
        });
    }

    /// Waits until `page`'s override catalog reaches `expected` items.
    ///
    /// # Panics
    ///
    /// Panics if the override count does not match within the bounded test deadline.
    pub fn settle_override_count(
        &self,
        cx: &mut TestAppContext,
        page: &Entity<RuntimePage>,
        expected: usize,
    ) {
        self.settle(cx, page, |page| page.override_count_for_test() == expected);
    }

    pub(crate) fn settle(
        &self,
        cx: &mut TestAppContext,
        page: &Entity<RuntimePage>,
        predicate: impl Fn(&RuntimePage) -> bool,
    ) {
        self.settle_with_context(cx, page, |page, _| predicate(page));
    }

    /// Waits until the `ConnectionsView` child entity finishes all pending projections.
    ///
    /// # Panics
    ///
    /// Panics if the connections view does not become idle within the bounded test deadline.
    pub fn settle_connections(&self, cx: &mut TestAppContext, page: &Entity<RuntimePage>) {
        self.settle_with_context(cx, page, |page, cx| page.connections_idle(cx));
    }

    /// Waits until the `RulesView` child entity finishes all pending rule projections.
    ///
    /// # Panics
    ///
    /// Panics if the rules view does not become idle within the bounded test deadline.
    pub fn settle_rules(&self, cx: &mut TestAppContext, page: &Entity<RuntimePage>) {
        self.settle_with_context(cx, page, |page, cx| page.rules_idle(cx));
    }

    /// Polls `condition` on the GPUI foreground executor using this fixture's Tokio timer.
    ///
    /// # Panics
    ///
    /// Panics if `condition` does not become `true` within the bounded test deadline.
    pub fn wait_until(
        &self,
        cx: &mut TestAppContext,
        mut condition: impl FnMut(&mut TestAppContext) -> bool,
    ) {
        cx.foreground_executor().clone().block_test(async {
            for _ in 0..1_000 {
                if condition(cx) {
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
            panic!("condition did not settle within bounded deadline");
        });
    }

    pub(crate) fn settle_with_context(
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

/// Opens a headless GPUI `Root` window hosting a `RuntimePage` starting at `initial`.
///
/// # Panics
///
/// Panics if the headless window fails to construct the `RuntimePage` view entity.
pub fn open_runtime_page_window(
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

/// Switches `page` to `target` using the production `RuntimePage::switch_to` path.
pub fn switch_runtime_page(cx: &mut TestAppContext, page: &Entity<RuntimePage>, target: Page) {
    cx.update(|cx| {
        page.update(cx, |page, cx| page.switch_to(target, cx));
    });
}

/// Returns the active destination currently presented by `page`.
#[must_use]
pub fn current_runtime_page(cx: &mut TestAppContext, page: &Entity<RuntimePage>) -> Page {
    cx.update(|cx| page.read(cx).current_page_for_test())
}

/// Returns whether `page` currently has an admitted core mutation in flight.
#[must_use]
pub fn is_core_busy(cx: &mut TestAppContext, page: &Entity<RuntimePage>) -> bool {
    cx.update(|cx| page.read(cx).is_core_busy_for_test())
}

/// Seeds a `ConnectionsSnapshot` into `page` through the normal snapshot replacement path.
pub fn seed_connections_snapshot(
    cx: &mut TestAppContext,
    page: &Entity<RuntimePage>,
    snapshot: zenclash_core::ConnectionsSnapshot,
) {
    cx.update(|cx| {
        page.update(cx, |page, cx| {
            page.seed_connections_snapshot_for_test(snapshot, cx);
        });
    });
}

/// Seeds a `RuntimeConfig` snapshot into `page` for configuration-page UI integration tests.
pub fn seed_runtime_config(
    cx: &mut TestAppContext,
    page: &Entity<RuntimePage>,
    config: zenclash_core::RuntimeConfig,
) {
    cx.update(|cx| {
        page.update(cx, |page, cx| {
            page.seed_runtime_config_for_test(config, cx);
        });
    });
}

/// Attaches an isolated preferences store and value snapshot to `page`.
pub fn seed_preferences_store(
    cx: &mut TestAppContext,
    page: &Entity<RuntimePage>,
    store: zenclash_core::AppPreferencesStore,
    preferences: AppPreferences,
) {
    cx.update(|cx| {
        page.update(cx, |page, cx| {
            page.seed_preferences_store_for_test(store, preferences, cx);
        });
    });
}
