use std::{
    collections::VecDeque,
    sync::{Arc, atomic::AtomicU64},
    time::{SystemTime, UNIX_EPOCH},
};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::{runtime::Handle, sync::watch, task::JoinHandle};

use crate::{MihomoClient, MihomoEndpoint, client::ControllerBinding};

mod publication;
#[cfg(test)]
use publication::{update_connection, update_frame};
use publication::{update_connection_for_generation, update_frame_for_generation};

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

#[derive(Debug)]
struct TrafficRevision {
    updates: watch::Sender<u64>,
}

impl TrafficRevision {
    fn new() -> Arc<Self> {
        Self::with_value(0)
    }

    fn with_value(value: u64) -> Arc<Self> {
        let (updates, _) = watch::channel(value);
        Arc::new(Self { updates })
    }

    fn current(&self) -> u64 {
        *self.updates.borrow()
    }

    fn advance(&self) {
        self.updates
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    fn subscribe(&self) -> watch::Receiver<u64> {
        self.updates.subscribe()
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
    snapshot: Arc<RwLock<TrafficSnapshot>>,
    samples: Arc<RwLock<TrafficSamples>>,
    revision: Arc<TrafficRevision>,
    expected_generation: Arc<AtomicU64>,
    generation: watch::Sender<u64>,
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
        let snapshot = Arc::new(RwLock::new(TrafficSnapshot::default()));
        let samples = Arc::new(RwLock::new(initial_samples()));
        let revision = TrafficRevision::new();
        let expected_generation = Arc::new(AtomicU64::new(0));
        let (generation, generation_updates) = watch::channel(0);
        let task = runtime.spawn(run_monitor(
            binding,
            snapshot.clone(),
            samples.clone(),
            revision.clone(),
            expected_generation.clone(),
            generation_updates,
        ));
        Arc::new(Self {
            snapshot,
            samples,
            revision,
            expected_generation,
            generation,
            task,
        })
    }

    /// Returns a cheap point-in-time copy of the latest traffic state.
    #[must_use]
    pub fn snapshot(&self) -> TrafficSnapshot {
        self.snapshot.read().clone()
    }

    /// Returns the short ordered frame history used by compact traffic displays.
    ///
    /// Frames are appended by the WebSocket reader, not by UI polling timers,
    /// so repeated renders cannot create duplicate or contradictory samples.
    #[must_use]
    pub fn samples(&self) -> VecDeque<TrafficSample> {
        self.samples.read().recent.clone()
    }

    /// Returns bounded real stream history, including frames collected while hidden.
    ///
    /// Historical rates survive configuration changes and reconnects. Queued frames
    /// from superseded generations are rejected before entering this history.
    #[must_use]
    pub fn timed_samples(&self) -> VecDeque<TimedTrafficSample> {
        self.samples.read().timed.clone()
    }

    /// Returns a monotonic revision for traffic frames and connection-state changes.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision.current()
    }

