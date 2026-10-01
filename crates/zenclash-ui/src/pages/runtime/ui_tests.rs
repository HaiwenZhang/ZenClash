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
async fn saving_yaml_preserves_edits_made_after_submission(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Override);
    fixture
        .settle(cx, &page, |page| !page.persistent_loading)
        .await;
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
async fn saving_yaml_after_navigation_still_invalidates_business_state(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Override);
    fixture
        .settle(cx, &page, |page| {
            !page.persistent_loading && !page.config_inputs_loading
        })
        .await;
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
    fixture
        .settle(cx, &page, |page| !page.config_inputs_loading)
        .await;
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
async fn cancelling_a_network_probe_aborts_the_owned_task_and_rejects_old_publication(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Network);
    fixture
        .settle(cx, &page, |page| !page.persistent_loading)
        .await;
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
}

#[gpui_kit::test]
async fn network_probe_stops_when_blurred_or_hidden_during_a_mutation(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let _runtime_context = fixture.runtime.as_ref().unwrap().enter();
    let (window, page) = open(cx, &fixture, Page::Network);
    cx.update_window(window, |_, window, _| window.activate_window())
        .unwrap();
    fixture
        .settle(cx, &page, |page| {
            !page.persistent_loading && page.live_updates_enabled()
        })
        .await;
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
    fixture
        .settle(cx, &page, |page| page.live_updates_enabled())
        .await;
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
}

#[gpui_kit::test]
async fn offline_settings_keep_local_controls_and_accessible_names(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Settings);
    fixture
        .settle(cx, &page, |page| !page.persistent_loading && !page.loading)
        .await;
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
        window.click("settings-traffic-history", cx);
    })
    .unwrap();
    fixture
        .settle(cx, &page, |page| page.preferences.traffic_history_enabled)
        .await;
    assert!(
        zenclash_core::AppPreferencesStore::new(fixture.root.join("preferences.json"))
            .load()
            .unwrap()
            .traffic_history_enabled
    );
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
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

#[gpui_kit::test]
async fn a_delayed_yaml_save_does_not_replace_a_newer_profile(cx: &mut TestAppContext) {
    exercise_delayed_yaml_save(cx, false).await;
}

#[gpui_kit::test]
async fn a_delayed_yaml_save_recovers_the_current_path_after_a_newer_mode_change(
    cx: &mut TestAppContext,
) {
    exercise_delayed_yaml_save(cx, true).await;
}

async fn exercise_delayed_yaml_save(cx: &mut TestAppContext, newer_mode: bool) {
    use zenclash_core::EffectiveConfigIntent;
    let controller = ControllerFixture::new(false);
    let fixture = Fixture::with_controller(controller.url.clone());
    let (window, page) = open(cx, &fixture, Page::Override);
    fixture
        .settle(cx, &page, |page| !page.persistent_loading)
        .await;
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
}

struct ControllerFixture {
    url: String,
    stopped: Arc<std::sync::atomic::AtomicBool>,
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
        let server = std::thread::spawn(move || {
            let mut should_fail = fail_first_apply;
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
                        if is_apply && should_fail {
                            should_fail = false;
                            continue;
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
            server: Some(server),
        }
    }
}

impl Drop for ControllerFixture {
    fn drop(&mut self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        self.server.take().unwrap().join().unwrap();
    }
}

#[gpui_kit::test]
async fn delayed_backup_completion_synchronizes_current_business_state_after_navigation(
    cx: &mut TestAppContext,
) {
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
    fixture
        .settle(cx, &page, |page| !page.persistent_loading)
        .await;

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
    fixture
        .settle(cx, &page, |page| {
            page.preferences.appearance == zenclash_core::AppearancePreference::Dark
                && page.overrides.catalog.items.len() == 1
        })
        .await;
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
}

#[gpui_kit::test]
async fn the_recovery_button_reapplies_the_recorded_profile_and_clears_uncertainty(
    cx: &mut TestAppContext,
) {
    exercise_recovery_completion(cx, false).await;
}

#[gpui_kit::test]
async fn a_manual_reload_clears_the_recovery_card_after_runtime_acceptance(
    cx: &mut TestAppContext,
) {
    exercise_recovery_completion(cx, true).await;
}

#[gpui_kit::test]
async fn adding_a_remote_profile_with_a_lost_response_displays_the_recovery_card(
    cx: &mut TestAppContext,
) {
    let controller = ControllerFixture::new(true);
    let fixture = Fixture::with_controller(controller.url.clone());
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture
        .settle(cx, &page, |page| !page.persistent_loading)
        .await;
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
    fixture
        .settle(cx, &page, |page| {
            !page.core_busy() && page.profiles.forms.subscription_error.is_some()
        })
        .await;
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

async fn exercise_recovery_completion(cx: &mut TestAppContext, manual_reload: bool) {
    let controller = ControllerFixture::new(true);
    let fixture = Fixture::with_controller(controller.url.clone());
    let source = fixture.root.join("candidate.yaml");
    fs::write(&source, "mixed-port: 7891\nrules: [MATCH,REJECT]\n").unwrap();
    let candidate = fixture.profiles.import_local(source).unwrap();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture
        .settle(cx, &page, |page| !page.persistent_loading && !page.loading)
        .await;
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
            window.click("reload-profile", cx);
        } else {
            window.click("reapply-profile-recovery", cx);
        }
    })
    .unwrap();
    fixture
        .settle(cx, &page, |page| {
            !page.core_busy() && page.core_session.generation() > attempted_version
        })
        .await;
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
}
