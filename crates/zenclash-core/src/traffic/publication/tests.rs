use super::*;
use crate::stream_publication_test_support::{PublicationPhase, publication_pause};
use std::sync::Arc;
use tokio::runtime::Handle;

fn monitor_without_socket(runtime: &Handle) -> Arc<TrafficMonitor> {
    let state = TrafficState::new();
    *state.snapshot.write() = TrafficSnapshot {
        generation: 1,
        connected: true,
        ..Default::default()
    };
    state.expected_generation.store(1, Ordering::Release);
    state.generation.send_replace(1);
    Arc::new(TrafficMonitor {
        state: Arc::new(state),
        task: runtime.spawn(std::future::pending()),
    })
}

#[test]
fn a_generation_change_notifies_once_and_retains_the_last_real_frame_until_replacement() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let monitor = monitor_without_socket(runtime.handle());
    assert!(
        monitor
            .state
            .record_frame(TrafficFrame { up: 10, down: 20 }, 1)
    );
    let previous = monitor.snapshot();
    let history = monitor.timed_samples();
    let revision = monitor.revision();
    let mut updates = monitor.subscribe();

    monitor.synchronize_generation(2);
    assert_eq!(
        monitor.snapshot(),
        TrafficSnapshot {
            connected: false,
            ..previous
        }
    );
    assert_eq!(monitor.timed_samples(), history);
    assert_eq!(monitor.revision(), revision + 1);
    assert!(updates.has_changed().unwrap());
    updates.borrow_and_update();

    monitor.synchronize_generation(2);
    assert_eq!(monitor.revision(), revision + 1);
    assert!(!updates.has_changed().unwrap());
}

#[test]
fn revision_wraparound_still_notifies_frame_subscribers() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let monitor = monitor_without_socket(runtime.handle());
    monitor.state.revision.send_replace(u64::MAX);
    let updates = monitor.subscribe();

    assert!(
        monitor
            .state
            .record_frame(TrafficFrame { up: 10, down: 20 }, 1)
    );
    assert_eq!(monitor.revision(), 0);
    assert!(updates.has_changed().unwrap());
    assert_eq!(monitor.samples().back().unwrap().upload, 10);
}

#[test]
fn a_paused_old_traffic_frame_cannot_replace_new_generation_rates_or_samples() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let monitor = monitor_without_socket(runtime.handle());
    let (pause, controller) = publication_pause(PublicationPhase::BeforeStateLock);
    let old = monitor.clone();
    let writer = std::thread::spawn(move || {
        pause.install();
        old.state
            .record_frame(TrafficFrame { up: 999, down: 888 }, 1)
    });
    controller.wait_until_reached();
    monitor.synchronize_generation(2);
    assert!(
        monitor
            .state
            .record_frame(TrafficFrame { up: 7, down: 8 }, 2)
    );
    let snapshot = monitor.snapshot();
    let samples = monitor.samples();
    let timed_samples = monitor.timed_samples();
    let revision = monitor.revision();
    controller.resume();
    assert!(!writer.join().unwrap());
    assert_eq!(monitor.snapshot(), snapshot);
    assert_eq!(monitor.samples(), samples);
    assert_eq!(monitor.timed_samples(), timed_samples);
    assert_eq!(monitor.revision(), revision);
}

#[test]
fn a_paused_old_traffic_connection_cannot_replace_new_generation_health() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let monitor = monitor_without_socket(runtime.handle());
    let (pause, controller) = publication_pause(PublicationPhase::BeforeStateLock);
    let old = monitor.clone();
    let writer = std::thread::spawn(move || {
        pause.install();
        old.state
            .record_connection(false, Some("old socket failed".into()), 1)
    });
    controller.wait_until_reached();
    monitor.synchronize_generation(2);
    assert!(monitor.state.record_connection(true, None, 2));
    let snapshot = monitor.snapshot();
    let revision = monitor.revision();
    controller.resume();
    assert!(!writer.join().unwrap());
    assert_eq!(monitor.snapshot(), snapshot);
    assert_eq!(monitor.revision(), revision);
}

#[test]
fn traffic_keeps_the_generation_gate_until_its_sample_is_published() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let monitor = monitor_without_socket(runtime.handle());
    let (pause, controller) = publication_pause(PublicationPhase::BeforeBufferAppend);
    let current = monitor.clone();
    let writer = std::thread::spawn(move || {
        pause.install();
        current
            .state
            .record_frame(TrafficFrame { up: 7, down: 8 }, 1)
    });
    controller.wait_until_reached();
    let state_gate_held = monitor.state.snapshot.try_write().is_none();
    let revision_before_append = monitor.revision();
    controller.resume();
    assert!(writer.join().unwrap());
    assert!(
        state_gate_held,
        "generation transition must wait for the pending sample append"
    );
    assert_eq!(revision_before_append, 0);
    assert_eq!(
        monitor.samples().back(),
        Some(&TrafficSample {
            upload: 7,
            download: 8
        })
    );
    assert_eq!(monitor.timed_samples().len(), 1);
    assert_eq!(
        monitor.timed_samples()[0].timestamp_ms(),
        monitor.snapshot().updated_at_ms
    );
}

