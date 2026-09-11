#!/usr/bin/env python3
"""Prepare a macOS full-app diagnostic copy without reading the user's app data.

Only directory literals, the diagnostic binary name, and injected diagnostic
observers/optional connection fixtures differ from the current source. This is an instrumented development
build, not a release FPS or human input-latency benchmark.
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--idle-seconds', type=int, default=180)
    parser.add_argument('--nodes', type=int, default=500)
    parser.add_argument('--connections', type=int, default=0)
    parser.add_argument('--interactive-seconds', type=int, default=0)
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
        interactive = f'''        app.update(cx, |app, cx| app.navigate(super::Page::Connections, cx)).expect("interactive connections");
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
    metadata = {'root': str(root), 'source': str(source), 'data': str(data),
                'profile': str(profile), 'path_replacements': replaced,
                'idle_seconds': args.idle_seconds, 'nodes': args.nodes,
                'connections': args.connections,
                'interactive_seconds': args.interactive_seconds,
                'target_dir': str(repo / 'target')}
    (root / 'probe.json').write_text(json.dumps(metadata, indent=2))
    print(json.dumps(metadata, indent=2))


if __name__ == '__main__':
    main()
