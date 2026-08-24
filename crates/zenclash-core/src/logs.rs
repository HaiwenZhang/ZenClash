use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::StreamExt;
use http::HeaderValue;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::{runtime::Handle, sync::watch, task::JoinHandle};
use tokio_tungstenite::{connect_async, tungstenite::client::IntoClientRequest};

use crate::MihomoEndpoint;

const MAX_LOG_ENTRIES: usize = 2_000;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct LogEntry {
    #[serde(default, rename = "type")]
    pub level: String,
    #[serde(default)]
    pub payload: String,
    #[serde(default)]
    pub timestamp_ms: u64,
}

pub struct LogMonitor {
    entries: Arc<RwLock<VecDeque<LogEntry>>>,
    connected: Arc<RwLock<bool>>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl LogMonitor {
    pub fn start(runtime: &Handle, endpoint: MihomoEndpoint, level: &str) -> Arc<Self> {
        let entries = Arc::new(RwLock::new(VecDeque::new()));
        let connected = Arc::new(RwLock::new(false));
        let (stop, stop_rx) = watch::channel(false);
        let task = runtime.spawn(run_monitor(
            endpoint,
            level.to_owned(),
            entries.clone(),
            connected.clone(),
            stop_rx,
        ));
        Arc::new(Self {
            entries,
            connected,
            stop,
            task,
        })
    }

    pub fn entries(&self) -> Vec<LogEntry> {
        self.entries.read().iter().cloned().collect()
    }

    pub fn clear(&self) {
        self.entries.write().clear();
    }

    pub fn connected(&self) -> bool {
        *self.connected.read()
    }

    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

impl Drop for LogMonitor {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.task.abort();
    }
}

async fn run_monitor(
    endpoint: MihomoEndpoint,
    level: String,
    entries: Arc<RwLock<VecDeque<LogEntry>>>,
    connected: Arc<RwLock<bool>>,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        if *stop.borrow() {
            return;
        }
        match connect(&endpoint, &level).await {
            Ok(mut socket) => {
                *connected.write() = true;
                loop {
                    tokio::select! {
                        changed = stop.changed() => {
                            if changed.is_err() || *stop.borrow() {
                                return;
                            }
                        }
                        message = socket.next() => {
                            match message {
                                Some(Ok(message)) if message.is_text() || message.is_binary() => {
                                    if let Ok(mut entry) = serde_json::from_slice::<LogEntry>(&message.into_data()) {
                                        entry.timestamp_ms = now_ms();
                                        let mut entries = entries.write();
                                        if entries.len() >= MAX_LOG_ENTRIES {
                                            entries.pop_front();
                                        }
                                        entries.push_back(entry);
                                    }
                                }
                                Some(Ok(message)) if message.is_close() => break,
                                Some(Ok(_)) => {}
                                Some(Err(_)) | None => break,
                            }
                        }
                    }
                }
            }
            Err(error) => {
                let mut entries = entries.write();
                if entries.len() >= MAX_LOG_ENTRIES {
                    entries.pop_front();
                }
                entries.push_back(LogEntry {
                    level: "error".into(),
                    payload: format!("日志流连接失败：{error}"),
                    timestamp_ms: now_ms(),
                });
            }
        }
        *connected.write() = false;
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    return;
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }
}

async fn connect(
    endpoint: &MihomoEndpoint,
    level: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    String,
> {
    let mut url = endpoint
        .websocket_url("/logs")
        .map_err(|error| error.to_string())?;
    url.push_str("?level=");
    url.push_str(level);
    let mut request = url
        .into_client_request()
        .map_err(|error| error.to_string())?;
    if !endpoint.secret.is_empty() {
        let value = HeaderValue::from_str(&format!("Bearer {}", endpoint.secret))
            .map_err(|_| "invalid Mihomo authorization header".to_owned())?;
        request.headers_mut().insert("Authorization", value);
    }
    connect_async(request)
        .await
        .map(|(socket, _)| socket)
        .map_err(|error| error.to_string())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_log_frame() {
        let entry: LogEntry = serde_json::from_str(
            r#"{"type":"info","payload":"[TCP] connected to example.com:443"}"#,
        )
        .unwrap();
        assert_eq!(entry.level, "info");
        assert!(entry.payload.contains("example.com"));
    }
}
