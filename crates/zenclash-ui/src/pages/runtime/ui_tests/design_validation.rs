//! Native page rendering with isolated fixtures or opt-in real subscription validation.

use super::*;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::{ActiveTheme, ThemeMode};
use gpui_kit::{AnyView, Bounds, Render, WindowBounds, WindowOptions, point};
use std::{cell::RefCell, rc::Rc};

mod live;

struct DesignValidationShell {
    page: Page,
    content: AnyView,
}

impl Render for DesignValidationShell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .id("design-validation-shell")
            .test_support()
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .child(
                div()
                    .id("design-validation-pointer-target")
                    .test_support()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_1(),
            )
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
#[ignore = "renders native Windows pages; optional real core validation requires explicit inputs"]
fn native_pages_render_for_design_validation() {
    let (fixture, live, process) = match std::env::var_os("ZENCLASH_UI_VALIDATION_PROFILE") {
        Some(source) => {
            let (fixture, live, process) = live::prepare(source.into());
            (fixture, Some(live), Some(process))
        }
        None => (Fixture::new(), None, None),
    };
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
    if let Some(private_output) = std::env::var_os("ZENCLASH_UI_VALIDATION_OUTPUT") {
        output = private_output.into();
    }
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
                            if let Some(live) = &live {
                                crate::pages::proxies::ProxiesPage::design_validation_catalog(
                                    services.client.clone(),
                                    services.runtime.clone(),
                                    live.catalog.clone(),
                                    live.mode.clone(),
                                    cx,
                                )
                            } else {
                                crate::pages::proxies::ProxiesPage::design_validation_fixture(
                                    services.client.clone(),
                                    services.runtime.clone(),
                                    cx,
                                )
                            }
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
                        if live.is_some() {
                            return;
                        }
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
                cx.update(|cx| {
                    let profile = page.read(cx).profiles.catalog.active_profile().cloned();
                    proxies.update(cx, |proxies, cx| proxies.set_active_profile(profile.as_ref(), cx));
                });
                for (width, height, viewport) in [(1536., 1024., ""), (1280., 820., "-compact"), (1513., 1008., "-reference")] {
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
                            Page::Network,
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
                                        let data = live
                                            .as_ref()
                                            .and_then(|live| {
                                                live.pages
                                                    .iter()
                                                    .find(|(page, _)| *page == destination)
                                            })
                                            .map_or_else(
                                                || fixture_data(destination),
                                                |(_, data)| data.clone(),
                                            );
                                        if let RuntimeData::Connections(snapshot) = &data {
                                            page.connections.expanded = snapshot
                                                .connections
                                                .first()
                                                .map(|connection| connection.id.clone());
                                        }
                                        page.replace_page_data(
                                            page.page_task_token_for(destination),
                                            data,
                                            cx,
                                        );
                                        if live.is_some() && destination == Page::Home {
                                            page.update_home_traffic_presentation(cx);
                                        }
                                        if live.is_none() && destination == Page::Home {
                                            let RuntimeData::Connections(connections) = fixture_data(Page::Connections) else {
                                                unreachable!();
                                            };
                                            page.prepare_home_design_validation((*connections).clone());
                                        }
                                        if destination == Page::Logs {
                                            if live.is_some() {
                                                page.update_log_presentation(cx);
                                            } else {
                                                page.logs.prepare_design_validation();
                                            }
                                        }
                                        if live.is_none() && destination == Page::Connections {
                                            page.connections.expanded = Some("fixture-0".into());
                                        }
                                        if live.is_none() && destination == Page::Network {
                                            page.network_probe.snapshot = Some(zenclash_core::NetworkProbeSnapshot {
                                                route: "Mihomo".into(),
                                                public_ip: Some(zenclash_core::PublicIpInfo { ip: "203.0.113.24".into(), ..Default::default() }),
                                                latencies: [("example.com", "https://example.com/generate_204", Some(42)),
                                                    ("example.net", "https://example.net/diagnostics/connectivity/generate_204", Some(58)),
                                                    ("example.org", "https://example.org/", None)]
                                                    .into_iter().map(|(name, url, latency_ms)| zenclash_core::NetworkLatencyResult {
                                                        target: zenclash_core::NetworkLatencyTarget::new(name, url).unwrap(),
                                                        latency_ms,
                                                        error: latency_ms.is_none().then(|| "Connection timed out after 8000 ms".into()),
                                                    }).collect(),
                                                ..Default::default()
                                            });
                                        }
                                        if live.is_none() && destination == Page::Network {
                                            use zenclash_core::{DiagnosticStepKind as Kind, DiagnosticRoute as Route, DiagnosticData as Data};
                                            let snapshot = page.network_probe.snapshot.clone().unwrap();
                                            page.network_probe.report = Some(zenclash_core::DiagnosticReport {
                                                started_at_ms: 1_700_000_000_000,
                                                steps: [
                                                    (Kind::Controller, Route::Controller, Ok(Data::Controller(zenclash_core::VersionInfo { meta: true, version: "v1.19.30".into() }))),
                                                    (Kind::Capture, Route::Local, Ok(Data::Capture(Default::default()))),
                                                    (Kind::DnsA, Route::Controller, Ok(Data::Dns(Default::default()))),
                                                    (Kind::DnsAaaa, Route::Controller, Ok(Data::Dns(Default::default()))),
                                                    (Kind::NetworkDirect, Route::Direct, Ok(Data::Network(snapshot.clone()))),
                                                    (Kind::NetworkMihomo, Route::Mihomo, Ok(Data::Network(snapshot))),
                                                    (Kind::ProxyProviders, Route::Controller, Ok(Data::Providers(Default::default()))),
                                                    (Kind::RuleProviders, Route::Controller, Ok(Data::Providers(Default::default()))),
                                                ].into_iter().enumerate().map(|(index, (kind, route, outcome))| zenclash_core::DiagnosticStep {
                                                    kind, route, outcome, duration_ms: 18 + index as u64,
                                                    completed_at_ms: 1_700_000_000_020 + index as u64,
                                                }).collect(),
                                            });
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
                                            let RuntimeData::Rules { catalog, .. } = &page.read(cx).data else {
                                                return Err("rule catalog is unavailable".to_owned());
                                            };
                                            let identity = catalog.rules.first().ok_or("rule catalog has no selectable row")?.index.unwrap_or(0);
                                            if !window.try_find(("rule-details", identity)).is_some_and(|target| target.visible()) {
                                                if let Ok(image) = window.render_to_image() {
                                                    let _ = image.save(output.join(format!("rules-{suffix}{viewport}-failure.png")));
                                                }
                                                return Err(format!("rule detail action {identity} is not visible at {width} x {height} ({suffix})"));
                                            }
                                            window.click(("rule-details", identity), cx);
                                            window.render_frame(cx);
                                        }
                                        if destination == Page::Network && live.is_none() {
                                            let toggle = "toggle-network-diagnostic-details";
                                            let supplemental = "diagnostic-status:DnsAaaa";
                                            if window.try_find(supplemental).is_some() || window.try_find(("network-provider", 0_usize)).is_some() {
                                                return Err("healthy supplemental results/options should start collapsed".to_owned());
                                            }
                                            if !window.try_find(toggle).is_some_and(|target| target.visible()) {
                                                return Err("supplemental diagnostic results are not reachable".to_owned());
                                            }
                                            window.click(toggle, cx);
                                            window.render_frame(cx);
                                            if !page.read(cx).network_probe.details_expanded {
                                                return Err("diagnostic expansion did not respond".to_owned());
                                            }
                                            if !window.try_find(supplemental).is_some_and(|target| target.visible()) || !window.try_find(("network-provider", 0_usize)).is_some_and(|target| target.visible()) {
                                                return Err("expanded diagnostics do not expose supplemental results and provider options".to_owned());
                                            }
                                            window.hover("design-validation-pointer-target", cx);
                                            window.render_frame(cx);
                                            window.render_to_image().map_err(|error| error.to_string())?.save(output.join(format!("network-{suffix}-expanded{viewport}.png"))).map_err(|error| error.to_string())?;
                                            window.click(toggle, cx);
                                            window.render_frame(cx);
                                            if page.read(cx).network_probe.details_expanded {
                                                return Err("diagnostic collapse did not respond".to_owned());
                                            }
                                            let previous = page.update(cx, |page, cx| {
                                                let step = page.network_probe.report.as_mut().unwrap().steps.iter_mut().find(|step| step.kind == DiagnosticStepKind::DnsAaaa).unwrap();
                                                let previous = step.outcome.clone();
                                                step.outcome = Err(zenclash_core::DiagnosticFailure { message: "DNS query timed out while resolving a-long-diagnostic-target.example.com after 8000 ms".into() });
                                                cx.notify();
                                                previous
                                            });
                                            window.render_frame(cx);
                                            if !window.try_find(supplemental).is_some_and(|target| target.visible()) {
                                                return Err("failed supplemental DNS result was hidden by collapse".to_owned());
                                            }
                                            window.hover("design-validation-pointer-target", cx);
                                            window.render_frame(cx);
                                            window.render_to_image().map_err(|error| error.to_string())?.save(output.join(format!("network-{suffix}-partial-failure{viewport}.png"))).map_err(|error| error.to_string())?;
                                            page.update(cx, |page, cx| {
                                                page.network_probe.report.as_mut().unwrap().steps.iter_mut().find(|step| step.kind == DiagnosticStepKind::DnsAaaa).unwrap().outcome = previous;
                                                cx.notify();
                                            });
                                            window.render_frame(cx);
                                        }
                                        if destination == Page::Logs {
                                            let first_log = page.read(cx).logs.design_validation_log_id();
                                            if let Some(id) = first_log {
                                                let button = window.try_find(("inspect-log", id)).ok_or_else(|| "log detail action is missing".to_owned())?;
                                                let table = window.try_find("logs-table").ok_or_else(|| "log table is missing".to_owned())?;
                                                if !button.visible() || button.bounds().right() > table.bounds().right() {
                                                    return Err("long log pushes its detail action outside the table".to_owned());
                                                }
                                                window.click(("inspect-log", id), cx);
                                                window.render_frame(cx);
                                            }
                                        }
                                        let bounds = window.viewport_size();
                                        let mut metadata = serde_json::json!({
                                            "viewport_width": f32::from(bounds.width),
                                            "viewport_height": f32::from(bounds.height),
                                            "scale_factor": window.scale_factor(),
                                            "fixture": live.is_none(),
                                            "locale": locale,
                                        });
                                        if destination == Page::Home {
                                            let mut geometry = serde_json::Map::new();
                                            let ids = [gpui_kit::ElementId::from("home-runtime-card"), gpui_kit::ElementId::from(("home-speed-card", 0_usize)), gpui_kit::ElementId::from(("home-speed-card", 1_usize))]
                                                .into_iter().chain(["home-primary-row", "home-traffic-row", "home-traffic-panel", "home-subscription-panel", "home-activity-row", "home-activity-panel", "home-node-panel", "home-recent-connections-panel"].map(gpui_kit::ElementId::from));
                                            for id in ids {
                                                let bounds = window.try_find(id.clone()).ok_or_else(|| format!("missing Home region {id:?}"))?.bounds();
                                                geometry.insert(format!("{id:?}"), serde_json::json!({"x": bounds.origin.x.as_f32(), "y": bounds.origin.y.as_f32(), "width": bounds.size.width.as_f32(), "height": bounds.size.height.as_f32()}));
                                            }
                                            metadata["regions"] = geometry.into();
                                            if width >= 1500. {
                                                for (left, right) in [("home-traffic-panel", "home-subscription-panel"), ("home-activity-panel", "home-node-panel")] {
                                                    let left = window.try_find(left).unwrap().bounds();
                                                    let right = window.try_find(right).unwrap().bounds();
                                                    if (left.top() - right.top()).abs() * window.scale_factor() > px(1.) || (left.bottom() - right.bottom()).abs() * window.scale_factor() > px(1.) {
                                                        return Err("Overview row panels do not share their top and bottom edges".to_owned());
                                                    }
                                                }
                                                if live.is_none() && window.try_find("home-recent-connections-panel").unwrap().bounds().bottom() > bounds.height {
                                                    if let Ok(image) = window.render_to_image() {
                                                        let _ = image.save(output.join(format!("home-{suffix}{viewport}-failure.png")));
                                                    }
                                                    return Err("populated Overview recent connections extend below the reference viewport".to_owned());
                                                }
                                            }
                                        }
                                        if destination == Page::Connections {
                                            let first = window.try_find(("connection-metric-card", 0_usize)).ok_or_else(|| "missing first connection metric".to_owned())?.bounds();
                                            for index in 1_usize..4 {
                                                let bounds = window.try_find(("connection-metric-card", index)).ok_or_else(|| format!("missing connection metric {index}"))?.bounds();
                                                if (bounds.origin.y - first.origin.y).abs() * window.scale_factor() > px(1.) {
                                                    return Err("connection summary cards wrap before the compact viewport".to_owned());
                                                }
                                            }
                                            if width >= 1500. {
                                                let table = window.try_find("connections-table").ok_or_else(|| "missing connection table".to_owned())?.bounds();
                                                let inspector = window.try_find("connection-inspector").ok_or_else(|| "missing connection inspector".to_owned())?.bounds();
                                                if (table.bottom() - inspector.bottom()).abs() * window.scale_factor() > px(1.) {
                                                    return Err("connection table and inspector bottom edges differ".to_owned());
                                                }
                                            }
                                        }
                                        if destination == Page::Rules && width >= 1500. {
                                            let table = window.try_find("rules-table-panel").ok_or_else(|| "missing Rules table".to_owned())?.bounds();
                                            let inspector = window.try_find("rule-inspector").ok_or_else(|| "missing Rules inspector".to_owned())?.bounds();
                                            if (table.bottom() - inspector.bottom()).abs() * window.scale_factor() > px(1.) {
                                                return Err("Rules table and inspector bottom edges differ".to_owned());
                                            }
                                        }
                                        if destination == Page::Proxies {
                                            let pagination = window.try_find("proxy-node-pagination").ok_or_else(|| "missing node pagination".to_owned())?.bounds();
                                            let controls = window.try_find("proxy-node-pagination-controls").ok_or_else(|| "missing node pagination controls".to_owned())?.bounds();
                                            if (controls.right() - pagination.right()).as_f32() * window.scale_factor() > 1.
                                                || (pagination.left() - controls.left()).as_f32() * window.scale_factor() > 1. {
                                                return Err("node pagination controls extend outside their panel".to_owned());
                                            }
                                            let mut geometry = serde_json::Map::new();
                                            let mut panel_top = None;
                                            for id in ["proxy-summary", "proxy-group-navigation", "proxy-node-panel", "proxy-node-inspector"] {
                                                let bounds = window.try_find(id).ok_or_else(|| format!("missing proxy region {id}"))?.bounds();
                                                if id != "proxy-summary" && width >= 1500. {
                                                    let top = bounds.origin.y.as_f32();
                                                    if panel_top.is_some_and(|previous: f32| (top - previous).abs() * window.scale_factor() > 1.) {
                                                        return Err("proxy columns do not share a top edge".to_owned());
                                                    }
                                                    panel_top = Some(top);
                                                }
                                                geometry.insert(id.into(), serde_json::json!({"x": bounds.origin.x.as_f32(), "y": bounds.origin.y.as_f32(), "width": bounds.size.width.as_f32(), "height": bounds.size.height.as_f32()}));
                                            }
                                            metadata["regions"] = geometry.into();
                                        }
                                        // Park the pointer away from navigation and row actions so
                                        // their previous hover state cannot masquerade as selection.
                                        window.hover("design-validation-pointer-target", cx);
                                        window.render_frame(cx);
                                        window.render_to_image().map(|image| (image, metadata)).map_err(|error| error.to_string())
                                    })
                                })
                                .map_err(|error| error.to_string())
                                .and_then(|image| image);
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
                cx.update(|cx| {
                    let _ = cx.update_window(window.into(), |_, window, cx| {
                        page.update(cx, |page, cx| page.set_window_visible(false, cx));
                        proxies.update(cx, |proxies, _| proxies.suspend());
                        window.remove_window();
                    });
                });
                drop(shell);
                drop(proxies);
                drop(page);
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    if let Some(process) = process {
        process.stop().unwrap();
    }
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
                value: RuntimeConfig {
                    mode: "rule".into(),
                    mixed_port: 7890,
                    ..Default::default()
                },
                observed_at_ms: 0,
            },
            proxies: Observation::Fresh {
                value: zenclash_core::ProxyCatalog::from_group_nodes(
                    vec![(
                        zenclash_core::ProxyGroup {
                            name: "节点选择".into(),
                            now: "香港 · HK 01".into(),
                            behavior: zenclash_core::ProxyGroupBehavior::Selector,
                            ..Default::default()
                        },
                        [
                            ("香港 · HK 01", Some(28)),
                            ("香港 · HK 02", Some(35)),
                            ("新加坡 · SG 01", Some(0)),
                            ("日本 · JP 01", None),
                        ]
                        .into_iter()
                        .map(|(name, delay)| zenclash_core::ProxyNode {
                            name: name.into(),
                            kind: "Shadowsocks".into(),
                            udp: true,
                            history: delay
                                .into_iter()
                                .map(|delay| zenclash_core::DelayHistory {
                                    delay,
                                    ..Default::default()
                                })
                                .collect(),
                            ..Default::default()
                        })
                        .collect(),
                    )],
                    4,
                ),
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
            connections: (0..128)
                .map(|index| zenclash_core::Connection {
                    id: format!("fixture-{index}"),
                    metadata: zenclash_core::ConnectionMetadata {
                        network: if index % 3 == 0 { "UDP" } else { "TCP" }.into(),
                        host: format!("service-{index}.example.com"),
                        destination_port: "443".into(),
                        process: "browser.exe".into(),
                        ..Default::default()
                    },
                    chains: vec!["Hong Kong 01".into(), "PROXY".into()],
                    download: 1024 * (index + 1),
                    upload: 512 * (index + 1),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })),
        Page::Rules => RuntimeData::Rules {
            proxies: None,
            config: Some(RuntimeConfig {
                mode: "rule".into(),
                ..Default::default()
            }),
            catalog: Arc::new(RuleCatalog {
                rules: (0..128)
                    .map(|index| zenclash_core::Rule {
                        kind: ["DOMAIN-SUFFIX", "RULE-SET", "IP-CIDR", "GEOSITE", "MATCH"]
                            [index % 5]
                            .into(),
                        payload: format!("service-{index}.example.com"),
                        proxy: ["DIRECT", "PROXY", "REJECT"][index % 3].into(),
                        index: Some(index),
                        extra: (index % 9 != 8).then(|| zenclash_core::RuleRuntimeStats {
                            hit_count: (index + 1) as u64 * 100,
                            miss_count: index as u64 * 4,
                            disabled: index % 8 == 7,
                            hit_at: if index % 4 == 0 {
                                "2026-10-06T15:47:08+08:00".into()
                            } else {
                                String::new()
                            },
                            ..Default::default()
                        }),
                        ..Default::default()
                    })
                    .collect(),
            }),
        },
        Page::Settings => RuntimeData::Settings {
            config: Some(RuntimeConfig::default()),
            autostart: Ok(AutostartStatus::default()),
        },
        Page::Network => RuntimeData::Network {
            config: RuntimeConfig {
                ipv6: true,
                unified_delay: true,
                ..Default::default()
            },
            system: zenclash_core::SystemNetworkSnapshot::default(),
        },
        _ => RuntimeData::Empty,
    }
}
