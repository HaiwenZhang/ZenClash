//! Explicit native rendering of isolated page fixtures; no core or capture mutation.

use super::*;
use gpui_kit::component::{ActiveTheme, ThemeMode};
use gpui_kit::{AnyView, Bounds, Render, WindowBounds, WindowOptions, point};
use std::{cell::RefCell, rc::Rc};

struct DesignValidationShell {
    page: Page,
    content: AnyView,
}

impl Render for DesignValidationShell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(crate::components::sidebar::Sidebar::new(self.page))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.content.clone()),
            )
    }
}

#[test]
#[ignore = "renders isolated native Windows page fixtures and writes validation PNGs"]
fn native_pages_render_for_design_validation() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime.as_ref().unwrap().handle().clone();
    let runtime_guard = runtime.enter();
    let services = fixture.services();
    let outcome = Rc::new(RefCell::new(None));
    let result = outcome.clone();
    let locale = if std::env::var("ZENCLASH_UI_VALIDATION_LOCALE").as_deref() == Ok("en") {
        zenclash_i18n::EN
    } else {
        zenclash_i18n::ZH_CN
    };
    let mut output =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-design-validation");
    if locale == zenclash_i18n::EN {
        output = output.join("en");
    }
    gpui_kit::application()
        .with_assets(crate::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            zenclash_i18n::set_locale(locale);
            let mut owners = None;
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(-10000.), px(-10000.)),
                            size(px(1536.), px(1024.)),
                        ))),
                        show: true,
                        focus: false,
                        ..Default::default()
                    },
                    |window, cx| {
                        let proxies = cx.new(|cx| {
                            crate::pages::proxies::ProxiesPage::design_validation_fixture(
                                services.client.clone(),
                                services.runtime.clone(),
                                cx,
                            )
                        });
                        let page = cx.new(|cx| RuntimePage::new(Page::Home, services, window, cx));
                        let shell = cx.new(|_| DesignValidationShell {
                            page: Page::Home,
                            content: page.clone().into(),
                        });
                        owners = Some((page, proxies, shell.clone()));
                        cx.new(|cx| Root::new(shell, window, cx))
                    },
                )
                .unwrap();
            let (page, proxies, shell) = owners.unwrap();
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(700))
                    .await;
                let mut saved = Ok(());
                cx.update(|cx| {
                    page.update(cx, |page, _| {
                        let template = page.profiles.catalog.profiles[0].clone();
                        page.profiles.catalog.profiles = [
                            "Daily Network",
                            "Streaming Network",
                            "Gaming Profile",
                            "Local Development",
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(index, name)| {
                            let mut record = template.clone();
                            if index > 0 {
                                record.id = format!("fixture-{index}");
                            }
                            record.name = name.into();
                            if index < 3 {
                                record.source = zenclash_core::ProfileSource::Remote {
                                    url: "https://example.invalid/profile.yaml".into(),
                                    user_agent: String::new(),
                                    options: Default::default(),
                                };
                                record.subscription.usage =
                                    Some(zenclash_core::SubscriptionUsage {
                                        total: 100 * 1024 * 1024 * 1024,
                                        download: 24 * 1024 * 1024 * 1024,
                                        upload: 1024 * 1024 * 1024,
                                        ..Default::default()
                                    });
                            }
                            record
                        })
                        .collect();
                        page.profiles
                            .forms
                            .catalog_view
                            .prepare(&page.profiles.catalog);
                    })
                });
                for (width, height, viewport) in [(1536., 1024., ""), (1280., 820., "-compact")] {
                    cx.update(|cx| {
                        cx.update_window(window.into(), |_, window, _| {
                            window.resize(size(px(width), px(height)));
                        })
                    })
                    .unwrap();
                    cx.background_executor()
                        .timer(Duration::from_millis(300))
                        .await;
                    for (mode, suffix) in [(ThemeMode::Light, "light"), (ThemeMode::Dark, "dark")] {
                        for destination in [
                            Page::Home,
                            Page::Proxies,
                            Page::Profiles,
                            Page::Connections,
                            Page::Rules,
                            Page::Logs,
                            Page::Settings,
                        ] {
                            cx.update(|cx| {
                                cx.update_window(window.into(), |_, window, cx| {
                                    crate::design::apply_zen_theme(mode, None, cx);
                                    page.update(cx, |page, cx| {
                                        page.invalidate_page_load();
                                        page.page = destination;
                                        page.persistent_loading = false;
                                        page.startup_error = None;
                                        page.error = None;
                                        page.app_update = Default::default();
                                        page.ui_visibility = lifecycle::UiVisibility::new(true);
                                        page.live_updates_enabled.send_replace(false);
                                        page.settings_navigation
                                            .scroll
                                            .set_offset(point(px(0.), px(0.)));
                                        let data = fixture_data(destination);
                                        page.replace_page_data(
                                            page.page_task_token_for(destination),
                                            data,
                                            cx,
                                        );
                                        if destination == Page::Logs {
                                            page.logs.prepare_design_validation();
                                        }
                                        if destination == Page::Connections {
                                            page.connections.expanded = Some("fixture-0".into());
                                        }
                                        cx.notify();
                                    });
                                    shell.update(cx, |shell, cx| {
                                        shell.page = destination;
                                        shell.content = if destination == Page::Proxies {
                                            proxies.clone().into()
                                        } else {
                                            page.clone().into()
                                        };
                                        cx.notify();
                                    });
                                    window.render_frame(cx);
                                })
                            })
                            .unwrap();
                            cx.background_executor()
                                .timer(Duration::from_millis(200))
                                .await;
                            let image = cx
                                .update(|cx| {
                                    cx.update_window(window.into(), |_, window, cx| {
                                        window.render_frame(cx);
                                        if destination == Page::Rules {
                                            window.click(("rule-details", 0usize), cx);
                                            window.render_frame(cx);
                                        }
                                        let bounds = window.viewport_size();
                                        let metadata = serde_json::json!({
                                            "viewport_width": f32::from(bounds.width),
                                            "viewport_height": f32::from(bounds.height),
                                            "scale_factor": window.scale_factor(),
                                            "fixture": true,
                                            "locale": locale,
                                        });
                                        window.render_to_image().map(|image| (image, metadata))
                                    })
                                })
                                .and_then(|image| image)
                                .map_err(|error| error.to_string());
                            let path = output
                                .join(format!("{}-{suffix}{viewport}.png", destination.route()));
                            saved = cx
                                .background_executor()
                                .spawn(async move {
                                    let (image, mut metadata) = image?;
                                    if image.width() < 1200 || image.height() < 800 {
                                        return Err(format!(
                                            "native renderer returned only {} x {} pixels",
                                            image.width(),
                                            image.height()
                                        ));
                                    }
                                    fs::create_dir_all(path.parent().unwrap())
                                        .map_err(|error| error.to_string())?;
                                    metadata["image_width"] = image.width().into();
                                    metadata["image_height"] = image.height().into();
                                    fs::write(path.with_extension("json"), metadata.to_string())
                                        .map_err(|error| error.to_string())?;
                                    image.save(path).map_err(|error| error.to_string())
                                })
                                .await;
                            if saved.is_err() {
                                break;
                            }
                        }
                        if saved.is_err() {
                            break;
                        }
                    }
                    if saved.is_err() {
                        break;
                    }
                }
                *result.borrow_mut() = Some(saved);
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    outcome
        .borrow_mut()
        .take()
        .expect("native render did not finish")
        .unwrap();
    drop(runtime_guard);
    drop(fixture);
}

