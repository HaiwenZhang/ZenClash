// Injected only into the isolated source copy by full_app_probe.py.
use super::{AppContext, Context, Page, ZenClashApp};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static RENDERS: AtomicU64 = AtomicU64::new(0);

pub(crate) fn record_render() {
    RENDERS.fetch_add(1, Ordering::Relaxed);
}

fn record(value: serde_json::Value) {
    eprintln!("ZENCLASH_PROBE {}", value);
}

impl ZenClashApp {
    pub(super) fn start_full_app_probe(&mut self, cx: &mut Context<Self>) {
        let library = self.runtime.spawn_blocking(|| {
            zenclash_core::ProfileStore::discover().and_then(|store| store.load())
        });
        let client = self.client.clone();
        let fixture = self.runtime.spawn(async move {
            let (proxies, rules, config) = tokio::join!(
                client.proxy_catalog(), client.rule_catalog(), client.runtime_config());
            match (proxies, rules, config) {
                (Ok(proxies), Ok(rules), Ok(config)) => {
                    let groups: Vec<_> = proxies.groups().iter()
                        .filter(|group| group.name.starts_with("group-")).collect();
                    !config.tun.enable && groups.len() == 20
                        && groups.iter().all(|group| group.all.len() == @NODES@
                            && group.all.iter().enumerate().all(|(index, node)| proxies.node(node).is_some_and(|node| node.name == format!("node-{index:05}"))))
                        && rules.rules.len() == @RULES@ + 1
                        && rules.rules.iter().take(@RULES@).enumerate()
                            .all(|(index, rule)| rule.payload == format!("fixture-{index:06}.example"))
                }
                _ => false,
            }
        });
        cx.spawn(async move |app, cx| {
            match library.await {
                Ok(Ok(catalog)) => record(serde_json::json!({"event": "library",
                    "profiles": catalog.profiles.len(), "active": catalog.active})),
                _ => record(serde_json::json!({"event": "library_failed"})),
            }
            record(serde_json::json!({"event": "fixture_loaded", "loaded": fixture.await.unwrap_or(false)}));
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let pages = [Page::Proxies, Page::Profiles, Page::Override, Page::Traffic,
                Page::Rules, Page::Connections, Page::Logs, Page::Settings, Page::Dns,
                Page::Home];
            for round in 0..2 {
                for page in pages {
                    let start = Instant::now();
                    let Ok(active) = app.update(cx, |app, cx| {
                        app.navigate(page, cx);
                        cx.update_window(app.main_window, |_, window, _| window.is_window_active())
                            .unwrap_or(false)
                    }) else { return; };
                    record(serde_json::json!({"event": "navigation", "round": round,
                        "page": page.route(), "active": active,
                        "microseconds": start.elapsed().as_micros()}));
                    let mut last = Instant::now();
                    let mut gaps = Vec::new();
                    for _ in 0..20 {
                        cx.background_executor().timer(Duration::from_millis(50)).await;
                        let now = Instant::now();
                        gaps.push(now.duration_since(last).as_micros());
                        last = now;
                    }
                    record(serde_json::json!({"event": "timer_gaps", "round": round,
                        "page": page.route(), "microseconds": gaps}));
                }
                if round == 0 {
                    let _ = app.update(cx, |app, cx| app.hide_main_window(cx));
                    let before = RENDERS.load(Ordering::Relaxed);
                    record(serde_json::json!({"event": "hidden", "renders": before}));
                    cx.background_executor().timer(Duration::from_secs(@IDLE_SECONDS@)).await;
                    record(serde_json::json!({"event": "hidden_finished",
                        "renders": RENDERS.load(Ordering::Relaxed).saturating_sub(before)}));
                    let _ = app.update(cx, |app, cx| app.show_main_window(cx));
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                }
            }
            if @INTERACTIVE_SECONDS@ > 0 {
                let _ = app.update(cx, |app, cx| app.navigate(Page::@INTERACTIVE_PAGE@, cx));
                record(serde_json::json!({"event": "interactive"}));
                cx.background_executor().timer(Duration::from_secs(@INTERACTIVE_SECONDS@)).await;
            }
            record(serde_json::json!({"event": "normal_quit",
                "renders": RENDERS.load(Ordering::Relaxed)}));
            let _ = app.update(cx, |app, cx| app.begin_quit(None, cx));
        }).detach();
    }
}
