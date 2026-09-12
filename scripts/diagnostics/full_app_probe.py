#!/usr/bin/env python3
"""Prepare a macOS full-app diagnostic copy without reading the user's app data.

Only directory literals, the diagnostic binary name, and injected diagnostic
observers/optional interaction fixtures differ from the current source. This is
an instrumented development build, not a release FPS or human input-latency benchmark.
"""
import argparse
import json
from pathlib import Path
import shutil
import socket
import sys
import tempfile

INJECTION = r'''
fn run_full_app_probe(app: gpui::Entity<super::ZenClashApp>, cx: &mut App) {
    use std::time::{Duration, Instant};
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_secs(3)).await;
        for pass in 0..2 {
            for page in [super::Page::Proxies, super::Page::Connections, super::Page::Logs,
                super::Page::Settings, super::Page::Dns, super::Page::Home] {
                let started = Instant::now();
                let handle = app.read_with(cx, |app, _| app.main_window).expect("window handle");
                cx.update_window(handle, |_, window, cx| {
                    eprintln!("probe_window pass={pass} page={page:?} active={}", window.is_window_active());
                    app.update(cx, |app, cx| app.navigate(page, cx));
                    eprintln!("probe_navigation pass={pass} page={page:?} call_us={}", started.elapsed().as_micros());
                    window.on_next_frame(move |_, _| {
                        eprintln!("probe_next_frame pass={pass} page={page:?} elapsed_us={}", started.elapsed().as_micros());
                    });
                }).expect("navigate diagnostic window");
                let mut max_gap = Duration::ZERO;
                for _ in 0..20 {
                    let tick = Instant::now();
                    cx.background_executor().timer(Duration::from_millis(50)).await;
                    max_gap = max_gap.max(tick.elapsed());
                }
                eprintln!("probe_active_gap pass={pass} page={page:?} max_us={}", max_gap.as_micros());
            }
            if pass == 0 {
                app.update(cx, |app, cx| app.navigate(super::Page::Settings, cx)).expect("before hide");
                cx.update(|cx| cx.hide()).expect("hide diagnostic app");
                eprintln!("probe_hidden seconds=IDLE_SECONDS");
                // No diagnostic polling during this interval; normal app background work remains.
                cx.background_executor().timer(Duration::from_secs(IDLE_SECONDS)).await;
                cx.update(|cx| cx.activate(true)).expect("activate diagnostic app");
                eprintln!("probe_reactivated");
            }
        }
        eprintln!("probe_request_normal_quit");
        app.update(cx, |app, cx| app.begin_quit(None, cx)).expect("normal quit");
    }).detach();
}
'''


CONNECTION_SETUP = r'''        let setup = app.read_with(cx, |app, _| {
            let client = app.client.clone();
            app.runtime.spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("fixture origin");
                let origin = listener.local_addr().expect("fixture origin address");
                let accept = tokio::spawn(async move {
                    let mut accepted = Vec::new();
                    loop {
                        let (stream, _) = listener.accept().await.expect("fixture accept");
                        accepted.push(stream);
                    }
                });
                let port = client.runtime_config().await.expect("managed config").mixed_port;
                assert!(port > 0);
                let mut streams = Vec::new();
                for _ in 0..CONNECTION_COUNT {
                    let mut stream = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await.expect("fixture proxy connection");
                    stream.write_all(format!("CONNECT {origin} HTTP/1.1\r\nHost: {origin}\r\n\r\n").as_bytes()).await.expect("CONNECT request");
                    let mut response = Vec::new();
                    while !response.ends_with(b"\r\n\r\n") {
                        response.push(stream.read_u8().await.expect("CONNECT response"));
                        assert!(response.len() < 4096);
                    }
                    assert!(response.starts_with(b"HTTP/1.1 200"));
                    stream.write_all(b"fixture payload").await.expect("fixture traffic");
                    streams.push(stream);
                }
                tokio::time::sleep(Duration::from_millis(1100)).await;
                let count = client.connections_snapshot().await.expect("real connections").connections.len();
                assert_eq!(count, CONNECTION_COUNT);
                eprintln!("probe_real_connections count={count}");
                (accept, streams)
            })
        }).expect("connection fixture task");
        let _connection_fixture = setup.await.expect("connection fixture ready");
'''