    /// Subscribes to traffic frames and connection-state changes.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.revision.subscribe()
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

async fn run_monitor(
    binding: Arc<ControllerBinding>,
    snapshot: Arc<RwLock<TrafficSnapshot>>,
    samples: Arc<RwLock<TrafficSamples>>,
    revision: Arc<TrafficRevision>,
    expected_generation: Arc<AtomicU64>,
    mut generation_updates: watch::Receiver<u64>,
) {
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
                update_connection_for_generation(
                    &snapshot,
                    true,
                    None,
                    generation,
                    &expected_generation,
                    &revision,
                );
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
                                    update_frame_for_generation(
                                        &snapshot,
                                        &samples,
                                        frame,
                                        generation,
                                        &expected_generation,
                                        &revision,
                                    );
                                }
                                Err(error) => {
                                    tracing::debug!(%error, "ignored malformed Mihomo traffic frame");
                                    update_connection_for_generation(
                                        &snapshot,
                                        true,
                                        Some(format!("流量帧解析失败：{error}")),
                                        generation,
                                        &expected_generation,
                                        &revision,
                                    );
                                }
                            }
                        }
                        Ok(message) if message.is_close() => break,
                        Ok(_) => {}
                        Err(error) => {
                            update_connection_for_generation(
                                &snapshot,
                                false,
                                Some(error.to_string()),
                                generation,
                                &expected_generation,
                                &revision,
                            );
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                update_connection_for_generation(
                    &snapshot,
                    false,
                    Some(error),
                    generation,
                    &expected_generation,
                    &revision,
                );
            }
        }

        update_connection_for_generation(
            &snapshot,
            false,
            None,
            generation,
            &expected_generation,
            &revision,
        );
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
    fn each_stream_frame_advances_the_single_logical_sample_series() {
        let snapshot = RwLock::new(TrafficSnapshot::default());
        let samples = RwLock::new(initial_samples());

        update_frame(
            &mut snapshot.write(),
            &mut samples.write(),
            TrafficFrame {
                up: 1_024,
                down: 2_048,
            },
            0,
        );

        let samples = samples.read();
        assert_eq!(samples.recent.len(), LIVE_TRAFFIC_SAMPLE_COUNT);
        assert_eq!(
            samples.recent.back(),
            Some(&TrafficSample {
                upload: 1_024,
                download: 2_048,
            })
        );
    }

    #[test]
    fn disconnect_does_not_append_synthetic_zero_samples() {
        let snapshot = RwLock::new(TrafficSnapshot::default());
        let samples = RwLock::new(initial_samples());
        update_frame(
            &mut snapshot.write(),
            &mut samples.write(),
            TrafficFrame { up: 10, down: 20 },
            0,
        );
        let before_disconnect = samples.read().clone();

        update_connection(&mut snapshot.write(), false, Some("offline".into()), 0);

        assert_eq!(*samples.read(), before_disconnect);
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

    #[test]
    fn reconnect_clears_a_previous_connection_error() {
        let snapshot = RwLock::new(TrafficSnapshot {
            last_error: Some("offline".into()),
            ..TrafficSnapshot::default()
        });

        update_connection(&mut snapshot.write(), true, None, 0);

        assert_eq!(
            snapshot.read().clone(),
            TrafficSnapshot {
                connected: true,
                ..TrafficSnapshot::default()
            }
        );
    }

    #[test]
    fn disconnect_preserves_last_successful_rates_and_timestamp() {
        let snapshot = RwLock::new(TrafficSnapshot {
            upload: 1_024,
            download: 2_048,
            connected: true,
            updated_at_ms: 42,
            ..TrafficSnapshot::default()
        });

        update_connection(
            &mut snapshot.write(),
            false,
            Some("socket closed".into()),
            0,
        );

        assert_eq!(
            snapshot.read().clone(),
            TrafficSnapshot {
                upload: 1_024,
                download: 2_048,
                updated_at_ms: 42,
                last_error: Some("socket closed".into()),
                ..TrafficSnapshot::default()
            }
        );
    }

    #[test]
    fn late_frame_from_an_old_generation_is_rejected() {
        let snapshot = RwLock::new(TrafficSnapshot {
            generation: 2,
            upload: 30,
            download: 40,
            updated_at_ms: 50,
            ..TrafficSnapshot::default()
        });
        let samples = RwLock::new(initial_samples());
        let expected_generation = AtomicU64::new(2);
        let revision = TrafficRevision::new();

        let accepted = update_frame_for_generation(
            &snapshot,
            &samples,
            TrafficFrame { up: 100, down: 200 },
            1,
            &expected_generation,
            &revision,
        );

        assert!(!accepted);
        assert_eq!(snapshot.read().upload, 30);
        assert_eq!(snapshot.read().generation, 2);
        assert_eq!(*samples.read(), initial_samples());
        assert_eq!(revision.current(), 0);
    }

    #[test]
    fn accepted_frame_advances_the_traffic_revision() {
        let snapshot = RwLock::new(TrafficSnapshot::default());
        let samples = RwLock::new(initial_samples());
        let expected_generation = AtomicU64::new(0);
        let revision = TrafficRevision::with_value(7);

        let accepted = update_frame_for_generation(
            &snapshot,
            &samples,
            TrafficFrame { up: 100, down: 200 },
            0,
            &expected_generation,
            &revision,
        );

        assert!(accepted);
        assert_eq!(revision.current(), 8);
    }

    #[test]
    fn repeated_connection_state_does_not_advance_the_traffic_revision() {
        let snapshot = RwLock::new(TrafficSnapshot {
            connected: true,
            ..TrafficSnapshot::default()
        });
        let expected_generation = AtomicU64::new(0);
        let revision = TrafficRevision::with_value(3);

        let accepted = update_connection_for_generation(
            &snapshot,
            true,
            None,
            0,
            &expected_generation,
            &revision,
        );

        assert!(accepted);
        assert_eq!(revision.current(), 3);
    }

    #[test]
    fn traffic_revision_subscription_observes_an_accepted_frame() {
        let snapshot = RwLock::new(TrafficSnapshot::default());
        let samples = RwLock::new(initial_samples());
        let expected_generation = AtomicU64::new(0);
        let revision = TrafficRevision::new();
        let updates = revision.subscribe();

        let _ = update_frame_for_generation(
            &snapshot,
            &samples,
            TrafficFrame { up: 100, down: 200 },
            0,
            &expected_generation,
            &revision,
        );

        assert!(updates.has_changed().unwrap());
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
