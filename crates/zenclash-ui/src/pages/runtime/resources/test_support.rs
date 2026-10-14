//! Owned loopback resource/config controller for completed-action readback tests.

use std::{
    io::Write,
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

pub(super) struct ResourceActionController {
    pub(super) url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stopped: Arc<AtomicBool>,
    fail_catalog_reads: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl ResourceActionController {
    pub(super) fn new(provider_label: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let fail_catalog_reads = Arc::new(AtomicBool::new(false));
        let fail_reads = fail_catalog_reads.clone();
        let catalog = serde_json::json!({"providers":{"airport":{"name":provider_label,"proxies":[{"name":"node"}]}}}).to_string();
        let thread = thread::spawn(move || {
            let mut mode = "rule".to_owned();
            'connections: while !stop.load(Ordering::Acquire) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("resource fixture accept failed: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let receipt = match zenclash_core::test_support::read_controller_request_blocking(
                    &mut stream,
                ) {
                    Ok(receipt) => receipt,
                    Err(error) if zenclash_core::test_support::is_controller_header_eof(&error) => {
                        continue 'connections;
                    }
                    Err(error) => panic!("resource fixture request: {error}"),
                };
                let (headers, payload) = receipt.into_parts();
                let request = headers.lines().next().unwrap().to_owned();
                recorded.lock().unwrap().push(request.clone());
                if fail_reads.load(Ordering::Acquire)
                    && request.starts_with("GET /providers/proxies ")
                {
                    let _ = stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    continue;
                }
                let body = if request.starts_with("POST /configs/geo ")
                    || request.starts_with("POST /upgrade/ui ")
                    || request.starts_with("PUT /providers/")
                    || request.starts_with("GET /providers/proxies/airport/healthcheck ")
                {
                    None
                } else if request.starts_with("PATCH /configs ")
                    || request.starts_with("PUT /configs")
                {
                    let value: serde_json::Value = serde_json::from_slice(&payload).unwrap();
                    let changed = if let Some(payload) = value["payload"].as_str() {
                        let config: serde_yaml::Value = serde_yaml::from_str(payload).unwrap();
                        config["mode"].as_str().map(str::to_owned)
                    } else {
                        value["mode"].as_str().map(str::to_owned)
                    };
                    if let Some(changed) = changed {
                        mode = changed;
                    }
                    None
                } else if request.starts_with("GET /providers/proxies ") {
                    Some(catalog.clone())
                } else if request.starts_with("GET /providers/rules ") {
                    Some("{\"providers\":{}}".to_owned())
                } else if request.starts_with("GET /configs ") {
                    Some(serde_json::json!({"mixed-port":7890,"mode":mode}).to_string())
                } else if request.starts_with("GET /version ") {
                    Some("{\"meta\":true,\"version\":\"fixture\"}".to_owned())
                } else {
                    let _ = stream.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    continue;
                };
                let response = body.map_or_else(
                    || "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned(),
                    |body| format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()),
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self {
            url,
            requests,
            stopped,
            fail_catalog_reads,
            thread: Some(thread),
        }
    }

    pub(super) fn fail_catalog_reads(&self) {
        self.fail_catalog_reads.store(true, Ordering::Release);
    }

    pub(super) fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for ResourceActionController {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
    }
}