CONNECTION_SEARCH = r'''
                if page == super::Page::Connections {
                    for query in ["127.0.0.1", "no-match-fixture", "127.0.0.1"] {
                        let started = Instant::now();
                        // Intermediate edits exercise coalescing and stale-result rejection.
                        for edit in ["127", "no-match-intermediate", query] {
                            cx.update_window(handle, |_, window, cx| {
                                app.update(cx, |app, cx| {
                                    app.runtime_page.update(cx, |page, cx| page.probe_set_connection_query(edit, window, cx));
                                });
                            }).expect("set query");
                            cx.background_executor().timer(Duration::from_millis(20)).await;
                        }
                        let deadline = Instant::now() + Duration::from_secs(3);
                        loop {
                            let ready = app.read_with(cx, |app, cx| app.runtime_page.read(cx).probe_connection_result(query, CONNECTION_COUNT)).expect("query result");
                            if ready { break; }
                            if Instant::now() >= deadline {
                                app.read_with(cx, |app, cx| app.runtime_page.read(cx).probe_connection_debug()).expect("projection diagnostics");
                                panic!("connection projection did not converge");
                            }
                            cx.background_executor().timer(Duration::from_millis(20)).await;
                        }
                        eprintln!("probe_search pass={pass} query={query} burst_to_result_us={}", started.elapsed().as_micros());
                    }
                }
'''

CONNECTION_HELPERS = r'''
impl RuntimePage {
    pub(crate) fn probe_set_connection_query(&mut self, query: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let started = std::time::Instant::now();
        self.connections.filter.update(cx, |input, cx| input.set_value(query, window, cx));
        eprintln!("probe_input_call query={query} us={}", started.elapsed().as_micros());
    }

    pub(crate) fn probe_connection_debug(&self) {
        let snapshot = match &self.data { RuntimeData::Connections(data) => Some(data.connections.len()), _ => None };
        let projection = self.connections.projection.as_ref().map(|p| (&p.query, p.order.len()));
        eprintln!("probe_connection_timeout page={:?} visibility={:?} loading={} snapshot={:?} query={:?} projecting={} projection={:?} error={}", self.page, self.ui_visibility, self.loading, snapshot, self.connections.query, self.connections.projecting, projection, self.error.is_some());
    }

    pub(crate) fn probe_connection_result(&self, query: &str, count: usize) -> bool {
        let Some(projection) = &self.connections.projection else { return false; };
        if self.connections.projecting || projection.query != query { return false; }
        assert_eq!(projection.snapshot.connections.len(), count);
        let expected = if query == "no-match-fixture" { 0 } else { count };
        assert_eq!(projection.order.len(), expected);
        assert!(projection.order.iter().all(|index| *index < projection.snapshot.connections.len()));
        true
    }
}
'''

PROXY_INTERACTION = r'''
                if pass == 0 && page == super::Page::Proxies {
                    let origin = app.read_with(cx, |app, _| app.runtime.spawn(async {
                        use tokio::io::{AsyncReadExt, AsyncWriteExt};
                        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                        let address = listener.local_addr().unwrap();
                        let task = tokio::spawn(async move {
                            loop {
                                let (mut stream, _) = listener.accept().await.unwrap();
                                tokio::spawn(async move {
                                    let mut request = [0; 4096];
                                    let _ = stream.read(&mut request).await;
                                    tokio::time::sleep(Duration::from_millis(10)).await;
                                    let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                                });
                            }
                        });
                        (format!("http://{address}/"), task)
                    })).unwrap().await.unwrap();
                    let deadline = Instant::now() + Duration::from_secs(30);
                    loop {
                        let ready = app.read_with(cx, |app, cx| app.proxies_page.read(cx).probe_proxy_ready()).unwrap();
                        if ready { break; }
                        assert!(Instant::now() < deadline, "proxy catalog did not load");
                        cx.background_executor().timer(Duration::from_millis(50)).await;
                    }
                    app.update(cx, |app, cx| app.proxies_page.update(cx, |page, cx| page.probe_proxy_measure(origin.0.clone(), cx))).unwrap();
                    let mut maximum = Duration::ZERO;
                    let mut calls = Duration::ZERO;
                    let mut step = 0usize;
                    loop {
                        let started = Instant::now();
                        app.update(cx, |app, cx| app.proxies_page.update(cx, |page, cx| page.probe_proxy_interact(step, cx))).unwrap();
                        calls = calls.max(started.elapsed());
                        let tick = Instant::now();
                        cx.background_executor().timer(Duration::from_millis(50)).await;
                        maximum = maximum.max(tick.elapsed());
                        step += 1;
                        let complete = app.read_with(cx, |app, cx| app.proxies_page.read(cx).probe_proxy_complete()).unwrap();
                        if complete && step >= 20 { break; }
                        assert!(Instant::now() < deadline, "proxy measurements did not finish");
                    }
                    origin.1.abort();
                    eprintln!("probe_proxy_interaction nodes=PROXY_NODE_COUNT steps={step} max_call_us={} max_gap_us={}", calls.as_micros(), maximum.as_micros());
                }
'''

