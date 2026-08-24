use std::{
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficSnapshot {
    pub upload: u64,
    pub download: u64,
    pub connected: bool,
    pub updated_at_ms: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TrafficFrame {
    #[serde(default)]
    up: u64,
    #[serde(default)]
    down: u64,
}

/// Maintains a reconnecting `/traffic` WebSocket and exposes a cheap snapshot
/// suitable for polling from GPUI's foreground executor.
pub struct TrafficMonitor {
    snapshot: Arc<RwLock<TrafficSnapshot>>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl TrafficMonitor {
    pub fn start(runtime: &Handle, endpoint: MihomoEndpoint) -> Arc<Self> {
        let snapshot = Arc::new(RwLock::new(TrafficSnapshot::default()));
        let (stop, stop_rx) = watch::channel(false);
        let task = runtime.spawn(run_monitor(endpoint, snapshot.clone(), stop_rx));
        Arc::new(Self {
            snapshot,
            stop,
            task,
        })
    }

    pub fn snapshot(&self) -> TrafficSnapshot {
        self.snapshot.read().clone()
    }

    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

impl Drop for TrafficMonitor {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.task.abort();
    }
}

async fn run_monitor(
    endpoint: MihomoEndpoint,
    snapshot: Arc<RwLock<TrafficSnapshot>>,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        if *stop.borrow() {
            break;
        }

        match connect(&endpoint).await {
            Ok(mut socket) => {
                update_connection(&snapshot, true, None);
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
                                    match serde_json::from_slice::<TrafficFrame>(&message.into_data()) {
                                        Ok(frame) => update_frame(&snapshot, frame),
                                        Err(error) => tracing::debug!(%error, "ignored malformed Mihomo traffic frame"),
                                    }
                                }
                                Some(Ok(message)) if message.is_close() => break,
                                Some(Ok(_)) => {}
                                Some(Err(error)) => {
                                    update_connection(&snapshot, false, Some(error.to_string()));
                                    break;
                                }
                                None => break,
                            }
                        }
                    }
                }
            }
            Err(error) => update_connection(&snapshot, false, Some(error)),
        }

        update_connection(&snapshot, false, None);
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }
}

async fn connect(
    endpoint: &MihomoEndpoint,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    String,
> {
    let url = endpoint
        .websocket_url("/traffic")
        .map_err(|error| error.to_string())?;
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

fn update_frame(snapshot: &RwLock<TrafficSnapshot>, frame: TrafficFrame) {
    let mut snapshot = snapshot.write();
    snapshot.upload = frame.up;
    snapshot.download = frame.down;
    snapshot.connected = true;
    snapshot.updated_at_ms = now_ms();
    snapshot.last_error = None;
}

fn update_connection(snapshot: &RwLock<TrafficSnapshot>, connected: bool, error: Option<String>) {
    let mut snapshot = snapshot.write();
    snapshot.connected = connected;
    if !connected {
        snapshot.upload = 0;
        snapshot.download = 0;
    }
    if let Some(error) = error {
        snapshot.last_error = Some(error);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn format_speed(bytes_per_second: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;

    match bytes_per_second {
        0..=999 => format!("{bytes_per_second} B/s"),
        1000..=1_023 => format!("{:.1} KB/s", bytes_per_second as f64 / 1000.0),
        1_024..=1_048_575 => format!("{:.1} KiB/s", bytes_per_second as f64 / KIB),
        1_048_576..=1_073_741_823 => {
            format!("{:.1} MiB/s", bytes_per_second as f64 / MIB)
        }
        _ => format!("{:.1} GiB/s", bytes_per_second as f64 / GIB),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mihomo_traffic_frames() {
        let frame: TrafficFrame = serde_json::from_str(r#"{"up":1024,"down":2048}"#).unwrap();
        assert_eq!(frame.up, 1024);
        assert_eq!(frame.down, 2048);
    }

    #[test]
    fn formats_speed_for_sidebar_and_tray() {
        assert_eq!(format_speed(0), "0 B/s");
        assert_eq!(format_speed(1024), "1.0 KiB/s");
        assert_eq!(format_speed(1_048_576), "1.0 MiB/s");
        assert_eq!(format_speed(1_073_741_824), "1.0 GiB/s");
    }
}
