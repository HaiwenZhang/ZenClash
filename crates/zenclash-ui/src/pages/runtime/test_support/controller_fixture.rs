//! Shared loopback controller with owned request state and bounded HTTP header parsing.

use std::{sync::Arc, time::Duration};

pub(in crate::pages::runtime) struct ControllerFixture {
    pub(in crate::pages::runtime) proxy_catalog: Arc<std::sync::Mutex<serde_json::Value>>,
    pub(in crate::pages::runtime) proxy_selections:
        Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>,
    pub(in crate::pages::runtime) subscription_requests: Arc<std::sync::atomic::AtomicUsize>,
    pub(in crate::pages::runtime) url: String,
    pub(in crate::pages::runtime) stopped: Arc<std::sync::atomic::AtomicBool>,
    pub(in crate::pages::runtime) apply_failures: Arc<std::sync::atomic::AtomicUsize>,
    pub(in crate::pages::runtime) apply_requests: Arc<std::sync::atomic::AtomicUsize>,
    pub(in crate::pages::runtime) blocked_apply: Arc<std::sync::atomic::AtomicBool>,
    rule_catalog: Arc<std::sync::Mutex<Option<serde_json::Value>>>,
    rule_patches: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    blocked_rule_patches: Arc<std::sync::atomic::AtomicBool>,
    pub(in crate::pages::runtime) server: Option<std::thread::JoinHandle<()>>,
}

impl ControllerFixture {
    pub(in crate::pages::runtime) fn set_proxy_catalog(&self, response: serde_json::Value) {
        *self.proxy_catalog.lock().unwrap() = response;
    }

    pub(in crate::pages::runtime) fn endpoint_url(&self) -> &str {
        &self.url
    }

    pub(in crate::pages::runtime) fn set_rule_catalog(&self, catalog: zenclash_core::RuleCatalog) {
        *self.rule_catalog.lock().unwrap() = Some(serde_json::to_value(catalog).unwrap());
    }

    pub(in crate::pages::runtime) fn rule_patches(&self) -> Vec<serde_json::Value> {
        self.rule_patches.lock().unwrap().clone()
    }

    pub(in crate::pages::runtime) fn set_rule_patches_blocked(&self, blocked: bool) {
        self.blocked_rule_patches
            .store(blocked, std::sync::atomic::Ordering::Release);
    }