PROXY_HELPERS = r'''
impl ProxiesPage {
    pub(crate) fn probe_proxy_ready(&self) -> bool {
        !self.loading && self.catalog.as_ref().is_some_and(|catalog| catalog.groups.iter().any(|group| group.name == "group-00"))
    }
    pub(crate) fn probe_proxy_measure(&mut self, url: String, cx: &mut Context<Self>) {
        let group = self.catalog.as_mut().unwrap().groups.iter_mut().find(|group| group.name == "group-00").unwrap();
        group.test_url = Some(url);
        self.test_group("group-00", cx);
        assert!(self.group_progress.contains_key("group-00"));
    }
    pub(crate) fn probe_proxy_interact(&mut self, step: usize, cx: &mut Context<Self>) {
        match step % 4 {
            0 | 2 => self.toggle_group("group-00", cx),
            1 => self.set_group_page("group-00".into(), 1, cx),
            _ => self.set_group_page("group-00".into(), 0, cx),
        }
    }
    pub(crate) fn probe_proxy_complete(&self) -> bool {
        if self.group_progress.contains_key("group-00") { return false; }
        assert!(self.testing.is_empty());
        assert!(self.test_failures.is_empty(), "fixture measurements failed: {:?}", self.error);
        let group = self.catalog.as_ref().unwrap().groups.iter().find(|group| group.name == "group-00").unwrap();
        assert_eq!(group.all.len(), PROXY_NODE_COUNT);
        assert!(group.all.iter().all(|node| node.latest_delay().is_some()));
        true
    }
}
'''

