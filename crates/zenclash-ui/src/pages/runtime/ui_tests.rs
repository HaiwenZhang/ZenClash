use std::{
    fs,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{TestAppContext, VisualTestContext, size};

use super::*;
use zenclash_core::MihomoProcess;

#[cfg(target_os = "windows")]
mod design_validation;

pub(super) struct Fixture {
    root: PathBuf,
    managed_process: Option<Arc<MihomoProcess>>,
    runtime: Option<tokio::runtime::Runtime>,
    profiles: ProfileStore,
    overrides: YamlOverrideStore,
    controlled: ControlledConfigStore,
    profile: PathBuf,
    status: Arc<OperationalStatus>,
    core: CoreSession,
    traffic: Arc<TrafficMonitor>,
    logs: Arc<LogMonitor>,
}

impl Fixture {
    pub(super) fn new() -> Self {
        Self::with_controller("http://127.0.0.1:1".to_owned())
    }

    fn with_controller(controller: String) -> Self {
        let root = std::env::temp_dir().join(format!(
            "zenclash-ui-review-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let profiles = ProfileStore::new(root.join("profiles")).unwrap();
        let source = root.join("source.yaml");
        fs::write(&source, "mixed-port: 7890\nrules: [MATCH,DIRECT]\n").unwrap();
        let record = profiles.import_local(source).unwrap();
        let profile = profiles.activate(&record.id).unwrap();
        let overrides = YamlOverrideStore::new(root.join("overrides")).unwrap();
        let controlled = ControlledConfigStore::new(root.join("controlled"));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
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

    pub(super) fn services(&self) -> RuntimePageServices {
        RuntimePageServices {
            profile_store: Some(self.profiles.clone()),
            override_store: Some(self.overrides.clone()),
            core_kind: CoreKind::Mihomo,
            core_session: self.core.clone(),
            profile_service: crate::ProfileService::new(
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

    pub(super) fn settle(
        &self,
        cx: &mut TestAppContext,
        page: &Entity<RuntimePage>,
        predicate: impl Fn(&RuntimePage) -> bool,
    ) {
        cx.foreground_executor().clone().block_test(async {
            for _ in 0..1_000 {
                if cx.update(|cx| predicate(page.read(cx))) {
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

impl Drop for Fixture {
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

pub(super) fn open(
    cx: &mut TestAppContext,
    fixture: &Fixture,
    initial: Page,
) -> (AnyWindowHandle, Entity<RuntimePage>) {
    cx.executor().allow_parking();
    cx.update(gpui_kit::init);
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

#[gpui_kit::test]
fn collapsing_subscription_form_restores_focus(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-add-subscription", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(page.read(cx).profiles.forms.adding_subscription);
        let input = page
            .read(cx)
            .profiles
            .forms
            .subscription_name
            .focus_handle(cx);
        window.focus(&input, cx);
        window.render_frame(cx);
        assert!(input.is_focused(window));
        window.click("toggle-add-subscription", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(!page.read(cx).profiles.forms.adding_subscription);
        assert!(page.read(cx).focus_handle.is_focused(window));
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn switching_pages_restores_focus_after_a_subscription_input_disappears(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-add-subscription", cx);
        let input = page
            .read(cx)
            .profiles
            .forms
            .subscription_name
            .focus_handle(cx);
        window.focus(&input, cx);
        window.render_frame(cx);
        assert!(input.is_focused(window));
        page.update(cx, |page, cx| page.switch_to(Page::Settings, cx));
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(page.read(cx).focus_handle.is_focused(window));
        window.press("tab", cx);
        assert!(window.focused(cx).is_some());
    })
    .unwrap();
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

// Ordinary owned children exercise UI binding behavior, not Mihomo or TUN acceptance.
fn owned_ui_children(fixture: &Fixture) -> [Arc<MihomoProcess>; 2] {
    let source = fixture.root.join("ui-owned-child.rs");
    fs::write(
        &source,
        "fn main() { std::thread::sleep(std::time::Duration::from_secs(30)); }",
    )
    .unwrap();
    let binary = fixture.root.join(if cfg!(windows) {
        "ui-owned-child.exe"
    } else {
        "ui-owned-child"
    });
    let compilation = std::process::Command::new("rustc")
        .args(["--edition=2024", "--crate-name", "ui_owned_child"])
        .arg(source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        compilation.status.success(),
        "{}",
        String::from_utf8_lossy(&compilation.stderr)
    );
    std::array::from_fn(|index| {
        let directory = fixture.root.join(format!("owned-{index}"));
        fs::create_dir_all(&directory).unwrap();
        let executable = directory.join(binary.file_name().unwrap());
        fs::copy(&binary, &executable).unwrap();
        let config = directory.join("profile.yaml");
        fs::write(&config, "rules: [MATCH,DIRECT]\n").unwrap();
        MihomoProcess::spawn_isolated_for_test(MihomoLaunchConfig {
            kind: CoreKind::Mihomo,
            binary: executable,
            config_file: config,
            home_dir: directory.join("home"),
            endpoint: zenclash_core::MihomoEndpoint::new("http://127.0.0.1:1", ""),
            controller_override: None,
        })
        .unwrap()
    })
}

#[gpui_kit::test]
fn stopped_owner_completion_cannot_authorize_old_config_for_a_new_owner(cx: &mut TestAppContext) {
    exercise_stop_completion(cx, true);
}

#[gpui_kit::test]
fn stopping_an_owner_discards_its_previous_controller_config(cx: &mut TestAppContext) {
    exercise_stop_completion(cx, false);
}

fn exercise_stop_completion(cx: &mut TestAppContext, replace_owner: bool) {
    let fixture = Fixture::new();
    let [previous, replacement] = owned_ui_children(&fixture);
    let runtime = fixture.runtime.as_ref().unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(previous.clone()))
        .unwrap();
    // This tall headless viewport exercises the production action, not native layout acceptance.
    let (window, page) = open(cx, &fixture, Page::Settings);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.switch_to(Page::Mihomo, cx);
            page.invalidate_page_load();
            page.data = RuntimeData::Core {
                version: VersionInfo::default(),
                config: RuntimeConfig {
                    mixed_port: 12345,
                    ..Default::default()
                },
            };
            page.data_runtime_version = fixture.core.generation();
        });
        window.render_frame(cx);
        window.click("stop-mihomo-core", cx);
    })
    .unwrap();
    // Do not pump the GUI while the real owned child is stopped and another owner is published.
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while previous.snapshot().pid.is_some() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        if replace_owner {
            fixture
                .core
                .switch_to_process(replacement.clone())
                .await
                .unwrap();
        }
    });
    fixture.settle(cx, &page, |page| !page.core_busy());
    cx.update_window(window, |_, window, cx| {
        assert!(
            page.read(cx).config().is_none(),
            "stopped controller data was republished as current"
        );
        if replace_owner {
            assert_ne!(
                page.read(cx).notice.as_deref(),
                Some(zenclash_i18n::text("automatic.user_stopped").as_str())
            );
            assert_eq!(
                page.read(cx).mihomo_binary().as_deref(),
                Some(replacement.launch_config().binary.as_path())
            );
        }
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn actual_owner_switch_updates_the_ui_binary_source_and_detach_clears_it(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let [previous, replacement] = owned_ui_children(&fixture);
    let runtime = fixture.runtime.as_ref().unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(previous.clone()))
        .unwrap();
    let (window, page) = open(cx, &fixture, Page::Mihomo);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, _, cx| {
        page.update(cx, |page, _| {
            page.data = RuntimeData::Core {
                version: VersionInfo::default(),
                config: RuntimeConfig::default(),
            };
            page.data_runtime_version = fixture.core.generation();
        });
    })
    .unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(replacement.clone()))
        .unwrap();
    assert!(previous.snapshot().pid.is_none());
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            page.read(cx).mihomo_binary().as_deref(),
            Some(replacement.launch_config().binary.as_path())
        );
        assert!(
            page.read(cx).config().is_none(),
            "old owner runtime config must not remain current"
        );
    })
    .unwrap();
    runtime
        .block_on(
            fixture
                .core
                .switch_to_direct(zenclash_core::MihomoEndpoint::default()),
        )
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(page.read(cx).mihomo_binary().is_none());
        window.remove_window();
    })
    .unwrap();
    assert!(replacement.snapshot().pid.is_none());
}

fn open_service_tun(
    cx: &mut TestAppContext,
    fixture: &Fixture,
) -> (AnyWindowHandle, Entity<RuntimePage>) {
    open_service_page(cx, fixture, Page::Tun)
}

fn open_service_page(
    cx: &mut TestAppContext,
    fixture: &Fixture,
    initial: Page,
) -> (AnyWindowHandle, Entity<RuntimePage>) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_kit::init(cx);
        // Match Kit's dialog fixtures: pointer clicks target a resting surface.
        cx.set_reduce_motion(true);
    });
    let mut page = None;
    let height = if initial == Page::Home { 2600. } else { 1000. };
    let handle = cx.open_window(size(px(1200.), px(height)), |window, cx| {
        let mut services = fixture.services();
        let manager = zenclash_core::ServiceManager::new(
            fixture.core.clone(),
            services.traffic_capture.clone(),
        );
        services.profile_service = services.profile_service.with_service_manager(manager);
        let view = cx.new(|cx| RuntimePage::new(initial, services, window, cx));
        page = Some(view.clone());
        Root::new(view, window, cx)
    });
    (handle.into(), page.unwrap())
}

#[gpui_kit::test]
fn home_service_feedback_saved_commit_has_keyboard_details_without_resubmission(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let version = fixture.core.generation();
    let bytes = fs::read(&fixture.profile).unwrap();
    let record = fixture
        .profiles
        .load()
        .unwrap()
        .active_profile()
        .unwrap()
        .clone();
    let (window, page) = open_service_page(cx, &fixture, Page::Home);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    let navigations = std::rc::Rc::new(std::cell::Cell::new(0));
    let received = navigations.clone();
    let target = page.downgrade();
    cx.update(|cx| {
        cx.on_action(move |_: &crate::app::NavigateTun, cx| {
            received.set(received.get() + 1);
            let _ = target.update(cx, |page, cx| page.switch_to(Page::Tun, cx));
        });
    });
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.profile_service
                .publish_test_outcome(
                    zenclash_core::ProfileApplyOutcome::CommittedButRuntimeUnknown {
                        source_version: (&record).into(),
                        profile: record.clone(),
                        path: fixture.profile.clone(),
                        cause: zenclash_core::ProfileApplicationError::Task(
                            "commit reply lost".into(),
                        ),
                        runtime_version: version,
                    },
                )
                .unwrap();
            page.synchronize_profile_recovery();
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(
            window.find("home-service-pending").label(),
            Some(zenclash_i18n::text("core_page.service.pending").as_str())
        );
        window.click("home-tun", cx);
        window.click("home-tun", cx);
        assert!(page.read(cx).home.action_error.is_none());
        assert_eq!(
            page.read(cx)
                .profile_service
                .service_state()
                .unwrap()
                .phase(),
            zenclash_core::ServicePhase::Idle
        );
        assert_eq!(
            page.read(cx).profile_service.pending_finalization(),
            Some(version)
        );
        let focus = page.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        for _ in 0..80 {
            if window.find("home-service-details").focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find("home-service-details").focused(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            navigations.get(),
            1,
            "details must dispatch the application navigation action"
        );
        assert_eq!(page.read(cx).page, Page::Tun);
        window.find("confirm-service-tun");
        page.update(cx, |page, cx| page.switch_to(Page::Home, cx));
        window.render_frame(cx);
        window.find("home-service-pending");
        assert_eq!(
            page.read(cx).profile_service.pending_finalization(),
            Some(version)
        );
        assert_eq!(fixture.core.generation(), version);
        assert_eq!(fs::read(&fixture.profile).unwrap(), bytes);
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn home_service_tun_immediate_rejection_survives_page_refresh(cx: &mut TestAppContext) {
    use gpui_kit::component::WindowExt;
    let fixture = Fixture::new();
    let generation = fixture.core.generation();
    let (window, page) = open_service_page(cx, &fixture, Page::Home);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("home-tun", cx);
        assert!(!window.has_active_dialog(cx));
        let expected = zenclash_i18n::text("core_page.service.unsupported");
        assert_eq!(
            page.read(cx).home.action_error.as_deref(),
            Some(expected.as_str())
        );
        page.update(cx, |page, cx| page.refresh(cx));
        window.render_frame(cx);
        assert_eq!(
            page.read(cx).home.action_error.as_deref(),
            Some(expected.as_str())
        );
        assert_eq!(fixture.core.generation(), generation);
        assert!(!page.read(cx).core_busy());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn home_service_tun_consent_cancel_preserves_local_owner_and_configuration(
    cx: &mut TestAppContext,
) {
    use gpui_kit::component::WindowExt;
    let fixture = Fixture::new();
    let [local, _unused] = owned_ui_children(&fixture);
    fixture
        .runtime
        .as_ref()
        .unwrap()
        .block_on(fixture.core.switch_to_process(local.clone()))
        .unwrap();
    let generation = fixture.core.generation();
    let pid = local.snapshot().pid;
    let configuration = fs::read(&local.launch_config().config_file).unwrap();
    let (window, page) = open_service_page(cx, &fixture, Page::Home);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let focus = page.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        for _ in 0..80 {
            if window.find("home-tun").focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find("home-tun").focused(), Some(true));
        page.update(cx, |page, _| {
            page.home.action_error = Some(zenclash_i18n::text("core_page.service.unsupported"));
        });
        window.click("home-tun", cx);
        assert!(page.read(cx).home.action_error.is_none());
        assert!(
            window.has_active_dialog(cx),
            "the real Home switch must request service consent before touching the local core"
        );
        window.click("home-tun", cx);
        assert!(window.has_active_dialog(cx));
        window.press("escape", cx);
        assert!(!window.has_active_dialog(cx));
        assert_eq!(window.find("home-tun").focused(), Some(true));
        window.press("space", cx);
        assert!(window.has_active_dialog(cx));
        window.press("escape", cx);
        assert_eq!(window.find("home-tun").checked(), Some(false));
        assert_eq!(fixture.core.generation(), generation);
        assert_eq!(local.snapshot().pid, pid);
        assert_eq!(
            fs::read(&local.launch_config().config_file).unwrap(),
            configuration
        );
        assert!(!page.read(cx).core_busy());
        assert!(
            !page
                .read(cx)
                .profile_service
                .service_state()
                .unwrap()
                .is_busy()
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn home_service_tun_confirmation_rejects_replaced_owner_without_local_grant(
    cx: &mut TestAppContext,
) {
    use gpui_kit::component::WindowExt;
    let fixture = Fixture::new();
    let [local, replacement] = owned_ui_children(&fixture);
    let runtime = fixture.runtime.as_ref().unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(local))
        .unwrap();
    let (window, page) = open_service_page(cx, &fixture, Page::Home);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("home-tun", cx);
        assert!(window.has_active_dialog(cx));
    })
    .unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(replacement.clone()))
        .unwrap();
    let generation = fixture.core.generation();
    let pid = replacement.snapshot().pid;
    let configuration = fs::read(&replacement.launch_config().config_file).unwrap();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(page.read(cx).core_session.generation(), generation);
        assert_eq!(
            page.read(cx).profile_service.session().generation(),
            generation
        );
        assert!(!page.read(cx).persistent_loading);
        assert!(!page.read(cx).core_busy());
        window.click("ok", cx);
        assert!(
            !window.has_active_dialog(cx),
            "the real confirm button must submit and close its dialog"
        );
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.core_busy() && page.home.action_error.is_some()
    });
    cx.update_window(window, |_, window, cx| {
        assert_eq!(
            page.read(cx).home.action_error.as_deref(),
            Some(zenclash_i18n::text("core_page.service.stale").as_str())
        );
        assert_eq!(fixture.core.generation(), generation);
        assert_eq!(replacement.snapshot().pid, pid);
        assert_eq!(
            fs::read(&replacement.launch_config().config_file).unwrap(),
            configuration
        );
        assert!(!window.has_active_dialog(cx));
        assert!(
            page.read(cx)
                .profile_service
                .pending_finalization()
                .is_none()
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn service_tun_consent_escape_keeps_the_real_local_owner_and_configuration(
    cx: &mut TestAppContext,
) {
    use gpui_kit::component::WindowExt;
    let fixture = Fixture::new();
    let [local, _unused] = owned_ui_children(&fixture);
    let runtime = fixture.runtime.as_ref().unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(local.clone()))
        .unwrap();
    let generation = fixture.core.generation();
    let configuration = fs::read(&local.launch_config().config_file).unwrap();
    let pid = local.snapshot().pid.unwrap();
    let (window, page) = open_service_tun(cx, &fixture);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let focus = page.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        for _ in 0..8 {
            if window.find("enable-service-tun").focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find("enable-service-tun").focused(), Some(true));
        window.click("enable-service-tun", cx);
        assert!(window.has_active_dialog(cx));
        window.press("escape", cx);
        assert!(!window.has_active_dialog(cx));
        assert_eq!(window.find("enable-service-tun").focused(), Some(true));
        window.press("enter", cx);
        assert!(
            window.has_active_dialog(cx),
            "keyboard activation must reach the same consent"
        );
        window.press("escape", cx);
        assert!(!window.has_active_dialog(cx));
        assert_eq!(local.snapshot().pid, Some(pid));
        assert_eq!(fixture.core.generation(), generation);
        assert_eq!(
            fs::read(&local.launch_config().config_file).unwrap(),
            configuration
        );
        assert!(
            !page
                .read(cx)
                .profile_service
                .service_state()
                .unwrap()
                .is_busy()
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn service_tun_confirmation_cannot_authorize_a_core_switched_while_dialog_open(
    cx: &mut TestAppContext,
) {
    use gpui_kit::component::WindowExt;
    let fixture = Fixture::new();
    let [local, replacement] = owned_ui_children(&fixture);
    let runtime = fixture.runtime.as_ref().unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(local))
        .unwrap();
    let (window, page) = open_service_tun(cx, &fixture);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("enable-service-tun", cx);
        assert!(window.has_active_dialog(cx));
    })
    .unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(replacement.clone()))
        .unwrap();
    let generation = fixture.core.generation();
    let replacement_pid = replacement.snapshot().pid.unwrap();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("ok", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.core_busy() && page.error.is_some());
    cx.update_window(window, |_, window, cx| {
        assert_eq!(
            page.read(cx).error.as_deref(),
            Some(zenclash_i18n::text("core_page.service.stale").as_str())
        );
        assert_eq!(fixture.core.generation(), generation);
        assert_eq!(replacement.snapshot().pid, Some(replacement_pid));
        assert!(!window.has_active_dialog(cx));
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn service_maintenance_dialogs_cancel_with_keyboard_and_restore_focus(cx: &mut TestAppContext) {
    use gpui_kit::component::WindowExt;

    let fixture = Fixture::new();
    let [local, _unused] = owned_ui_children(&fixture);
    fixture
        .runtime
        .as_ref()
        .unwrap()
        .block_on(fixture.core.switch_to_process(local.clone()))
        .unwrap();
    let pid = local.snapshot().pid;
    let generation = fixture.core.generation();
    let bytes = fs::read(&local.launch_config().config_file).unwrap();
    let (window, page) = open_service_tun(cx, &fixture);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        for id in ["repair-service", "uninstall-service"] {
            window.render_frame(cx);
            window.focus(&page.read(cx).focus_handle.clone(), cx);
            for _ in 0..12 {
                if window.find(id).focused() == Some(true) {
                    break;
                }
                window.press("tab", cx);
            }
            assert_eq!(window.find(id).focused(), Some(true));
            window.press("enter", cx);
            assert!(window.has_active_dialog(cx));
            window.press("escape", cx);
            assert!(!window.has_active_dialog(cx));
            assert_eq!(window.find(id).focused(), Some(true));
            window.click(id, cx);
            assert!(window.has_active_dialog(cx));
            window.press("escape", cx);
        }
        assert_eq!(local.snapshot().pid, pid);
        assert_eq!(fixture.core.generation(), generation);
        assert_eq!(fs::read(&local.launch_config().config_file).unwrap(), bytes);
        assert!(
            page.read(cx)
                .profile_service
                .service_maintenance_preparation()
                .is_none()
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn offline_service_maintenance_dialog_can_be_cancelled_and_missing_core_never_restarts(
    cx: &mut TestAppContext,
) {
    use gpui_kit::component::WindowExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let mut fixture = Fixture::new();
    fixture.status.stop();
    fixture.core = CoreSession::open_offline(
        CoreKind::Mihomo,
        fixture.root.join("offline-home"),
        Some(fixture.root.join("missing-selected-core")),
    )
    .unwrap();
    fixture.status = OperationalStatus::start(
        fixture.runtime.as_ref().unwrap().handle(),
        fixture.core.clone(),
        None,
        fixture.traffic.clone(),
        fixture.logs.clone(),
    );
    let (window, page) = open_service_tun(cx, &fixture);
    let restarts = Arc::new(AtomicUsize::new(0));
    let counter = restarts.clone();
    let _subscription = cx.update(|cx| {
        cx.subscribe(&page, move |_, _: &ServiceRepairRestartRequested, _| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
    });
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    let config_before = fs::read(&fixture.profile).unwrap();
    cx.update_window(window, |_, window, cx| {
        for id in ["repair-service", "uninstall-service"] {
            window.render_frame(cx);
            window.focus(&page.read(cx).focus_handle.clone(), cx);
            for _ in 0..12 {
                if window.find(id).focused() == Some(true) {
                    break;
                }
                window.press("tab", cx);
            }
            assert_eq!(window.find(id).focused(), Some(true));
            window.press("enter", cx);
            assert!(window.has_active_dialog(cx));
            window.press("escape", cx);
            assert!(!window.has_active_dialog(cx));
            assert_eq!(window.find(id).focused(), Some(true));
        }
        assert!(
            page.read(cx)
                .profile_service
                .service_maintenance_preparation()
                .is_none()
        );
        window.render_frame(cx);
        window.click("repair-service", cx);
        assert!(window.has_active_dialog(cx));
        window.click("ok", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.core_busy() && page.error.is_some());
    cx.update_window(window, |_, window, cx| {
        assert!(fixture.core.is_offline_recovery());
        assert!(!fixture.core.is_managed());
        assert_eq!(fixture.core.generation(), 0);
        assert_eq!(restarts.load(Ordering::SeqCst), 0);
        assert_eq!(fs::read(&fixture.profile).unwrap(), config_before);
        assert_eq!(
            page.read(cx)
                .profile_service
                .service_state()
                .unwrap()
                .phase(),
            zenclash_core::ServicePhase::Failed
        );
        assert!(
            page.read(cx)
                .profile_service
                .service_maintenance_preparation()
                .is_some()
        );
        assert!(!window.has_active_dialog(cx));
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn service_maintenance_confirmation_rejects_replaced_owner_before_native_work(
    cx: &mut TestAppContext,
) {
    use gpui_kit::component::WindowExt;

    let fixture = Fixture::new();
    let [local, replacement] = owned_ui_children(&fixture);
    let runtime = fixture.runtime.as_ref().unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(local))
        .unwrap();
    let (window, page) = open_service_tun(cx, &fixture);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    let preferences =
        zenclash_core::AppPreferencesStore::new(fixture.root.join("maintenance-preferences.json"));
    preferences
        .save(&zenclash_core::AppPreferences::default())
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, _| {
            page.preferences_store = Some(preferences.clone());
            page.preferences.system_proxy_enabled = true;
            page.preferences.language = zenclash_core::LanguagePreference::En;
        });
        window.render_frame(cx);
        window.click("uninstall-service", cx);
        assert!(window.has_active_dialog(cx));
    })
    .unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(replacement.clone()))
        .unwrap();
    let generation = fixture.core.generation();
    let pid = replacement.snapshot().pid;
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("ok", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.core_busy() && page.error.is_some());
    cx.update_window(window, |_, window, cx| {
        assert_eq!(
            page.read(cx).error.as_deref(),
            Some(zenclash_i18n::text("core_page.service.stale").as_str())
        );
        assert!(!page.read(cx).preferences.system_proxy_enabled);
        assert_eq!(
            page.read(cx).preferences.language,
            zenclash_core::LanguagePreference::En
        );
        assert!(!preferences.load().unwrap().system_proxy_enabled);
        assert_eq!(fixture.core.generation(), generation);
        assert_eq!(replacement.snapshot().pid, pid);
        assert!(
            page.read(cx)
                .profile_service
                .service_maintenance_preparation()
                .is_none()
        );
        assert!(!window.has_active_dialog(cx));
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn saving_yaml_preserves_edits_made_after_submission(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Override);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    let submitted = "mixed-port: 7890\nmode: global\n".to_owned();
    let newer = format!("{submitted}# next edit\n");
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            let id = page.profiles.catalog.active.clone().unwrap();
            let original = fs::read_to_string(&fixture.profile).unwrap();
            page.overrides.editor.profile_id = Some(id.clone());
            page.overrides.editor.original = Some(original.clone());
            page.overrides.editor.input.update(cx, |input, cx| {
                input.set_value(submitted.clone(), window, cx)
            });
            let token = page.begin_mutation(Page::Override).unwrap();
            page.overrides
                .editor
                .input
                .update(cx, |input, cx| input.set_value(newer.clone(), window, cx));
            page.complete_profile_yaml_save(token, id, original, submitted.clone(), None, cx);
            assert_eq!(
                page.overrides.editor.original.as_deref(),
                Some(submitted.as_str())
            );
            assert_eq!(page.overrides.editor.input.read(cx).value(), newer);
            assert!(page.overrides.editor.profile_id.is_some());
        });
        window.render_frame(cx);
        window.find("save-profile-yaml-edit");
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn saving_yaml_after_navigation_still_invalidates_business_state(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Override);
    fixture.settle(cx, &page, |page| {
        !page.persistent_loading && !page.config_inputs_loading
    });
    let events = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let recorded = events.clone();
    let _subscription = cx.update(|cx| {
        cx.subscribe(&page, move |_, _: &ProfileActivated, _| {
            recorded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
    });
    let submitted = "mixed-port: 7891\nmode: global\n".to_owned();
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            assert_eq!(page.config_inputs.core.mixed_port.read(cx).value(), "7890");
            let id = page.profiles.catalog.active.clone().unwrap();
            let original = fs::read_to_string(&fixture.profile).unwrap();
            page.overrides.editor.profile_id = Some(id.clone());
            page.overrides.editor.original = Some(original.clone());
            page.overrides.editor.input.update(cx, |input, cx| {
                input.set_value(submitted.clone(), window, cx)
            });
            let token = page.begin_mutation(Page::Override).unwrap();
            let generation = page.config_inputs_generation;
            fs::write(&fixture.profile, &submitted).unwrap();
            page.switch_to(Page::Dns, cx);
            page.notice = Some("other page notice".into());
            page.complete_profile_yaml_save(
                token,
                id,
                original,
                submitted.clone(),
                Some((fixture.profile.clone(), fixture.core.generation())),
                cx,
            );
            assert!(page.config_inputs_generation > generation);
            assert_eq!(page.notice.as_deref(), Some("other page notice"));
            assert!(!page.core_busy());
        });
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.config_inputs_loading);
    cx.update(|cx| {
        let page = page.read(cx);
        assert_eq!(page.effective_config["mixed-port"].as_u64(), Some(7891));
        assert_eq!(page.config_inputs.core.mixed_port.read(cx).value(), "7891");
    });
    assert_eq!(events.load(std::sync::atomic::Ordering::SeqCst), 1);
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn cancelling_a_network_probe_aborts_the_owned_task_and_rejects_old_publication(
    cx: &mut TestAppContext,
) {
    cx.foreground_executor().clone().block_test(async {
        let fixture = Fixture::new();
        let (window, page) = open(cx, &fixture, Page::Network);
        fixture.settle(cx, &page, |page| !page.persistent_loading);
        let pending = fixture
            .runtime
            .as_ref()
            .unwrap()
            .spawn(std::future::pending::<()>());
        cx.update_window(window, |_, window, cx| {
            page.update(cx, |page, cx| {
                page.network_probe.loading = true;
                let old_revision = page.network_probe.revision;
                let generation = page.core_session.generation();
                page.network_probe.task.replace(&pending);
                page.cancel_network_probe();
                page.network_probe.loading = true;
                let before = fixture.status.snapshot().path;
                page.complete_network_probe(
                    old_revision,
                    generation,
                    Err("old route".into()),
                    DiagnosticStepKind::NetworkDirect,
                    Err("old probe failed".into()),
                    cx,
                );
                assert_eq!(fixture.status.snapshot().path, before);
                assert!(page.network_probe.loading);
                assert!(page.network_probe.snapshot.is_none());
                page.complete_network_probe(
                    page.network_probe.revision,
                    generation.wrapping_add(1),
                    Err("obsolete core".into()),
                    DiagnosticStepKind::NetworkDirect,
                    Err("obsolete probe failed".into()),
                    cx,
                );
                assert_eq!(fixture.status.snapshot().path, before);
                assert!(!page.network_probe.loading);
                assert!(page.network_probe.snapshot.is_none());
                page.cancel_network_probe();
            });
            window.remove_window();
        })
        .unwrap();
        assert!(pending.await.unwrap_err().is_cancelled());
    });
}

#[gpui_kit::test]
fn network_probe_stops_when_blurred_or_hidden_during_a_mutation(cx: &mut TestAppContext) {
    cx.foreground_executor().clone().block_test(async {
        let fixture = Fixture::new();
        let _runtime_context = fixture.runtime.as_ref().unwrap().enter();
        let (window, page) = open(cx, &fixture, Page::Network);
        cx.update_window(window, |_, window, _| window.activate_window())
            .unwrap();
        fixture.settle(cx, &page, |page| {
            !page.persistent_loading && page.live_updates_enabled()
        });
        let blurred = fixture
            .runtime
            .as_ref()
            .unwrap()
            .spawn(std::future::pending::<()>());
        cx.update_window(window, |_, _, cx| {
            page.update(cx, |page, _| {
                page.network_probe.loading = true;
                page.network_probe.task.replace(&blurred);
            });
        })
        .unwrap();
        VisualTestContext::from_window(window, cx).deactivate_window();
        assert!(!cx.update(|cx| page.read(cx).network_probe.loading));
        assert!(blurred.await.unwrap_err().is_cancelled());
        cx.update_window(window, |_, window, _| window.activate_window())
            .unwrap();
        fixture.settle(cx, &page, |page| page.live_updates_enabled());
        let hidden = fixture
            .runtime
            .as_ref()
            .unwrap()
            .spawn(std::future::pending::<()>());
        cx.update_window(window, |_, _, cx| {
            page.update(cx, |page, cx| {
                let mutation = page.begin_mutation(Page::Network).unwrap();
                page.network_probe.loading = true;
                page.network_probe.task.replace(&hidden);
                page.set_window_visible(false, cx);
                assert!(!page.network_probe.loading);
                assert!(page.core_busy());
                page.finish_mutation(mutation);
            });
        })
        .unwrap();
        assert!(hidden.await.unwrap_err().is_cancelled());
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
        drop(page);
        cx.run_until_parked();
    });
}

#[gpui_kit::test]
fn connection_transport_buttons_filter_rows_with_pointer_and_keyboard(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Connections);
    fixture.settle(cx, &page, |page| !page.persistent_loading && !page.loading);
    cx.update_window(window, |_, _, cx| {
        page.update(cx, |page, cx| {
            page.invalidate_page_load();
            page.replace_page_data(
                page.page_task_token_for(Page::Connections),
                RuntimeData::Connections(Arc::new(zenclash_core::ConnectionsSnapshot {
                    connections: ["TCP", "UDP"]
                        .into_iter()
                        .map(|network| zenclash_core::Connection {
                            id: network.into(),
                            metadata: zenclash_core::ConnectionMetadata {
                                network: network.into(),
                                host: format!("{}.example", network.to_ascii_lowercase()),
                                ..Default::default()
                            },
                            ..Default::default()
                        })
                        .collect(),
                    ..Default::default()
                })),
                cx,
            );
        });
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.connections.projecting);
    let row = |id: &str| {
        gpui_kit::ElementId::from((
            gpui_kit::ElementId::from("connection-details"),
            id.to_owned(),
        ))
    };
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(row("TCP"));
        window.find(row("UDP"));
        window.click(("connection-transport", 1usize), cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.connections.projecting);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(row("TCP"));
        assert!(window.try_find(row("UDP")).is_none());
        let focus = page.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        for _ in 0..12 {
            if window.find(("connection-transport", 2usize)).focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(
            window.find(("connection-transport", 2usize)).focused(),
            Some(true)
        );
        window.press("enter", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.connections.projecting);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(row("UDP"));
        assert!(window.try_find(row("TCP")).is_none());
        window.click("pause-connections-display", cx);
        page.update(cx, |page, cx| {
            page.replace_page_data(
                page.page_task_token_for(Page::Connections),
                RuntimeData::Connections(Arc::new(zenclash_core::ConnectionsSnapshot {
                    connections: vec![zenclash_core::Connection {
                        id: "latest".into(),
                        metadata: zenclash_core::ConnectionMetadata {
                            network: "UDP".into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }],
                    ..Default::default()
                })),
                cx,
            );
        });
        window.render_frame(cx);
        window.find(row("UDP"));
        assert!(window.try_find(row("latest")).is_none());
        window.click(("connection-transport", 0usize), cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.connections.projecting);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(row("TCP"));
        window.find(row("UDP"));
        assert!(window.try_find(row("latest")).is_none());
        window.click("pause-connections-display", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.connections.projecting);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(row("latest"));
        assert!(window.try_find(row("UDP")).is_none());
        assert!(window.try_find(row("TCP")).is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn offline_settings_keep_local_controls_and_accessible_names(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Settings);
    fixture.settle(cx, &page, |page| !page.persistent_loading && !page.loading);
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.invalidate_page_load();
            page.data = RuntimeData::Settings {
                config: None,
                autostart: Err("autostart unavailable".into()),
            };
            let store =
                zenclash_core::AppPreferencesStore::new(fixture.root.join("preferences.json"));
            page.preferences = store
                .update(|preferences| preferences.traffic_history_enabled = false)
                .unwrap();
            page.preferences_store = Some(store);
            cx.notify();
        });
        window.render_frame(cx);
        window.find("language-en");
        window.find("theme-dark");
        assert_eq!(
            window.find("settings-traffic-history").label(),
            Some(zenclash_i18n::text("settings.traffic_history.title").as_str())
        );
        assert!(window.try_find("settings-ipv6").is_none());
        window.click("settings-autostart", cx);
        assert!(!page.read(cx).core_busy());
        assert!(matches!(
            &page.read(cx).data,
            RuntimeData::Settings { autostart: Err(error), .. } if error == "autostart unavailable"
        ));
        let focus = page.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        for _ in 0..40 {
            window.press("tab", cx);
            assert_ne!(window.find("settings-autostart").focused(), Some(true));
        }
    })
    .unwrap();
    // Default 1280 px app window minus the 224 px application sidebar.
    cx.simulate_window_resize(window, size(px(1056.), px(820.)));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        for id in [
            "theme-light",
            "theme-dark",
            "theme-system",
            "workspace-appearance",
        ] {
            let bounds = window.find(id).bounds();
            assert!(bounds.size.width > px(0.));
            assert!(
                bounds.origin.x >= px(0.) && bounds.right() <= px(1056.),
                "{id} exceeds the workspace width"
            );
        }
        for (card, ids) in [
            (
                "settings-appearance-card",
                ["language-zh-cn", "language-en"],
            ),
            ("settings-startup-card", ["tray-show", "tray-hide"]),
        ] {
            let card_bounds = window.find(card).bounds();
            for id in ids {
                let bounds = window.find(id).bounds();
                assert!(
                    bounds.origin.x >= card_bounds.origin.x
                        && bounds.right() <= card_bounds.right(),
                    "{id} is clipped by its settings card"
                );
            }
        }
        assert!(!window.find("settings-traffic-history").visible());
        window.click(("settings-section", 3usize), cx);
        assert!(window.simulate_next_frame(cx) > 0);
        window.render_frame(cx);
        assert!(page.read(cx).settings_navigation.scroll.offset().y < px(0.));
        assert!(window.find("settings-traffic-history").visible());
        window.click("settings-traffic-history", cx);
        window.click(("settings-section", 0usize), cx);
        window.render_frame(cx);
        assert_eq!(page.read(cx).settings_navigation.scroll.offset().y, px(0.));
    })
    .unwrap();
    fixture.settle(cx, &page, |page| page.preferences.traffic_history_enabled);
    assert!(
        zenclash_core::AppPreferencesStore::new(fixture.root.join("preferences.json"))
            .load()
            .unwrap()
            .traffic_history_enabled
    );
    let navigation_target = page.downgrade();
    cx.update(|cx| {
        // The fixture hosts RuntimePage alone; install its application's navigation receiver.
        cx.on_action(move |_: &crate::app::NavigateDns, cx| {
            navigation_target
                .update(cx, |page, cx| page.switch_to(Page::Dns, cx))
                .unwrap();
        });
    });
    cx.update_window(window, |_, window, cx| {
        window.click(
            (
                gpui_kit::ElementId::from("settings-tool"),
                Page::Dns.route(),
            ),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| page.read(cx).page), Page::Dns);
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn offline_pages_render_local_content_and_delete_a_disabled_override(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let source = fixture.root.join("local-override.yaml");
    fs::write(&source, "allow-lan: true\n").unwrap();
    let record = fixture.overrides.import_paths([source]).unwrap().remove(0);
    fixture.overrides.set_enabled(&record.id, false).unwrap();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.invalidate_page_load();
            page.data = RuntimeData::Empty;
            cx.notify();
        });
        window.render_frame(cx);
        let profile = fixture.profiles.load().unwrap().profiles.remove(0);
        assert_eq!(
            window
                .find(format!("delete-profile:{}", profile.id))
                .label(),
            Some(
                zenclash_i18n::text_with("profiles.actions.delete", &[("name", profile.name)])
                    .as_str()
            )
        );
        page.update(cx, |page, cx| page.switch_to(Page::Override, cx));
        window.render_frame(cx);
        window.find("preview-overrides");
        window.click(format!("override-delete:{}", record.id), cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| page.overrides.catalog.items.is_empty());
    assert!(fixture.overrides.load().unwrap().items.is_empty());
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.switch_to(Page::Traffic, cx);
            page.invalidate_page_load();
            page.data = RuntimeData::Empty;
            cx.notify();
        });
        window.render_frame(cx);
        window.find("request-clear-traffic");
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn keyboard_focus_and_delete_follow_the_same_override_after_reordering(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let mut records = Vec::new();
    for name in ["first", "second", "third"] {
        let source = fixture.root.join(format!("{name}.yaml"));
        fs::write(&source, "allow-lan: true\n").unwrap();
        let record = fixture.overrides.import_paths([source]).unwrap().remove(0);
        fixture.overrides.set_enabled(&record.id, false).unwrap();
        records.push(record);
    }
    let (window, page) = open(cx, &fixture, Page::Override);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    let target = format!("override-delete:{}", records[1].id);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let focus = page.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        for _ in 0..40 {
            if window.find(target.clone()).focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find(target.clone()).focused(), Some(true));
        fixture.overrides.move_to(&records[0].id, 2).unwrap();
        page.update(cx, |page, cx| {
            page.overrides.catalog = fixture.overrides.load().unwrap();
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(window.find(target.clone()).focused(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| page.overrides.catalog.items.len() == 2);
    let catalog = fixture.overrides.load().unwrap();
    assert!(
        !catalog
            .items
            .iter()
            .any(|record| record.id == records[1].id)
    );
    assert!(
        catalog
            .items
            .iter()
            .any(|record| record.id == records[0].id)
    );
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn actual_rule_filter_input_publishes_the_matching_projection(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Rules);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, _, cx| {
        page.update(cx, |page, cx| {
            page.ui_visibility = lifecycle::UiVisibility::new(true);
            page.invalidate_page_load();
            let data = RuntimeData::Rules(Arc::new(RuleCatalog {
                rules: vec![
                    zenclash_core::Rule {
                        payload: "alpha.example".into(),
                        ..Default::default()
                    },
                    zenclash_core::Rule {
                        payload: "beta.example".into(),
                        index: Some(1),
                        extra: Some(Default::default()),
                        ..Default::default()
                    },
                ],
            }));
            page.replace_page_data(page.page_task_token_for(Page::Rules), data, cx);
        });
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.rules.projecting);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(("input", page.read(cx).rules.filter.entity_id()), cx);
        window.input("beta", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.rules.projecting);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("rule-row", 1usize)).is_some());
        assert!(window.try_find(("rule-row", 0usize)).is_none());
        assert_eq!(
            window.find(("rule-enabled", 1usize)).label(),
            Some(
                zenclash_i18n::text_with(
                    "rules.row.enabled_named",
                    &[("index", "1".into()), ("payload", "beta.example".into())]
                )
                .as_str()
            )
        );
        window.press("secondary-a", cx);
        window.input("missing", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.rules.projecting);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("rule-row", 1usize)).is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn blurring_preserves_loaded_rules_until_the_window_is_hidden(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let _runtime_context = fixture.runtime.as_ref().unwrap().enter();
    let (window, page) = open(cx, &fixture, Page::Rules);
    cx.update_window(window, |_, window, _| window.activate_window())
        .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.persistent_loading && !page.loading && page.live_updates_enabled()
    });
    cx.update_window(window, |_, _, cx| {
        page.update(cx, |page, cx| {
            page.invalidate_page_load();
            page.replace_page_data(
                page.page_task_token_for(Page::Rules),
                RuntimeData::Rules(Arc::new(RuleCatalog {
                    rules: vec![zenclash_core::Rule {
                        payload: "loaded.example".into(),
                        ..Default::default()
                    }],
                })),
                cx,
            );
        });
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.rules.projecting);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(("rule-row", 0usize));
    })
    .unwrap();

    VisualTestContext::from_window(window, cx).deactivate_window();
    assert!(!cx.update(|cx| page.read(cx).live_updates_enabled()));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(("rule-row", 0usize));
        window.activate_window();
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        page.live_updates_enabled() && !page.loading
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(("input", page.read(cx).rules.filter.entity_id()), cx);
        window.input("loaded", cx);
    })
    .unwrap();
    assert!(cx.update(|cx| page.read(cx).rules.projecting));
    VisualTestContext::from_window(window, cx).deactivate_window();
    assert!(!cx.update(|cx| page.read(cx).rules.projecting));
    cx.update_window(window, |_, window, _| window.activate_window())
        .unwrap();
    fixture.settle(cx, &page, |page| {
        page.live_updates_enabled() && !page.rules.projecting
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(("rule-row", 0usize));
    })
    .unwrap();

    VisualTestContext::from_window(window, cx).deactivate_window();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(("rule-row", 0usize));
        page.update(cx, |page, cx| page.set_window_visible(false, cx));
        window.render_frame(cx);
        assert!(window.try_find(("rule-row", 0usize)).is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn blurring_preserves_loaded_yaml_preview_until_the_window_is_hidden(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let _runtime_context = fixture.runtime.as_ref().unwrap().enter();
    let (window, page) = open(cx, &fixture, Page::Override);
    cx.update_window(window, |_, window, _| window.activate_window())
        .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.persistent_loading && !page.loading && page.live_updates_enabled()
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("preview-overrides", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| page.overrides.preview.is_some());

    VisualTestContext::from_window(window, cx).deactivate_window();
    assert!(!cx.update(|cx| page.read(cx).live_updates_enabled()));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find("copy-source-config");
        window.find("copy-effective-config");
        page.update(cx, |page, cx| page.set_window_visible(false, cx));
        window.render_frame(cx);
        assert!(window.try_find("copy-source-config").is_none());
        assert!(window.try_find("copy-effective-config").is_none());
        assert!(page.read(cx).overrides.preview.is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_delayed_yaml_save_does_not_replace_a_newer_profile(cx: &mut TestAppContext) {
    exercise_delayed_yaml_save(cx, false);
}

#[gpui_kit::test]
fn a_delayed_yaml_save_recovers_the_current_path_after_a_newer_mode_change(
    cx: &mut TestAppContext,
) {
    exercise_delayed_yaml_save(cx, true);
}

fn exercise_delayed_yaml_save(cx: &mut TestAppContext, newer_mode: bool) {
    cx.foreground_executor().clone().block_test(async {
        use zenclash_core::EffectiveConfigIntent;
        let controller = ControllerFixture::new(false);
        let fixture = Fixture::with_controller(controller.url.clone());
        let (window, page) = open(cx, &fixture, Page::Override);
        fixture.settle(cx, &page, |page| !page.persistent_loading);
        let original = fs::read_to_string(&fixture.profile).unwrap();
        let saved = "mixed-port: 7891\nrules: [MATCH,DIRECT]\n".to_owned();
        let id = fixture.profiles.load().unwrap().active.unwrap();
        let token = cx
            .update_window(window, |_, window, cx| {
                page.update(cx, |page, cx| {
                    page.overrides.editor.profile_id = Some(id.clone());
                    page.overrides.editor.original = Some(original.clone());
                    page.overrides
                        .editor
                        .input
                        .update(cx, |input, cx| input.set_value(saved.clone(), window, cx));
                    page.begin_mutation(Page::Override).unwrap()
                })
            })
            .unwrap();
        fs::write(&fixture.profile, &saved).unwrap();
        let session = fixture.core.clone();
        let controlled = fixture.controlled.clone();
        let saved_path = fixture.profile.clone();
        let old_version = fixture
            .runtime
            .as_ref()
            .unwrap()
            .spawn(async move {
                session
                    .apply(
                        &controlled,
                        EffectiveConfigIntent::ActivateProfile {
                            profile: saved_path,
                            overrides: Vec::new(),
                        },
                    )
                    .await
                    .unwrap()
                    .generation
            })
            .await
            .unwrap();
        let second_source = fixture.root.join("second.yaml");
        fs::write(&second_source, "mixed-port: 7892\nrules: [MATCH,DIRECT]\n").unwrap();
        let second = fixture.profiles.import_local(second_source).unwrap();
        let second_path = fixture.profiles.activate(&second.id).unwrap();
        let session = fixture.core.clone();
        let controlled = fixture.controlled.clone();
        let applied_second = second_path.clone();
        fixture
            .runtime
            .as_ref()
            .unwrap()
            .spawn(async move {
                session
                    .apply(
                        &controlled,
                        EffectiveConfigIntent::ActivateProfile {
                            profile: applied_second,
                            overrides: Vec::new(),
                        },
                    )
                    .await
                    .unwrap()
            })
            .await
            .unwrap();
        assert!(fixture.core.generation() > old_version);
        if newer_mode {
            let session = fixture.core.clone();
            let controlled = fixture.controlled.clone();
            fixture
                .runtime
                .as_ref()
                .unwrap()
                .spawn(async move { session.set_mode(&controlled, "global").await.unwrap() })
                .await
                .unwrap();
        }
        let events = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let recorded = events.clone();
        let expected_path = second_path.clone();
        let expected_version = fixture.core.generation();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&page, move |_, event: &ProfileActivated, _| {
                assert_eq!(event.path, expected_path);
                assert_eq!(event.runtime_version, expected_version);
                recorded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
        });
        let still_second = cx
            .update_window(window, |_, _, cx| {
                page.update(cx, |page, cx| {
                    if !newer_mode {
                        page.profile_path = Some(second_path.clone());
                    }
                    page.complete_profile_yaml_save(
                        token,
                        id,
                        original,
                        saved,
                        Some((fixture.profile.clone(), old_version)),
                        cx,
                    );
                    // The completed disk write is still acknowledged, while runtime truth stays on B.
                    assert!(page.overrides.editor.original.is_none());
                    page.profile_path.as_ref() == Some(&second_path)
                })
            })
            .unwrap();
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
        assert!(
            still_second,
            "delayed A save replaced the newer active profile B"
        );
        assert_eq!(events.load(std::sync::atomic::Ordering::SeqCst), 1);
    });
}

struct ControllerFixture {
    url: String,
    stopped: Arc<std::sync::atomic::AtomicBool>,
    apply_failures: Arc<std::sync::atomic::AtomicUsize>,
    apply_requests: Arc<std::sync::atomic::AtomicUsize>,
    blocked_apply: Arc<std::sync::atomic::AtomicBool>,
    server: Option<std::thread::JoinHandle<()>>,
}

impl ControllerFixture {
    fn new(fail_first_apply: bool) -> Self {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server_stop = stopped.clone();
        let apply_failures = Arc::new(std::sync::atomic::AtomicUsize::new(usize::from(
            fail_first_apply,
        )));
        let apply_requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let blocked_apply = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let failures = apply_failures.clone();
        let requests = apply_requests.clone();
        let blocked = blocked_apply.clone();
        let server = std::thread::spawn(move || {
            let mut mode = "rule".to_owned();
            while !server_stop.load(std::sync::atomic::Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut request = Vec::new();
                        loop {
                            let mut byte = [0];
                            stream.read_exact(&mut byte).unwrap();
                            request.push(byte[0]);
                            assert!(request.len() <= 8192);
                            if request.ends_with(b"\r\n\r\n") {
                                break;
                            }
                        }
                        let headers = std::str::from_utf8(&request).unwrap();
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        let mut payload = vec![0; length];
                        stream.read_exact(&mut payload).unwrap();
                        let is_apply = request.starts_with(b"PUT /configs")
                            || request.starts_with(b"PATCH /configs");
                        if is_apply {
                            requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            while blocked.load(std::sync::atomic::Ordering::Acquire)
                                && !server_stop.load(std::sync::atomic::Ordering::Acquire)
                            {
                                std::thread::sleep(Duration::from_millis(2));
                            }
                            if failures
                                .fetch_update(
                                    std::sync::atomic::Ordering::SeqCst,
                                    std::sync::atomic::Ordering::SeqCst,
                                    |remaining| remaining.checked_sub(1),
                                )
                                .is_ok()
                            {
                                continue;
                            }
                        }
                        if request.starts_with(b"PATCH /configs") {
                            let patch: serde_json::Value =
                                serde_json::from_slice(&payload).unwrap();
                            mode = patch["mode"].as_str().unwrap().to_owned();
                        }
                        let response = if is_apply {
                            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned()
                        } else if request.starts_with(b"GET /configs ") {
                            let body =
                                serde_json::json!({"mixed-port":7892,"mode":mode}).to_string();
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                        } else if request.starts_with(b"GET /proxies ") {
                            let body = r#"{"proxies":{}}"#;
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                        } else if request.starts_with(b"GET /subscription ") {
                            let body = "mixed-port: 7891\nrules: [MATCH,REJECT]\n";
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/yaml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                        } else {
                            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
                        };
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                }
            }
        });
        Self {
            url: format!("http://{address}"),
            stopped,
            apply_failures,
            apply_requests,
            blocked_apply,
            server: Some(server),
        }
    }
}

fn retain_failed_backup_snapshot(fixture: &Fixture) {
    zenclash_core::AppPreferencesStore::new(fixture.root.join("preferences.json"))
        .save(&AppPreferences::default())
        .unwrap();
    YamlOverrideStore::new(fixture.root.join("yaml-overrides")).unwrap();
    let controlled = ControlledConfigStore::new(fixture.root.join("controlled-config"));
    controlled.materialize(&fixture.profile).unwrap();
    let manager = zenclash_core::BackupManager::new(&fixture.root);
    let archive = fixture.root.join("retry-fixture.zip");
    manager.export_to(&archive).unwrap();
    let mut transaction = manager
        .prepare_restore(&archive)
        .unwrap()
        .activate_for_session(&fixture.core)
        .unwrap();
    let snapshot = transaction.previous_runtime_snapshot().unwrap();
    transaction.rollback_in_place().unwrap();
    let controlled = transaction.authorize_controlled_store(controlled).unwrap();
    fixture.runtime.as_ref().unwrap().block_on(async {
        let admission = fixture.core.begin_backup_restore().await.unwrap();
        assert!(
            fixture
                .core
                .restore_backup_snapshot(&controlled, &snapshot, &admission)
                .await
                .is_err()
        );
    });
    assert!(fixture.core.pending_backup_restore().is_some());
}

#[gpui_kit::test]
fn backup_retry_button_preserves_failed_snapshot_then_refreshes_after_success(
    cx: &mut TestAppContext,
) {
    use std::sync::atomic::Ordering;
    let controller = ControllerFixture::new(true);
    let fixture = Fixture::with_controller(controller.url.clone());
    retain_failed_backup_snapshot(&fixture);
    controller.apply_failures.store(1, Ordering::SeqCst);
    let (window, page) = open(cx, &fixture, Page::Settings);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("backup-retry-runtime", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.mutation_busy(busy::MutationDomain::Backup) && page.error.is_some()
    });
    assert_eq!(
        fixture.core.pending_backup_restore(),
        Some(fixture.core.generation())
    );
    assert_eq!(controller.apply_requests.load(Ordering::SeqCst), 2);
    let saved = zenclash_core::AppPreferencesStore::new(fixture.root.join("preferences.json"))
        .update(|preferences| preferences.appearance = zenclash_core::AppearancePreference::Dark)
        .unwrap();
    let applied = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = applied.clone();
    let session = fixture.core.clone();
    let _subscription = cx.update(|cx| {
        cx.subscribe(&page, move |_, event: &ProfileActivated, _| {
            assert_eq!(event.runtime_version, session.generation());
            observed.fetch_add(1, Ordering::SeqCst);
        })
    });
    cx.update_window(window, |_, window, cx| {
        window.click("backup-retry-runtime", cx)
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.mutation_busy(busy::MutationDomain::Backup)
            && applied.load(Ordering::SeqCst) == 1
            && page.notice.is_some()
    });
    cx.update_window(window, |_, window, cx| {
        let page = page.read(cx);
        assert_eq!(page.preferences, saved);
        assert_eq!(page.profile_path.as_ref(), Some(&fixture.profile));
        assert!(page.error.is_none());
        assert!(fixture.core.pending_backup_restore().is_none());
        assert_eq!(controller.apply_requests.load(Ordering::SeqCst), 3);
        assert_eq!(applied.load(Ordering::SeqCst), 1);
        window.render_frame(cx);
        assert!(window.try_find("backup-retry-runtime").is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn late_backup_retry_refresh_cannot_replace_a_newer_profile_or_notice(cx: &mut TestAppContext) {
    use std::sync::atomic::Ordering;
    let controller = ControllerFixture::new(true);
    let fixture = Fixture::with_controller(controller.url.clone());
    retain_failed_backup_snapshot(&fixture);
    controller.blocked_apply.store(true, Ordering::Release);
    let (window, page) = open(cx, &fixture, Page::Settings);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("backup-retry-runtime", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |_| {
        controller.apply_requests.load(Ordering::SeqCst) == 2
    });
    controller.blocked_apply.store(false, Ordering::Release);
    let candidate = fixture.root.join("newer-profile.yaml");
    fs::write(&candidate, "mixed-port: 7999\nrules: [MATCH,DIRECT]\n").unwrap();
    fixture.runtime.as_ref().unwrap().block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while fixture.core.pending_backup_restore().is_some() {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        fixture
            .core
            .apply(
                &fixture.controlled,
                zenclash_core::EffectiveConfigIntent::ActivateProfile {
                    profile: candidate.clone(),
                    overrides: vec![],
                },
            )
            .await
            .unwrap();
    });
    cx.update_window(window, |_, _, cx| {
        page.update(cx, |page, cx| {
            page.synchronize_committed_profile(cx);
            page.notice = Some("newer operation notice".into());
        });
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.mutation_busy(busy::MutationDomain::Backup)
    });
    cx.update_window(window, |_, window, cx| {
        let page = page.read(cx);
        assert_eq!(page.profile_path.as_ref(), Some(&candidate));
        assert_eq!(page.notice.as_deref(), Some("newer operation notice"));
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn backup_retry_ignores_duplicate_click_and_synchronizes_after_navigation(cx: &mut TestAppContext) {
    use std::sync::atomic::Ordering;
    let controller = ControllerFixture::new(true);
    let fixture = Fixture::with_controller(controller.url.clone());
    retain_failed_backup_snapshot(&fixture);
    controller.blocked_apply.store(true, Ordering::Release);
    let (window, page) = open(cx, &fixture, Page::Settings);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("backup-retry-runtime", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |_| {
        controller.apply_requests.load(Ordering::SeqCst) == 2
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            page.read(cx)
                .mutations
                .active(busy::MutationDomain::BackupRecovery)
        );
        window.click("backup-retry-runtime", cx);
        page.update(cx, |page, cx| {
            page.switch_to(Page::Dns, cx);
            page.notice = Some("new page notice".into());
        });
    })
    .unwrap();
    controller.blocked_apply.store(false, Ordering::Release);
    fixture.settle(cx, &page, |page| {
        !page.mutation_busy(busy::MutationDomain::Backup)
    });
    cx.update_window(window, |_, window, cx| {
        let page = page.read(cx);
        assert!(fixture.core.pending_backup_restore().is_none());
        assert_eq!(controller.apply_requests.load(Ordering::SeqCst), 2);
        assert_eq!(page.profile_path.as_ref(), Some(&fixture.profile));
        assert_eq!(page.notice.as_deref(), Some("new page notice"));
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn backup_retry_refresh_failure_keeps_the_accepted_runtime_and_reports_refresh_failure(
    cx: &mut TestAppContext,
) {
    let controller = ControllerFixture::new(true);
    let fixture = Fixture::with_controller(controller.url.clone());
    retain_failed_backup_snapshot(&fixture);
    let (window, page) = open(cx, &fixture, Page::Settings);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    fs::write(
        fixture.root.join("preferences.json"),
        b"invalid persisted JSON",
    )
    .unwrap();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("backup-retry-runtime", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.mutation_busy(busy::MutationDomain::Backup) && page.error.is_some()
    });
    cx.update_window(window, |_, window, cx| {
        let page = page.read(cx);
        assert!(fixture.core.pending_backup_restore().is_none());
        let localized = zenclash_i18n::text("backup.errors.retry_refresh_failed");
        assert!(
            page.error
                .as_ref()
                .unwrap()
                .starts_with(localized.split("%{error}").next().unwrap())
        );
        assert_eq!(page.profile_path.as_ref(), Some(&fixture.profile));
        window.render_frame(cx);
        assert!(window.try_find("backup-retry-runtime").is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn backup_retry_is_reachable_by_tab_and_activates_once_with_enter(cx: &mut TestAppContext) {
    use std::sync::atomic::Ordering;
    let controller = ControllerFixture::new(true);
    let fixture = Fixture::with_controller(controller.url.clone());
    retain_failed_backup_snapshot(&fixture);
    let (window, page) = open(cx, &fixture, Page::Settings);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        // Start contextual keyboard traversal in RuntimePage's retained focus region.
        let focus = page.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        for _ in 0..80 {
            window.press("tab", cx);
            if window.find("backup-retry-runtime").focused() == Some(true) {
                break;
            }
        }
        assert_eq!(window.find("backup-retry-runtime").focused(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.mutation_busy(busy::MutationDomain::Backup) && page.notice.is_some()
    });
    assert!(fixture.core.pending_backup_restore().is_none());
    assert_eq!(controller.apply_requests.load(Ordering::SeqCst), 2);
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn cancelled_backup_retry_waiter_still_records_the_accepted_shared_business_result(
    cx: &mut TestAppContext,
) {
    use std::sync::atomic::Ordering;
    let controller = ControllerFixture::new(true);
    let fixture = Fixture::with_controller(controller.url.clone());
    retain_failed_backup_snapshot(&fixture);
    let service = crate::ProfileService::new(fixture.core.clone(), None);
    let record = fixture
        .profiles
        .load()
        .unwrap()
        .active_profile()
        .unwrap()
        .clone();
    let version = fixture.core.generation();
    service
        .publish_test_outcome(
            zenclash_core::ProfileApplyOutcome::CommittedButRuntimeUnknown {
                source_version: (&record).into(),
                profile: record,
                path: fixture.profile.clone(),
                cause: zenclash_core::ProfileApplicationError::Task(
                    "fixture pending result".into(),
                ),
                runtime_version: version,
            },
        )
        .unwrap();
    controller.blocked_apply.store(true, Ordering::Release);
    let completion = service.clone();
    let task = fixture
        .runtime
        .as_ref()
        .unwrap()
        .spawn(async move { completion.retry_backup_restore().await });
    fixture.runtime.as_ref().unwrap().block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while controller.apply_requests.load(Ordering::SeqCst) != 2 {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        controller.blocked_apply.store(false, Ordering::Release);
        tokio::time::timeout(Duration::from_secs(5), async {
            while service.pending_finalization().is_some()
                || fixture.core.pending_backup_restore().is_some()
            {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
    });
    assert!(fixture.core.generation() > version);
    assert_eq!(controller.apply_requests.load(Ordering::SeqCst), 2);
    cx.run_until_parked();
}

impl Drop for ControllerFixture {
    fn drop(&mut self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        self.server.take().unwrap().join().unwrap();
    }
}

#[gpui_kit::test]
fn delayed_backup_completion_synchronizes_current_business_state_after_navigation(
    cx: &mut TestAppContext,
) {
    cx.foreground_executor().clone().block_test(async {
        use settings::backup::RestoreOutcome;

        let controller = ControllerFixture::new(false);
        let fixture = Fixture::with_controller(controller.url.clone());
        let preferences =
            zenclash_core::AppPreferencesStore::new(fixture.root.join("preferences.json"));
        preferences.save(&AppPreferences::default()).unwrap();
        let backup_controlled = ControlledConfigStore::new(fixture.root.join("controlled-config"));
        let backup_overrides = YamlOverrideStore::new(fixture.root.join("yaml-overrides")).unwrap();
        let outcome = RestoreOutcome {
            data_root: fixture.root.clone(),
            preferences: AppPreferences::default(),
            catalog: fixture.profiles.load().unwrap(),
            profile_store: fixture.profiles.clone(),
            controlled_store: backup_controlled.clone(),
            controlled_config: backup_controlled.load_json().unwrap(),
            override_store: backup_overrides.clone(),
            override_catalog: backup_overrides.load().unwrap(),
            runtime_version: 0,
            page_data: RuntimeData::Empty,
            file_count: 1,
            payload_bytes: 1,
            cleanup_warning: None,
        };
        let (window, page) = open(cx, &fixture, Page::Settings);
        fixture.settle(cx, &page, |page| !page.persistent_loading);

        let candidate = fixture.root.join("new-current.yaml");
        fs::write(&candidate, "mixed-port: 7991\nrules: [MATCH,DIRECT]\n").unwrap();
        let session = fixture.core.clone();
        let controlled = fixture.controlled.clone();
        let next_profile = candidate.clone();
        let applied = fixture
            .runtime
            .as_ref()
            .unwrap()
            .spawn(async move {
                session
                    .apply(
                        &controlled,
                        zenclash_core::EffectiveConfigIntent::ActivateProfile {
                            profile: next_profile,
                            overrides: Vec::new(),
                        },
                    )
                    .await
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(applied.generation, 1);
        let saved = preferences
            .update(|preferences| {
                preferences.appearance = zenclash_core::AppearancePreference::Dark;
                preferences.traffic_tray_visible = true;
            })
            .unwrap();
        let overlay = fixture.root.join("later-overlay.yaml");
        fs::write(&overlay, "mode: global\n").unwrap();
        let record = backup_overrides.import_paths([overlay]).unwrap().remove(0);
        backup_overrides.set_enabled(&record.id, false).unwrap();

        let preference_events = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let recorded_preferences = preference_events.clone();
        let _preferences_subscription = cx.update(|cx| {
            cx.subscribe(&page, move |_, event: &PreferencesRestored, _| {
                assert_eq!(event.preferences, saved);
                assert_eq!(event.scope, PreferenceScope::Restore);
                recorded_preferences.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
        });
        let paths = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded_paths = paths.clone();
        let _profile_subscription = cx.update(|cx| {
            cx.subscribe(&page, move |_, event: &ProfileActivated, _| {
                recorded_paths
                    .lock()
                    .unwrap()
                    .push((event.path.clone(), event.runtime_version));
            })
        });
        cx.update_window(window, |_, _, cx| {
            page.update(cx, |page, cx| {
                let token = page.page_task_token_for(Page::Settings);
                page.switch_to(Page::Dns, cx);
                page.notice = Some("current page notice".into());
                page.apply_restore_outcome(outcome, token, cx);
            });
        })
        .unwrap();
        fixture.settle(cx, &page, |page| {
            page.preferences.appearance == zenclash_core::AppearancePreference::Dark
                && page.overrides.catalog.items.len() == 1
        });
        cx.update(|cx| {
            let page = page.read(cx);
            assert_eq!(page.profile_path.as_ref(), Some(&candidate));
            assert!(!page.overrides.catalog.items[0].enabled);
            assert!(page.preferences.traffic_tray_visible);
            assert_eq!(page.notice.as_deref(), Some("current page notice"));
        });
        assert_eq!(
            preference_events.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(*paths.lock().unwrap(), vec![(candidate, 1)]);
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
    });
}

#[gpui_kit::test]
fn a_saved_profile_confirmation_uses_a_separate_button_and_preserves_data_on_failure(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    let record = fixture
        .profiles
        .load()
        .unwrap()
        .active_profile()
        .unwrap()
        .clone();
    let before = fs::read(&fixture.profile).unwrap();
    let version = fixture.core.generation();
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            // Typed committed outcome fixture; this is UI recovery coverage, not native service validation.
            page.profile_service
                .publish_test_outcome(
                    zenclash_core::ProfileApplyOutcome::CommittedButRuntimeUnknown {
                        source_version: (&record).into(),
                        profile: record.clone(),
                        path: fixture.profile.clone(),
                        cause: zenclash_core::ProfileApplicationError::Task(
                            "commit reply lost".into(),
                        ),
                        runtime_version: version,
                    },
                )
                .unwrap();
            page.synchronize_profile_recovery();
            cx.notify();
        });
        window.render_frame(cx);
        assert!(window.try_find("reapply-profile-recovery").is_none());
        window.find("confirm-service-profile");
        window.click("confirm-service-profile", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.core_busy() && page.error.is_some());
    cx.update_window(window, |_, window, cx| {
        let page = page.read(cx);
        assert_eq!(page.profiles.pending_finalization, Some(version));
        assert!(page.profiles.recovery.is_none());
        assert_eq!(page.profile_path.as_ref(), Some(&fixture.profile));
        assert_eq!(fixture.core.generation(), version);
        assert_eq!(
            fixture.profiles.active_path().unwrap(),
            Some(fixture.profile.clone())
        );
        assert_eq!(fs::read(&fixture.profile).unwrap(), before);
        window.render_frame(cx);
        window.find("confirm-service-profile");
        assert!(window.try_find("reapply-profile-recovery").is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_committed_tray_receipt_keeps_the_pending_service_warning(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    let profile = fixture
        .profiles
        .load()
        .unwrap()
        .active_profile()
        .unwrap()
        .clone();
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            let receipt = page
                .profile_service
                .publish_test_outcome(
                    zenclash_core::ProfileApplyOutcome::CommittedButRuntimeUnknown {
                        source_version: (&profile).into(),
                        profile,
                        path: fixture.profile.clone(),
                        cause: zenclash_core::ProfileApplicationError::Task(
                            "commit reply lost".into(),
                        ),
                        runtime_version: fixture.core.generation(),
                    },
                )
                .unwrap();
            let warning = receipt.warning().unwrap();
            page.profile_activated_from_tray(receipt, cx);
            assert_eq!(page.notice.as_deref(), Some(warning.as_str()));
            assert_eq!(
                page.profiles.pending_finalization,
                Some(fixture.core.generation())
            );
            assert_eq!(page.profile_path.as_ref(), Some(&fixture.profile));
        });
        window.render_frame(cx);
        window.find("confirm-service-profile");
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_recovery_button_reapplies_the_recorded_profile_and_clears_uncertainty(
    cx: &mut TestAppContext,
) {
    exercise_recovery_completion(cx, false);
}

#[gpui_kit::test]
fn a_manual_reload_clears_the_recovery_card_after_runtime_acceptance(cx: &mut TestAppContext) {
    exercise_recovery_completion(cx, true);
}

#[gpui_kit::test]
fn adding_a_remote_profile_with_a_lost_response_displays_the_recovery_card(
    cx: &mut TestAppContext,
) {
    let controller = ControllerFixture::new(true);
    let fixture = Fixture::with_controller(controller.url.clone());
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.profiles.forms.adding_subscription = true;
            page.profiles.forms.subscription_route = zenclash_core::RemoteProfileRoute::Direct;
            page.profiles
                .forms
                .subscription_name
                .update(cx, |input, cx| {
                    input.set_value("remote fixture", window, cx)
                });
            page.profiles
                .forms
                .subscription_url
                .update(cx, |input, cx| {
                    input.set_value(format!("{}/subscription", controller.url), window, cx)
                });
        });
        window.render_frame(cx);
        window.click("download-subscription", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.core_busy() && page.profiles.forms.subscription_error.is_some()
    });
    cx.update_window(window, |_, window, cx| {
        assert!(
            page.read(cx).profiles.recovery.is_some(),
            "a remote runtime uncertainty was hidden from the profile page"
        );
        window.render_frame(cx);
        window.find("reapply-profile-recovery");
        window.remove_window();
    })
    .unwrap();
}

fn exercise_recovery_completion(cx: &mut TestAppContext, manual_reload: bool) {
    cx.foreground_executor().clone().block_test(async {
        let controller = ControllerFixture::new(true);
        let fixture = Fixture::with_controller(controller.url.clone());
        let source = fixture.root.join("candidate.yaml");
        fs::write(&source, "mixed-port: 7891\nrules: [MATCH,REJECT]\n").unwrap();
        let candidate = fixture.profiles.import_local(source).unwrap();
        let (window, page) = open(cx, &fixture, Page::Profiles);
        fixture.settle(cx, &page, |page| !page.persistent_loading && !page.loading);
        let service = cx.update(|cx| page.read(cx).profile_service.clone());
        let store = fixture.profiles.clone();
        let controlled = fixture.controlled.clone();
        let failure = fixture
            .runtime
            .as_ref()
            .unwrap()
            .spawn(async move {
                service
                    .activate(store, controlled, candidate.id)
                    .await
                    .err()
                    .unwrap()
                    .to_string()
            })
            .await
            .unwrap();
        let attempted_version = fixture.core.generation();
        cx.update_window(window, |_, window, cx| {
            page.update(cx, |page, cx| page.report_tray_profile_error(&failure, cx));
            assert!(page.read(cx).profiles.recovery.is_some());
            window.render_frame(cx);
            window.find("reapply-profile-recovery");
            if manual_reload {
                window.scroll(
                    "reapply-profile-recovery",
                    gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-1000.))),
                    cx,
                );
                window.click("reload-profile", cx);
            } else {
                window.click("reapply-profile-recovery", cx);
            }
        })
        .unwrap();
        fixture.settle(cx, &page, |page| {
            !page.core_busy() && page.core_session.generation() > attempted_version
        });
        cx.update(|cx| {
            assert_eq!(page.read(cx).profile_path.as_ref(), Some(&fixture.profile));
            assert!(
                page.read(cx).profiles.recovery.is_none(),
                "an accepted reload left a stale recovery card"
            );
        });
        assert_eq!(
            fixture.core.committed_profile_snapshot().profile_path,
            Some(fixture.profile.clone())
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("reapply-profile-recovery").is_none());
            window.remove_window();
        })
        .unwrap();
    });
}

#[gpui_kit::test]
fn license_and_fork_notices_are_readable_with_an_offline_core(cx: &mut TestAppContext) {
    use gpui_kit::component::WindowExt as _;
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Settings);
    fixture.settle(cx, &page, |page| !page.persistent_loading && !page.loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-license-notices", cx);
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        assert!(window.find("license-notices-scroll").visible());
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));
        assert!(!page.read(cx).core_busy());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn accepted_sidecar_does_not_repeat_local_choice_but_keeps_service_repair_reachable(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let [local, _unused] = owned_ui_children(&fixture);
    fixture
        .runtime
        .as_ref()
        .unwrap()
        .block_on(fixture.core.switch_to_process(local))
        .unwrap();
    fixture
        .core
        .record_startup_service_health(zenclash_core::ServiceHealthKind::Unavailable(
            "accepted local fallback".into(),
        ));
    let (window, page) = open_service_tun(cx, &fixture);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("continue-local").is_none());
        assert!(fixture.core.run_state().sidecar_allowed);
        window.focus(&page.read(cx).focus_handle.clone(), cx);
        for _ in 0..12 {
            if window.find("repair-service").focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find("repair-service").focused(), Some(true));
        window.remove_window();
    })
    .unwrap();
}
