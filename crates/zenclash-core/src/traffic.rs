use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::StreamExt;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::{runtime::Handle, task::JoinHandle};

use crate::{websocket::connect_stream, MihomoEndpoint};

/// Latest values and connection health from Mihomo's `/traffic` stream.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficSnapshot {
    /// Current upload rate in bytes per second.
    pub upload: u64,
    /// Current download rate in bytes per second.
    pub download: u64,
    /// Whether the traffic WebSocket is currently connected.
    pub connected: bool,
    /// Unix timestamp in milliseconds of the most recent traffic frame.
    pub updated_at_ms: u64,
    /// Most recent connection error, cleared after reconnection.
    pub last_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
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
    task: JoinHandle<()>,
}

impl TrafficMonitor {
    /// Starts a reconnecting traffic monitor on the supplied Tokio runtime.
    #[must_use]
    pub fn start(runtime: &Handle, endpoint: MihomoEndpoint) -> Arc<Self> {
        let snapshot = Arc::new(RwLock::new(TrafficSnapshot::default()));
        let task = runtime.spawn(run_monitor(endpoint, snapshot.clone()));
        Arc::new(Self { snapshot, task })
    }

    /// Returns a cheap point-in-time copy of the latest traffic state.
    #[must_use]
    pub fn snapshot(&self) -> TrafficSnapshot {
        self.snapshot.read().clone()
    }

    /// Returns whether the background monitor task has terminated unexpectedly.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

impl Drop for TrafficMonitor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run_monitor(endpoint: MihomoEndpoint, snapshot: Arc<RwLock<TrafficSnapshot>>) {
    loop {
        match connect_stream(&endpoint, "/traffic", &[], "连接 Mihomo 流量流超时").await {
            Ok(mut socket) => {
                update_connection(&snapshot, true, None);
                while let Some(message) = socket.next().await {
                    match message {
                        Ok(message) if message.is_text() || message.is_binary() => {
                            match serde_json::from_slice::<TrafficFrame>(&message.into_data()) {
                                Ok(frame) => update_frame(&snapshot, frame),
                                Err(error) => {
                                    tracing::debug!(%error, "ignored malformed Mihomo traffic frame");
                                    update_connection(
                                        &snapshot,
                                        true,
                                        Some(format!("流量帧解析失败：{error}")),
                                    );
                                }
                            }
                        }
                        Ok(message) if message.is_close() => break,
                        Ok(_) => {}
                        Err(error) => {
                            update_connection(&snapshot, false, Some(error.to_string()));
                            break;
                        }
                    }
                }
            }
            Err(error) => update_connection(&snapshot, false, Some(error)),
        }

        update_connection(&snapshot, false, None);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
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
    if connected && error.is_none() {
        snapshot.last_error = None;
    }
    if !connected {
        snapshot.upload = 0;
        snapshot.download = 0;
    }
    if let Some(error) = error {
        snapshot.last_error = Some(error);
    }
}

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

/// Formats a byte-per-second rate using B, KiB, MiB, or GiB units.
#[must_use]
pub fn format_speed(bytes_per_second: u64) -> String {
    match bytes_per_second {
        0..=999 => format!("{bytes_per_second} B/s"),
        1000..=1_023 => format_decimal_rate(bytes_per_second, 1000, "KB/s"),
        1_024..=1_048_575 => format_decimal_rate(bytes_per_second, 1_024, "KiB/s"),
        1_048_576..=1_073_741_823 => format_decimal_rate(bytes_per_second, 1_048_576, "MiB/s"),
        _ => format_decimal_rate(bytes_per_second, 1_073_741_824, "GiB/s"),
    }
}

fn format_decimal_rate(bytes_per_second: u64, divisor: u64, unit: &str) -> String {
    let tenths =
        (u128::from(bytes_per_second) * 10 + u128::from(divisor / 2)) / u128::from(divisor);
    format!("{}.{:01} {unit}", tenths / 10, tenths % 10)
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
        assert!(format_speed(u64::MAX).ends_with(" GiB/s"));
    }

    #[test]
    fn reconnect_clears_a_previous_connection_error() {
        let snapshot = RwLock::new(TrafficSnapshot {
            last_error: Some("offline".into()),
            ..TrafficSnapshot::default()
        });

        update_connection(&snapshot, true, None);

        assert_eq!(
            snapshot.read().clone(),
            TrafficSnapshot {
                connected: true,
                ..TrafficSnapshot::default()
            }
        );
    }

    #[test]
    fn disconnect_zeroes_rates_and_preserves_transport_error() {
        let snapshot = RwLock::new(TrafficSnapshot {
            upload: 1_024,
            download: 2_048,
            connected: true,
            ..TrafficSnapshot::default()
        });

        update_connection(&snapshot, false, Some("socket closed".into()));

        assert_eq!(
            snapshot.read().clone(),
            TrafficSnapshot {
                last_error: Some("socket closed".into()),
                ..TrafficSnapshot::default()
            }
        );
    }
}