FORM_INTERACTION = r'''
                if pass == 0 && page == super::Page::Settings {
                    app.update(cx, |app, cx| app.navigate(super::Page::Mihomo, cx)).unwrap();
                    let deadline = Instant::now() + Duration::from_secs(15);
                    loop {
                        if app.read_with(cx, |app, cx| app.runtime_page.read(cx).probe_form_ready()).unwrap() { break; }
                        assert!(Instant::now() < deadline, "form did not become ready");
                        cx.background_executor().timer(Duration::from_millis(50)).await;
                    }
                    let identity = cx.update_window(handle, |_, window, cx| {
                        app.update(cx, |app, cx| app.runtime_page.update(cx, |page, cx| page.probe_form_submit(window, cx)))
                    }).unwrap();
                    app.update(cx, |app, cx| app.navigate(super::Page::Dns, cx)).unwrap();
                    loop {
                        if app.read_with(cx, |app, cx| app.runtime_page.read(cx).probe_form_complete(identity, cx)).unwrap() { break; }
                        assert!(Instant::now() < deadline, "form save did not converge");
                        cx.background_executor().timer(Duration::from_millis(50)).await;
                    }
                    app.update(cx, |app, cx| app.runtime_page.update(cx, |page, cx| page.probe_form_fail(cx))).unwrap();
                    loop {
                        if app.read_with(cx, |app, cx| app.runtime_page.read(cx).probe_form_failed()).unwrap() { break; }
                        assert!(Instant::now() < deadline, "failed save did not release busy state");
                        cx.background_executor().timer(Duration::from_millis(50)).await;
                    }
                    assert!(app.read_with(cx, |app, cx| app.runtime_page.read(cx).probe_form_complete(identity, cx)).unwrap());
                    cx.update_window(handle, |_, window, cx| {
                        app.update(cx, |app, cx| app.runtime_page.update(cx, |page, cx| {
                            assert_eq!(page.probe_form_submit(window, cx), identity);
                        }));
                    }).unwrap();
                    loop {
                        if app.read_with(cx, |app, cx| app.runtime_page.read(cx).probe_form_complete(identity, cx)).unwrap() { break; }
                        assert!(Instant::now() < deadline, "retry did not finish");
                        cx.background_executor().timer(Duration::from_millis(50)).await;
                    }
                    assert!(!app.read_with(cx, |app, cx| app.runtime_page.read(cx).probe_form_failed()).unwrap());
                    app.update(cx, |app, cx| app.navigate(super::Page::Mihomo, cx)).unwrap();
                    cx.background_executor().timer(Duration::from_millis(250)).await;
                    assert!(app.read_with(cx, |app, cx| app.runtime_page.read(cx).probe_form_complete(identity, cx)).unwrap());
                    let checked = app.read_with(cx, |app, _| {
                        let client = app.client.clone();
                        app.runtime.spawn(async move { client.runtime_config().await.unwrap().log_level })
                    }).unwrap().await.unwrap();
                    assert_eq!(checked, "debug");
                    let old_field = cx.update_window(handle, |_, window, cx| {
                        app.update(cx, |app, cx| app.runtime_page.update(cx, |page, cx| page.probe_profile_dirty(window, cx)))
                    }).unwrap();
                    app.update(cx, |app, cx| app.runtime_page.update(cx, |page, cx| page.probe_profile_import(std::path::PathBuf::from(PROFILE_B), cx))).unwrap();
                    let switch_deadline = Instant::now() + Duration::from_secs(15);
                    loop {
                        if app.read_with(cx, |app, cx| app.runtime_page.read(cx).probe_profile_changed(old_field, identity, cx)).unwrap() { break; }
                        assert!(Instant::now() < switch_deadline, "profile switch did not refresh inputs");
                        cx.background_executor().timer(Duration::from_millis(50)).await;
                    }
                    eprintln!("probe_profile_switch_passed identity_replaced=true stale_edit_cleared=true");
                    eprintln!("probe_form_passed save_after_edit=true navigation=true identity=true busy_released=true failed_save=true retry=true core_readback=true");
                }
'''

FORM_HELPERS = r'''
impl RuntimePage {
    pub(crate) fn probe_form_ready(&self) -> bool {
        self.profile_path.is_some() && !self.core_busy()
            && self.config_inputs.is_for_profile(self.profile_path.as_deref())
            && self.effective_config.pointer("/log-level").is_some()
    }
    pub(crate) fn probe_form_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::EntityId {
        let input = self.config_inputs.core.log_level.clone();
        input.update(cx, |input, cx| input.set_value("debug", window, cx));
        self.apply_controlled_config(serde_json::json!({"log-level": "debug"}), "diagnostic save", cx);
        assert!(self.core_busy(), "save did not start");
        input.update(cx, |input, cx| {
            input.set_value("warning", window, cx);
            input.set_cursor_position(gpui_component::input::Position::new(0, 3), window, cx);
        });
        input.entity_id()
    }
    pub(crate) fn probe_form_complete(&self, identity: gpui::EntityId, cx: &gpui::App) -> bool {
        if self.core_busy() || self.effective_config.pointer("/log-level").and_then(serde_json::Value::as_str) != Some("debug") { return false; }
        let input = &self.config_inputs.core.log_level;
        assert_eq!(input.entity_id(), identity);
        assert_eq!(input.read(cx).value().as_str(), "warning");
        assert_eq!(input.read(cx).cursor_position(), gpui_component::input::Position::new(0, 3));
        true
    }
    pub(crate) fn probe_form_fail(&mut self, cx: &mut Context<Self>) {
        self.apply_controlled_config(serde_json::json!({"port": 65536}), "unexpected success", cx);
        assert!(self.core_busy());
    }
    pub(crate) fn probe_form_failed(&self) -> bool {
        !self.core_busy() && self.error.is_some()
    }
    pub(crate) fn probe_profile_dirty(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::EntityId {
        let input = &self.config_inputs.core.bind_address;
        input.update(cx, |input, cx| input.set_value("unsaved-old-profile", window, cx));
        input.entity_id()
    }
    pub(crate) fn probe_profile_changed(&self, old_field: gpui::EntityId, old_log: gpui::EntityId, cx: &gpui::App) -> bool {
        if self.core_busy() || self.config_inputs_loading || self.config_inputs_profile != self.profile_path { return false; }
        let input = &self.config_inputs.core.bind_address;
        if input.entity_id() == old_field { return false; }
        assert_eq!(input.read(cx).value().as_str(), "127.0.0.1");
        assert_ne!(self.config_inputs.core.log_level.entity_id(), old_log);
        assert_ne!(self.config_inputs.core.log_level.read(cx).value().as_str(), "warning");
        true
    }
}
'''

