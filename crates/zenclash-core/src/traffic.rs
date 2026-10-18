use std::{
    collections::VecDeque,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tokio::{runtime::Handle, task::JoinHandle};

use crate::{MihomoClient, MihomoEndpoint, client::ControllerBinding};

mod publication;
use publication::TrafficState;

/// Latest values and connection health from Mihomo's `/traffic` stream.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficSnapshot {
    /// Core-session generation that produced the last accepted frame.
    pub generation: u64,
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

/// One ordered sample from Mihomo's traffic stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficSample {
    /// Upload rate in bytes per second.
    pub upload: u64,
    /// Download rate in bytes per second.
    pub download: u64,
}

/// Number of logical traffic frames retained for charts.
pub const LIVE_TRAFFIC_SAMPLE_COUNT: usize = 24;

/// Maximum number of real, timestamped frames retained for window restoration.
pub const LIVE_TRAFFIC_HISTORY_LIMIT: usize = 300;

/// One real traffic observation retained independently of window visibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimedTrafficSample {
    timestamp_ms: u64,
    rates: TrafficSample,
}

impl TimedTrafficSample {
    /// Creates a historical observation from its timestamp and observed rates.
    #[must_use]
    pub const fn new(timestamp_ms: u64, rates: TrafficSample) -> Self {
        Self {
            timestamp_ms,
            rates,
        }
    }

    /// Returns the Unix-millisecond time of the real stream frame.
    #[must_use]
    pub const fn timestamp_ms(&self) -> u64 {
        self.timestamp_ms
    }

