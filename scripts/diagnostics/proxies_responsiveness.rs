//! Isolated real-window probe: `cargo run -p zenclash-ui --example proxies_responsiveness -- 20 500 0 180`.
//! Arguments are group count, node count, response delay seconds, and idle seconds.
//! Uses generated data only; never starts a kernel or reads user configuration.

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{AppContext, Application, Context, Entity, IntoElement, Render, Window, WindowOptions};
use gpui_component::Root;
use serde_json::json;
use zenclash_core::{MihomoClient, MihomoEndpoint};
use zenclash_ui::{assets::Assets, pages::proxies::ProxiesPage};

struct Probe {
    page: Entity<ProxiesPage>,
}

impl Render for Probe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.page.clone()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let argument = |index: usize, default: u64| {
        args.get(index)
            .map(|value| value.parse::<u64>().expect("integer argument"))
            .unwrap_or(default)
    };
    let groups = argument(0, 20);
    let nodes = argument(1, 500);
    let delay = argument(2, 0);
    let idle = argument(3, 180);
    assert!(nodes > 0 && groups > 0);
    let names = (0..nodes)
        .map(|index| format!("香港节点 {index:04}"))
        .collect::<Vec<_>>();
    let mut proxies = serde_json::Map::new();
    for name in &names {
        proxies.insert(
            name.clone(),
            json!({"name": name, "type": "Shadowsocks", "udp": true, "history": []}),
        );
    }
    for index in 0..groups {
        let name = format!("Group {index:04}");
        proxies.insert(
            name.clone(),
            json!({"name": name, "type": "Selector", "now": names[0], "all": names}),
        );
    }
    let body = Arc::new(json!({"proxies": proxies}).to_string());
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.expect("fixture connection");
            let body = body.clone();
            std::thread::spawn(move || {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = [0; 4096];
                let count = stream.read(&mut request).unwrap();
                let is_catalog = request[..count].starts_with(b"GET /proxies ");
                std::thread::sleep(Duration::from_secs(delay));
                let body = if is_catalog {
                    body.as_str()
                } else {
                    r#"{"mode":"rule"}"#
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                eprintln!("fixture_response catalog={is_catalog} bytes={}", body.len());
            });
        }
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let _guard = runtime.enter();
    let client = MihomoClient::new(MihomoEndpoint::new(address.to_string(), ""))?;
    let handle = runtime.handle().clone();
    eprintln!("probe groups={groups} nodes={nodes} delay_s={delay} idle_s={idle}");
    Application::new().with_assets(Assets).run(move |cx| {
        gpui_component::init(cx);
        let window = cx
            .open_window(WindowOptions::default(), |window, cx| {
                window.set_window_title("ZenClash responsiveness probe — synthetic data");
                let page = cx.new(|cx| ProxiesPage::new(client.clone(), handle.clone(), cx));
                let probe = cx.new(|_| Probe { page });
                let root = cx.new(|cx| Root::new(probe.clone(), window, cx));
                cx.spawn(async move |cx| {
                    let started = Instant::now();
                    let mut previous = started;
                    let mut maximum = Duration::ZERO;
                    let mut phase = 0;
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(50))
                            .await;
                        let now = Instant::now();
                        let gap = now.duration_since(previous);
                        previous = now;
                        maximum = maximum.max(gap);
                        if gap > Duration::from_millis(250) {
                            eprintln!(
                                "ui_gap_ms={} elapsed_ms={}",
                                gap.as_millis(),
                                started.elapsed().as_millis()
                            );
                        }
                        let elapsed = started.elapsed().as_secs();
                        if (phase == 0 && elapsed >= 1)
                            || (phase == 1 && elapsed >= idle + delay + 5)
                        {
                            probe
                                .update(cx, |probe, cx| {
                                    probe.page = cx.new(|cx| {
                                        ProxiesPage::new(client.clone(), handle.clone(), cx)
                                    });
                                    probe.page.update(cx, ProxiesPage::reload);
                                    cx.notify();
                                })
                                .unwrap();
                            phase += 1;
                            eprintln!("load phase={phase} elapsed_s={elapsed}");
                        }
                        if elapsed >= idle + 2 * delay + 12 {
                            eprintln!("probe_complete max_ui_gap_ms={}", maximum.as_millis());
                            cx.update(|cx| cx.quit()).unwrap();
                            break;
                        }
                    }
                })
                .detach();
                root
            })
            .expect("probe window");
        window
            .update(cx, |_, window, _| window.activate_window())
            .unwrap();
    });
    Ok(())
}