PROFILE_HELPERS = r'''
impl RuntimePage {
    pub(crate) fn probe_profile_import(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        assert!(self.profile_store.is_some());
        self.import_local_profile_for_page(path, self.page, cx);
        assert!(self.core_busy(), "profile import did not start");
    }
}
'''

EXPORT_INTERACTION = r'''
                if pass == 0 && page == super::Page::Logs {
                    let root = std::path::PathBuf::from(PROBE_ROOT);
                    app.update(cx, |app, cx| app.runtime_page.update(cx, |page, cx| page.probe_log_export(root.clone(), cx))).unwrap();
                    let deadline = Instant::now() + Duration::from_secs(15);
                    loop {
                        if app.read_with(cx, |app, cx| app.runtime_page.read(cx).probe_log_export_done(true)).unwrap() { break; }
                        assert!(Instant::now() < deadline, "log failure did not finish");
                        cx.background_executor().timer(Duration::from_millis(50)).await;
                    }
                    app.update(cx, |app, cx| app.runtime_page.update(cx, |page, cx| {
                        page.probe_log_export(root.join("export.log"), cx);
                        page.probe_backup_export(root.join("export.zip"), cx);
                    })).unwrap();
                    loop {
                        if app.read_with(cx, |app, cx| {
                            let page = app.runtime_page.read(cx);
                            page.probe_backup_done() && page.probe_log_export_done(false)
                        }).unwrap() { break; }
                        assert!(Instant::now() < deadline, "parallel exports did not finish");
                        cx.background_executor().timer(Duration::from_millis(50)).await;
                    }
                    let verified = app.read_with(cx, |app, _| app.runtime.spawn_blocking(move || {
                        assert!(std::fs::read_to_string(root.join("export.log")).unwrap().contains("diagnostic export payload"));
                        assert!(std::fs::metadata(root.join("export.zip")).unwrap().len() > 0);
                    })).unwrap();
                    verified.await.unwrap();
                    eprintln!("probe_export_passed log_failure=true retry=true concurrent_backup=true conflicts_rejected=true files_written=true");
                }
'''

EXPORT_LOG_HELPERS = r'''
impl RuntimePage {
    pub(crate) fn probe_log_export(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        assert!(!self.logs.exporting);
        self.logs.exporting = true;
        self.error = None;
        let entries = vec![Arc::new(zenclash_core::LogEntry {
            payload: "diagnostic export payload".into(),
            ..Default::default()
        })];
        self.write_log_export(path, entries, self.page_task_token_for(Page::Logs), cx);
    }
    pub(crate) fn probe_log_export_done(&self, failed: bool) -> bool {
        if self.logs.exporting { return false; }
        assert_eq!(self.error.is_some(), failed);
        true
    }
}
'''