    /// Returns upload and download rates in bytes per second.
    #[must_use]
    pub const fn rates(&self) -> TrafficSample {
        self.rates
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TrafficSamples {
    recent: VecDeque<TrafficSample>,
    timed: VecDeque<TimedTrafficSample>,
}

impl TrafficSamples {
    fn record(&mut self, timestamp_ms: u64, rates: TrafficSample) {
        if self.recent.len() >= LIVE_TRAFFIC_SAMPLE_COUNT {
            self.recent.pop_front();
        }
        self.recent.push_back(rates);
        if self.timed.len() >= LIVE_TRAFFIC_HISTORY_LIMIT {
            self.timed.pop_front();
        }
        self.timed
            .push_back(TimedTrafficSample::new(timestamp_ms, rates));
    }
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
    state: Arc<TrafficState>,
    task: JoinHandle<()>,
}

impl TrafficMonitor {
    /// Starts a reconnecting traffic monitor on the supplied Tokio runtime.
    #[must_use]
    pub fn start(runtime: &Handle, endpoint: MihomoEndpoint) -> Arc<Self> {
        Self::start_binding(runtime, ControllerBinding::direct(endpoint))
    }

    /// Starts a monitor following every transport switch of the shared client.
    #[must_use]
    pub fn start_with_client(runtime: &Handle, client: MihomoClient) -> Arc<Self> {
        Self::start_binding(runtime, client.binding)
    }

    fn start_binding(runtime: &Handle, binding: Arc<ControllerBinding>) -> Arc<Self> {
        let state = Arc::new(TrafficState::new());
        let task = runtime.spawn(run_monitor(binding, state.clone()));
        Arc::new(Self { state, task })
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

async fn run_monitor(binding: Arc<ControllerBinding>, state: Arc<TrafficState>) {
    let mut generation_updates = state.subscribe_generation();
    let mut backoff = crate::websocket::ReconnectBackoff::default();
    let mut transport_updates = binding.subscribe();
    loop {
        let transport_generation = transport_updates.borrow_and_update().generation;
        let generation = *generation_updates.borrow_and_update();
        let mut generation_changed = false;
        let connection = tokio::select! {
            changed = transport_updates.changed() => { if changed.is_err() { return; } continue; }
            changed = generation_updates.changed() => { if changed.is_err() { return; } continue; }
            connection = binding.connect("/traffic", &[], "连接 Mihomo 流量流超时") => connection,
        };
        match connection {
            Ok(mut socket) => {
                state.record_connection(true, None, generation);
                loop {
                    let message = tokio::select! {
                        changed = transport_updates.changed() => {
                            if changed.is_err() { return; }
                            generation_changed = true;
                            break;
                        }
                        changed = generation_updates.changed() => {
                            if changed.is_err() {
                                return;
                            }
                            generation_changed = true;
                            break;
                        }
                        message = socket.next() => message,
                    };
                    let Some(message) = message else {
                        break;
                    };
                    match message {
                        Ok(message)
                            if (message.is_text() || message.is_binary())
                                && binding.is_current(transport_generation) =>
                        {
                            match serde_json::from_slice::<TrafficFrame>(&message.into_data()) {
                                Ok(frame) => {
                                    backoff.reset();
                                    state.record_frame(frame, generation);
                                }
                                Err(error) => {
                                    tracing::debug!(%error, "ignored malformed Mihomo traffic frame");
                                    state.record_connection(
                                        true,
                                        Some(format!("流量帧解析失败：{error}")),
                                        generation,
                                    );
                                }
                            }
                        }
                        Ok(message) if message.is_close() => break,
                        Ok(_) => {}
                        Err(error) => {
                            state.record_connection(false, Some(error.to_string()), generation);
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                state.record_connection(false, Some(error), generation);
            }
        }

        state.record_connection(false, None, generation);
        if generation_changed {
            backoff.reset();
            continue;
        }
        tokio::select! {
            changed = transport_updates.changed() => {
                if changed.is_err() { return; }
                backoff.reset();
            }
            changed = generation_updates.changed() => {
                if changed.is_err() {
                    return;
                }
                backoff.reset();
            }
            () = tokio::time::sleep(backoff.next_delay()) => {}
        }
    }
}

fn initial_samples() -> TrafficSamples {
    TrafficSamples {
        recent: VecDeque::from(vec![TrafficSample::default(); LIVE_TRAFFIC_SAMPLE_COUNT]),
        timed: VecDeque::new(),
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
    fn timed_history_retains_only_real_frames_with_a_fixed_capacity() {
        let mut samples = initial_samples();
        assert!(samples.timed.is_empty());
        for timestamp_ms in 1..=1_000 {
            samples.record(
                timestamp_ms,
                TrafficSample {
                    upload: timestamp_ms,
                    download: timestamp_ms * 2,
                },
            );
        }
        assert_eq!(samples.timed.len(), LIVE_TRAFFIC_HISTORY_LIMIT);
        assert_eq!(samples.timed.front().unwrap().timestamp_ms(), 701);
        assert_eq!(samples.timed.back().unwrap().timestamp_ms(), 1_000);
        assert_eq!(samples.timed.back().unwrap().rates().download, 2_000);
    }

    #[test]
    fn formats_speed_for_sidebar_and_tray() {
        assert_eq!(format_speed(0), "0 B/s");
        assert_eq!(format_speed(1024), "1.0 KiB/s");
        assert_eq!(format_speed(1_048_576), "1.0 MiB/s");
        assert_eq!(format_speed(1_073_741_824), "1.0 GiB/s");
        assert!(format_speed(u64::MAX).ends_with(" GiB/s"));
    }

    #[tokio::test]
    async fn generation_selected_before_the_reader_starts_is_used_and_drop_closes_the_socket() {
        use futures_util::SinkExt;
        use tokio_tungstenite::{accept_async, tungstenite::Message};

        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let endpoint =
            MihomoEndpoint::new(format!("http://{}", listener.local_addr().unwrap()), "");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            socket
                .send(Message::Text(r#"{"up":10,"down":20}"#.into()))
                .await
                .unwrap();
            futures_util::StreamExt::next(&mut socket).await;
        });

        let monitor = TrafficMonitor::start(&Handle::current(), endpoint);
        monitor.synchronize_generation(7);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while monitor.snapshot().updated_at_ms == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(monitor.snapshot().generation, 7);
        assert_eq!(monitor.snapshot().upload, 10);
        assert_eq!(monitor.timed_samples().len(), 1);

        let weak = Arc::downgrade(&monitor);
        drop(monitor);
        assert!(weak.upgrade().is_none());
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn monitor_reconnects_when_an_existing_client_clone_switches_controller() {
        use futures_util::SinkExt;
        use tokio_tungstenite::{accept_async, tungstenite::Message};
        let first = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let second = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let first_endpoint =
            MihomoEndpoint::new(format!("http://{}", first.local_addr().unwrap()), "");
        let second_endpoint =
            MihomoEndpoint::new(format!("http://{}", second.local_addr().unwrap()), "");
        let first_server = tokio::spawn(async move {
            let (stream, _) = first.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            socket
                .send(Message::Text(r#"{"up":10,"down":20}"#.into()))
                .await
                .unwrap();
            futures_util::StreamExt::next(&mut socket).await;
        });
        let second_server = tokio::spawn(async move {
            let (stream, _) = second.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            socket
                .send(Message::Text(r#"{"up":30,"down":40}"#.into()))
                .await
                .unwrap();
            futures_util::StreamExt::next(&mut socket).await;
        });
        let client = MihomoClient::new(first_endpoint).unwrap();
        let monitor = TrafficMonitor::start_with_client(&Handle::current(), client.clone());
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while monitor.snapshot().upload != 10 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        client.switch_to_direct(second_endpoint).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while monitor.snapshot().upload != 30 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(monitor.samples().back().unwrap().download, 40);
        drop(monitor);
        first_server.await.unwrap();
        second_server.await.unwrap();
    }
}