    pub(in crate::pages::runtime) fn new(fail_first_apply: bool) -> Self {
        use std::io::Write;
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
        let subscription_requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let subscriptions = subscription_requests.clone();
        let proxy_catalog = Arc::new(std::sync::Mutex::new(serde_json::json!({"proxies":{}})));
        let proxy_selections = Arc::new(std::sync::Mutex::new(Vec::new()));
        let catalog = proxy_catalog.clone();
        let selections = proxy_selections.clone();
        let rule_catalog = Arc::new(std::sync::Mutex::new(None::<serde_json::Value>));
        let rule_patches = Arc::new(std::sync::Mutex::new(Vec::new()));
        let blocked_rule_patches = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let rules = rule_catalog.clone();
        let patches = rule_patches.clone();
        let blocked_rules = blocked_rule_patches.clone();
        let server = std::thread::spawn(move || {
            let mut mode = "rule".to_owned();
            let mut log_level = "info".to_owned();
            'connections: while !server_stop.load(std::sync::atomic::Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let receipt =
                            match zenclash_core::test_support::read_controller_request_blocking(
                                &mut stream,
                            ) {
                                Ok(receipt) => receipt,
                                Err(error)
                                    if zenclash_core::test_support::is_controller_header_eof(
                                        &error,
                                    ) =>
                                {
                                    continue 'connections;
                                }
                                Err(error) => panic!("controller fixture request: {error}"),
                            };
                        let (headers, payload) = receipt.into_parts();
                        let request = headers.as_bytes();
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
                                .try_update(
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
                            if let Some(value) = patch["mode"].as_str() {
                                mode = value.to_owned();
                            }
                            if let Some(value) = patch["log-level"].as_str() {
                                log_level = value.to_owned();
                            }
                        }
                        if request.starts_with(b"PUT /configs") {
                            let request: serde_json::Value =
                                serde_json::from_slice(&payload).unwrap();
                            if let Some(source) = request["payload"].as_str() {
                                let config: serde_yaml::Value =
                                    serde_yaml::from_str(source).unwrap();
                                if let Some(value) = config["log-level"].as_str() {
                                    log_level = value.to_owned();
                                }
                            }
                        }
                        let proxy_path = headers
                            .lines()
                            .next()
                            .unwrap()
                            .split_whitespace()
                            .nth(1)
                            .unwrap()
                            .replace("%2D", "-");
                        let is_selection = request.starts_with(b"PUT /proxies/");
                        if is_selection {
                            let payload: serde_json::Value =
                                serde_json::from_slice(&payload).unwrap();
                            let group = proxy_path.strip_prefix("/proxies/").unwrap();
                            catalog.lock().unwrap()["proxies"][group]["now"] =
                                payload["name"].clone();
                            selections
                                .lock()
                                .unwrap()
                                .push((proxy_path.clone(), payload));
                        }
                        let is_rule_patch = request.starts_with(b"PATCH /rules/disable ")
                            && rules.lock().unwrap().is_some();
                        if is_rule_patch {
                            let patch: serde_json::Value =
                                serde_json::from_slice(&payload).unwrap();
                            patches.lock().unwrap().push(patch.clone());
                            while blocked_rules.load(std::sync::atomic::Ordering::Acquire)
                                && !server_stop.load(std::sync::atomic::Ordering::Acquire)
                            {
                                std::thread::sleep(Duration::from_millis(2));
                            }
                            for (index, disabled) in patch.as_object().unwrap() {
                                for rule in rules.lock().unwrap().as_mut().unwrap()["rules"]
                                    .as_array_mut()
                                    .unwrap()
                                {
                                    if rule["index"].as_u64() == Some(index.parse::<u64>().unwrap())
                                    {
                                        rule["extra"]["disabled"] = disabled.clone();
                                    }
                                }
                            }
                        }
                        let response = if is_apply || is_selection || is_rule_patch {
                            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned()
                        } else if request.starts_with(b"GET /configs ") {
                            let body =
                                serde_json::json!({"mixed-port":7892,"mode":mode,"log-level":log_level}).to_string();
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                        } else if request.starts_with(b"GET /proxies/") {
                            let group = proxy_path.strip_prefix("/proxies/").unwrap();
                            let body = catalog.lock().unwrap()["proxies"][group].to_string();
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                        } else if request.starts_with(b"GET /proxies ") {
                            let body = catalog.lock().unwrap().to_string();
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                        } else if request.starts_with(b"GET /rules ") {
                            if let Some(catalog) = rules.lock().unwrap().as_ref() {
                                let body = catalog.to_string();
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                    body.len()
                                )
                            } else {
                                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
                            }
                        } else if request.starts_with(b"GET /subscription ") {
                            subscriptions.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
            proxy_catalog,
            proxy_selections,
            subscription_requests,
            stopped,
            apply_failures,
            apply_requests,
            blocked_apply,
            rule_catalog,
            rule_patches,
            blocked_rule_patches,
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

#[test]
fn controller_fixture_keeps_serving_after_incomplete_peer_disconnects() {
    use std::io::{Read, Write};
    let controller = ControllerFixture::new(false);
    let address = controller.url.strip_prefix("http://").unwrap();
    for headers in ["", "GET /configs HTTP/1.1\r\nHost:"] {
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream.write_all(headers.as_bytes()).unwrap();
        stream.shutdown(std::net::Shutdown::Both).unwrap();
    }
    let mut stream = std::net::TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .write_all(b"GET /configs HTTP/1.1\r\nHost: fixture\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
}