EXPORT_BACKUP_HELPERS = r'''
impl RuntimePage {
    pub(crate) fn probe_backup_export(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        use crate::pages::runtime::busy::MutationDomain;
        self.export_backup(path, cx);
        assert!(self.mutation_busy(MutationDomain::Backup));
        for domain in [MutationDomain::Core, MutationDomain::Logs, MutationDomain::Language,
            MutationDomain::TrafficHistory, MutationDomain::Network, MutationDomain::Backup] {
            assert!(self.begin_scoped_mutation(self.page, domain).is_none());
        }
    }
    pub(crate) fn probe_backup_done(&self) -> bool {
        !self.mutation_busy(crate::pages::runtime::busy::MutationDomain::Backup)
    }
}
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--idle-seconds', type=int, default=180)
    parser.add_argument('--nodes', type=int, default=500)
    parser.add_argument('--connections', type=int, default=0)
    parser.add_argument('--interactive-seconds', type=int, default=0)
    parser.add_argument('--interactive-page', choices=['connections', 'proxies', 'logs', 'mihomo'], default='connections')
    parser.add_argument('--proxy-interactions', action='store_true')
    parser.add_argument('--form-interactions', action='store_true')
    parser.add_argument('--export-interactions', action='store_true')
    args = parser.parse_args()
    if sys.platform != 'darwin' or args.idle_seconds < 1 or args.nodes < 1 or args.connections < 0 or args.interactive_seconds < 0:
        parser.error('macOS, positive idle-seconds/nodes, and non-negative connections are required')
    repo = Path(__file__).resolve().parents[2]
    root = Path(tempfile.mkdtemp(prefix='zenclash-full-app-'))
    source = root / 'source'
    source.mkdir()
    for name in ['Cargo.toml', 'Cargo.lock']:
        shutil.copy2(repo / name, source / name)
    for name in ['crates', 'platforms']:
        shutil.copytree(repo / name, source / name, ignore=shutil.ignore_patterns('examples'))
    data = root / 'data'
    data.mkdir()
    replaced = []
    for path in (source / 'crates').rglob('*.rs'):
        text = path.read_text()
        old = 'Library/Application Support/ZenClash'
        if old in text:
            count = text.count(old)
            path.write_text(text.replace(old, str(data)))
            replaced.append({'path': str(path.relative_to(source)), 'count': count})
    # Fail closed if directory discovery changed: review these exact sites first.
    expected = {
        'crates/zenclash-core/src/preferences.rs',
        'crates/zenclash-core/src/profiles/store.rs',
        'crates/zenclash-core/src/controlled_config/storage.rs',
        'crates/zenclash-core/src/yaml_overrides.rs',
        'crates/zenclash-core/src/process/resources.rs',
    }
    if {entry['path'] for entry in replaced} != expected or any(entry['count'] != 1 for entry in replaced):
        raise RuntimeError(f'Review changed directory discovery before running: {replaced}')
    manifest = source / 'crates/zenclash-ui/Cargo.toml'
    text = manifest.read_text()
    assert text.count('name = "zenclash"') == 1
    manifest.write_text(text.replace('name = "zenclash"', 'name = "zenclash-full-app-probe"'))
    bootstrap = source / 'crates/zenclash-ui/src/app/bootstrap.rs'
    text = bootstrap.read_text()
    marker = '            let app_for_global_quit = app.downgrade();'
    assert text.count(marker) == 1
    text = text.replace(marker, '            run_full_app_probe(app.clone(), cx);\n' + marker)
    injected = INJECTION.replace('IDLE_SECONDS', str(args.idle_seconds))
    if args.export_interactions:
        marker = '                eprintln!("probe_active_gap pass={pass} page={page:?} max_us={}", max_gap.as_micros());'
        assert marker in injected
        injected = injected.replace(marker, marker + EXPORT_INTERACTION.replace('PROBE_ROOT', json.dumps(str(root))))
        log_source = source / 'crates/zenclash-ui/src/pages/runtime/logs.rs'
        log_source.write_text(log_source.read_text() + EXPORT_LOG_HELPERS)
        backup_source = source / 'crates/zenclash-ui/src/pages/runtime/settings/backup/actions.rs'
        backup_source.write_text(backup_source.read_text() + EXPORT_BACKUP_HELPERS)
    if args.form_interactions:
        marker = '                eprintln!("probe_active_gap pass={pass} page={page:?} max_us={}", max_gap.as_micros());'
        assert marker in injected
        injected = injected.replace(marker, marker + FORM_INTERACTION.replace('PROFILE_B', json.dumps(str(root / 'fixture-b.yaml'))))
        form_source = source / 'crates/zenclash-ui/src/pages/runtime/lifecycle.rs'
        form_source.write_text(form_source.read_text() + FORM_HELPERS)
        profile_source = source / 'crates/zenclash-ui/src/pages/runtime/profiles/actions.rs'
        profile_source.write_text(profile_source.read_text() + PROFILE_HELPERS)
    if args.proxy_interactions:
        marker = '                eprintln!("probe_active_gap pass={pass} page={page:?} max_us={}", max_gap.as_micros());'
        assert marker in injected
        injected = injected.replace(marker, marker + PROXY_INTERACTION.replace('PROXY_NODE_COUNT', str(args.nodes)))
        proxy_source = source / 'crates/zenclash-ui/src/pages/proxies.rs'
        proxy_source.write_text(proxy_source.read_text() + PROXY_HELPERS.replace('PROXY_NODE_COUNT', str(args.nodes)))
    if args.connections:
        injected = injected.replace('        for pass in 0..2 {', CONNECTION_SETUP + '\n        for pass in 0..2 {')
        marker = '                eprintln!("probe_active_gap pass={pass} page={page:?} max_us={}", max_gap.as_micros());'
        assert marker in injected
        injected = injected.replace(marker, marker + CONNECTION_SEARCH)
        injected = injected.replace('CONNECTION_COUNT', str(args.connections))
        connection_source = source / 'crates/zenclash-ui/src/pages/runtime/connections.rs'
        connection_source.write_text(connection_source.read_text() + CONNECTION_HELPERS)
    if args.interactive_seconds:
        marker = '        eprintln!("probe_request_normal_quit");'
        interactive_page = {'connections': 'Connections', 'proxies': 'Proxies', 'logs': 'Logs', 'mihomo': 'Mihomo'}[args.interactive_page]
        interactive = f'''        app.update(cx, |app, cx| app.navigate(super::Page::{interactive_page}, cx)).expect("interactive page");
        cx.update(|cx| cx.activate(true)).expect("interactive activation");
        eprintln!("probe_interactive seconds={args.interactive_seconds}");
        cx.background_executor().timer(Duration::from_secs({args.interactive_seconds})).await;