fn fixture_data(page: Page) -> RuntimeData {
    match page {
        Page::Home => RuntimeData::Dashboard {
            config: Observation::Fresh {
                value: RuntimeConfig::default(),
                observed_at_ms: 0,
            },
            proxies: Observation::Fresh {
                value: zenclash_core::ProxyCatalog::default(),
                observed_at_ms: 0,
            },
        },
        Page::Profiles => RuntimeData::Profile {
            config: Some(RuntimeConfig::default()),
            proxy_count: Some(9),
            group_count: Some(2),
            rule_count: Some(30),
        },
        Page::Connections => RuntimeData::Connections(Arc::new(ConnectionsSnapshot {
            connections: (0..8)
                .map(|index| zenclash_core::Connection {
                    id: format!("fixture-{index}"),
                    metadata: zenclash_core::ConnectionMetadata {
                        network: if index % 3 == 0 { "UDP" } else { "TCP" }.into(),
                        host: format!("service-{index}.example.com"),
                        destination_port: "443".into(),
                        process: "browser.exe".into(),
                        ..Default::default()
                    },
                    chains: vec!["PROXY".into(), "Hong Kong 01".into()],
                    download: 1024 * (index + 1),
                    upload: 512 * (index + 1),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })),
        Page::Rules => RuntimeData::Rules(Arc::new(RuleCatalog {
            rules: (0..8)
                .map(|index| zenclash_core::Rule {
                    kind: if index % 2 == 0 {
                        "DOMAIN-SUFFIX"
                    } else {
                        "RULE-SET"
                    }
                    .into(),
                    payload: format!("service-{index}.example.com"),
                    proxy: if index % 3 == 0 { "DIRECT" } else { "PROXY" }.into(),
                    index: Some(index),
                    extra: Some(zenclash_core::RuleRuntimeStats {
                        hit_count: (index + 1) as u64 * 100,
                        ..Default::default()
                    }),
                    ..Default::default()
                })
                .collect(),
        })),
        Page::Settings => RuntimeData::Settings {
            config: Some(RuntimeConfig::default()),
            autostart: Ok(AutostartStatus::default()),
        },
        _ => RuntimeData::Empty,
    }
}