#[test]
fn generation_reconnects_retain_real_history_without_appending_old_rates_as_new_frames() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let monitor = monitor_without_socket(runtime.handle());
    assert!(
        monitor
            .state
            .record_frame(TrafficFrame { up: 10, down: 20 }, 1)
    );
    let history = monitor.timed_samples();
    monitor.synchronize_generation(2);
    assert!(monitor.state.record_connection(true, None, 2));
    assert_eq!(monitor.timed_samples(), history);
    assert!(
        monitor
            .state
            .record_frame(TrafficFrame { up: 30, down: 40 }, 2)
    );
    let history = monitor.timed_samples();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].rates().upload, 10);
    assert_eq!(history[1].rates().upload, 30);
}

#[tokio::test]
async fn each_stream_frame_advances_the_single_logical_sample_series() {
    let monitor = monitor_without_socket(&Handle::current());
    assert!(monitor.state.record_frame(
        TrafficFrame {
            up: 1_024,
            down: 2_048
        },
        1
    ));
    let samples = monitor.samples();
    assert_eq!(samples.len(), super::super::LIVE_TRAFFIC_SAMPLE_COUNT);
    assert_eq!(
        samples.back(),
        Some(&TrafficSample {
            upload: 1_024,
            download: 2_048
        })
    );
}

#[tokio::test]
async fn disconnect_does_not_append_synthetic_zero_samples() {
    let monitor = monitor_without_socket(&Handle::current());
    monitor
        .state
        .record_frame(TrafficFrame { up: 10, down: 20 }, 1);
    let samples = monitor.samples();
    let history = monitor.timed_samples();

    monitor
        .state
        .record_connection(false, Some("offline".into()), 1);

    assert_eq!(monitor.samples(), samples);
    assert_eq!(monitor.timed_samples(), history);
}

#[tokio::test]
async fn reconnect_clears_a_previous_connection_error() {
    let monitor = monitor_without_socket(&Handle::current());
    monitor
        .state
        .record_connection(false, Some("offline".into()), 1);
    monitor.state.record_connection(true, None, 1);
    assert_eq!(
        monitor.snapshot(),
        TrafficSnapshot {
            generation: 1,
            connected: true,
            ..Default::default()
        }
    );
}

#[tokio::test]
async fn disconnect_preserves_last_successful_rates_and_timestamp() {
    let monitor = monitor_without_socket(&Handle::current());
    monitor.state.record_frame(
        TrafficFrame {
            up: 1_024,
            down: 2_048,
        },
        1,
    );
    let before = monitor.snapshot();

    monitor
        .state
        .record_connection(false, Some("socket closed".into()), 1);

    assert_eq!(
        monitor.snapshot(),
        TrafficSnapshot {
            connected: false,
            last_error: Some("socket closed".into()),
            ..before
        }
    );
}

#[tokio::test]
async fn late_frame_from_an_old_generation_is_rejected() {
    let monitor = monitor_without_socket(&Handle::current());
    monitor.synchronize_generation(2);
    monitor
        .state
        .record_frame(TrafficFrame { up: 30, down: 40 }, 2);
    let before = monitor.snapshot();
    let samples = monitor.samples();
    let revision = monitor.revision();

    assert!(
        !monitor
            .state
            .record_frame(TrafficFrame { up: 100, down: 200 }, 1)
    );

    assert_eq!(monitor.snapshot(), before);
    assert_eq!(monitor.samples(), samples);
    assert_eq!(monitor.revision(), revision);
}

#[tokio::test]
async fn accepted_frame_advances_the_traffic_revision() {
    let monitor = monitor_without_socket(&Handle::current());
    monitor.state.revision.send_replace(7);
    assert!(
        monitor
            .state
            .record_frame(TrafficFrame { up: 100, down: 200 }, 1)
    );
    assert_eq!(monitor.revision(), 8);
}

#[tokio::test]
async fn repeated_connection_state_does_not_advance_the_traffic_revision() {
    let monitor = monitor_without_socket(&Handle::current());
    monitor.state.revision.send_replace(3);
    assert!(monitor.state.record_connection(true, None, 1));
    assert_eq!(monitor.revision(), 3);
}

#[tokio::test]
async fn traffic_revision_subscription_observes_an_accepted_frame() {
    let monitor = monitor_without_socket(&Handle::current());
    let updates = monitor.subscribe();
    assert!(
        monitor
            .state
            .record_frame(TrafficFrame { up: 100, down: 200 }, 1)
    );
    assert!(updates.has_changed().unwrap());
}