'''
        assert marker in injected
        injected = injected.replace(marker, interactive + marker)
    bootstrap.write_text(text + injected)
    (data / 'preferences.json').write_text(json.dumps({
        'system_proxy_enabled': False, 'system_proxy_ownership': None,
        'traffic_history_enabled': True, 'log_file_enabled': True,
    }))
    profile = root / 'fixture.yaml'
    # Direct adapters provide a large genuine Mihomo catalog without private endpoints.
    mixed_port = 0
    if args.connections:
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            mixed_port = listener.getsockname()[1]
    lines = [f'mixed-port: {mixed_port}', 'allow-lan: false', 'mode: rule', 'log-level: info',
             'external-controller: 127.0.0.1:0', 'tun:', '  enable: false', 'proxies:']
    for index in range(args.nodes):
        lines += [f'  - name: fixture-{index:05}', '    type: direct']
    names = ', '.join(f'fixture-{index:05}' for index in range(args.nodes))
    lines += ['proxy-groups:']
    for index in range(20):
        lines += [f'  - name: group-{index:02}', '    type: select', f'    proxies: [{names}]']
    lines += ['rules:', '  - MATCH,DIRECT']
    profile.write_text('\n'.join(lines) + '\n')
    if args.form_interactions:
        (root / 'fixture-b.yaml').write_text('\n'.join(lines) + '\nbind-address: 127.0.0.1\n')
    metadata = {'root': str(root), 'source': str(source), 'data': str(data),
                'profile': str(profile), 'path_replacements': replaced,
                'idle_seconds': args.idle_seconds, 'nodes': args.nodes,
                'connections': args.connections,
                'proxy_interactions': args.proxy_interactions,
                'form_interactions': args.form_interactions,
                'export_interactions': args.export_interactions,
                'interactive_seconds': args.interactive_seconds,
                'interactive_page': args.interactive_page,
                'target_dir': str(repo / 'target')}
    (root / 'probe.json').write_text(json.dumps(metadata, indent=2))
    print(json.dumps(metadata, indent=2))


if __name__ == '__main__':
    main()
