use std::{
    fs,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{TestAppContext, VisualTestContext, size};

use super::*;

struct Fixture {
    root: PathBuf,
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
    fn new() -> Self {
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
        let client = MihomoClient::new(endpoint.clone()).unwrap();
        let core = CoreSession::open_with_config(
            CoreKind::Mihomo,
            client,
            None,
            Some(profile.clone()),
            Vec::new(),
        );
        let traffic = TrafficMonitor::start(runtime.handle(), endpoint.clone());
        let logs = LogMonitor::start(runtime.handle(), endpoint, MihomoLogLevel::Info);
        let status = OperationalStatus::start(
            runtime.handle(),
            core.clone(),
            None,
            None,
            traffic.clone(),
            logs.clone(),
        );
        Self {
            root,
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

    fn services(&self) -> RuntimePageServices {
        RuntimePageServices {
            profile_store: Some(self.profiles.clone()),
            override_store: Some(self.overrides.clone()),
            core_kind: CoreKind::Mihomo,
            core_session: self.core.clone(),
            client: self.core.client().clone(),
            runtime: self.runtime.as_ref().unwrap().handle().clone(),
            traffic_monitor: self.traffic.clone(),
            log_monitor: self.logs.clone(),
            operational_status: self.status.clone(),
            traffic_capture: TrafficCaptureSession::new(
                self.core.clone(),
                self.controlled.clone(),
                None,
                None,
                Some(self.profile.clone()),
            ),
            process: None,
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

    async fn settle(
        &self,
        cx: &mut TestAppContext,
        page: &Entity<RuntimePage>,
        predicate: impl Fn(&RuntimePage) -> bool,
    ) {
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
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.status.stop();
        self.runtime
            .take()
            .unwrap()
            .shutdown_timeout(Duration::from_secs(1));
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn open(
    cx: &mut TestAppContext,
    fixture: &Fixture,
    initial: Page,
) -> (AnyWindowHandle, Entity<RuntimePage>) {
    cx.executor().allow_parking();
    cx.update(gpui_kit::init);
    let mut page = None;
    let handle = cx.open_window(size(px(1200.), px(1000.)), |window, cx| {
        let view = cx.new(|cx| RuntimePage::new(initial, fixture.services(), window, cx));
        page = Some(view.clone());
        Root::new(view, window, cx)
    });
    (handle.into(), page.unwrap())
}

#[gpui_kit::test]
async fn offline_pages_render_local_content_and_delete_a_disabled_override(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let source = fixture.root.join("local-override.yaml");
    fs::write(&source, "allow-lan: true\n").unwrap();
    let record = fixture.overrides.import_paths([source]).unwrap().remove(0);
    fixture.overrides.set_enabled(&record.id, false).unwrap();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture
        .settle(cx, &page, |page| !page.persistent_loading)
        .await;
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.invalidate_page_load();
            page.data = RuntimeData::Empty;
            cx.notify();
        });
        window.render_frame(cx);
        window.find(format!(
            "delete-profile:{}",
            fixture.profiles.load().unwrap().profiles[0].id
        ));
        page.update(cx, |page, cx| page.switch_to(Page::Override, cx));
        window.render_frame(cx);
        window.find("preview-overrides");
        window.click(format!("override-delete:{}", record.id), cx);
    })
    .unwrap();
    fixture
        .settle(cx, &page, |page| page.overrides.catalog.items.is_empty())
        .await;
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
async fn keyboard_focus_and_delete_follow_the_same_override_after_reordering(
    cx: &mut TestAppContext,
) {
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
    fixture
        .settle(cx, &page, |page| !page.persistent_loading)
        .await;
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
    fixture
        .settle(cx, &page, |page| page.overrides.catalog.items.len() == 2)
        .await;
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
async fn actual_rule_filter_input_publishes_the_matching_projection(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Rules);
    fixture
        .settle(cx, &page, |page| !page.persistent_loading)
        .await;
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
                        ..Default::default()
                    },
                ],
            }));
            page.replace_page_data(page.page_task_token_for(Page::Rules), data, cx);
        });
    })
    .unwrap();
    fixture
        .settle(cx, &page, |page| !page.rules.projecting)
        .await;
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(("input", page.read(cx).rules.filter.entity_id()), cx);
        window.input("beta", cx);
    })
    .unwrap();
    fixture
        .settle(cx, &page, |page| !page.rules.projecting)
        .await;
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("rule-row", 1usize)).is_some());
        assert!(window.try_find(("rule-row", 0usize)).is_none());
        window.press("secondary-a", cx);
        window.input("missing", cx);
    })
    .unwrap();
    fixture
        .settle(cx, &page, |page| !page.rules.projecting)
        .await;
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("rule-row", 1usize)).is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
async fn blurring_preserves_loaded_rules_until_the_window_is_hidden(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let _runtime_context = fixture.runtime.as_ref().unwrap().enter();
    let (window, page) = open(cx, &fixture, Page::Rules);
    cx.update_window(window, |_, window, _| window.activate_window())
        .unwrap();
    fixture
        .settle(cx, &page, |page| {
            !page.persistent_loading && !page.loading && page.live_updates_enabled()
        })
        .await;
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
    fixture
        .settle(cx, &page, |page| !page.rules.projecting)
        .await;
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
    fixture
        .settle(cx, &page, |page| {
            page.live_updates_enabled() && !page.loading
        })
        .await;
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
    fixture
        .settle(cx, &page, |page| {
            page.live_updates_enabled() && !page.rules.projecting
        })
        .await;
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
async fn blurring_preserves_loaded_yaml_preview_until_the_window_is_hidden(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let _runtime_context = fixture.runtime.as_ref().unwrap().enter();
    let (window, page) = open(cx, &fixture, Page::Override);
    cx.update_window(window, |_, window, _| window.activate_window())
        .unwrap();
    fixture
        .settle(cx, &page, |page| {
            !page.persistent_loading && !page.loading && page.live_updates_enabled()
        })
        .await;
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("preview-overrides", cx);
    })
    .unwrap();
    fixture
        .settle(cx, &page, |page| page.overrides.preview.is_some())
        .await;

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
