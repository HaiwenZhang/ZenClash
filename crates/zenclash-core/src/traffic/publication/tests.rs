use super::*;
use crate::stream_publication_test_support::{PublicationPhase, publication_pause};
use crate::traffic::initial_samples;
use std::sync::Arc;
use tokio::{runtime::Handle, sync::watch};

fn monitor_without_socket(runtime: &Handle) -> Arc<TrafficMonitor> {
    let (generation, _) = watch::channel(1);
    Arc::new(TrafficMonitor {
        snapshot: Arc::new(RwLock::new(TrafficSnapshot {
            generation: 1,
            connected: true,
            ..Default::default()
        })),
        samples: Arc::new(RwLock::new(initial_samples())),
        revision: TrafficRevision::new(),
        expected_generation: Arc::new(AtomicU64::new(1)),
        generation,
        task: runtime.spawn(std::future::pending()),
    })
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
        update_frame_for_generation(
            &old.snapshot,
            &old.samples,
            TrafficFrame { up: 999, down: 888 },
            1,
            &old.expected_generation,
            &old.revision,
        )
    });
    controller.wait_until_reached();
    monitor.synchronize_generation(2);
    assert!(update_frame_for_generation(
        &monitor.snapshot,
        &monitor.samples,
        TrafficFrame { up: 7, down: 8 },
        2,
        &monitor.expected_generation,
        &monitor.revision
    ));
    let snapshot = monitor.snapshot();
    let samples = monitor.samples();
    let revision = monitor.revision();
    controller.resume();
    assert!(!writer.join().unwrap());
    assert_eq!(monitor.snapshot(), snapshot);
    assert_eq!(monitor.samples(), samples);
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
        update_connection_for_generation(
            &old.snapshot,
            false,
            Some("old socket failed".into()),
            1,
            &old.expected_generation,
            &old.revision,
        )
    });
    controller.wait_until_reached();
    monitor.synchronize_generation(2);
    assert!(update_connection_for_generation(
        &monitor.snapshot,
        true,
        None,
        2,
        &monitor.expected_generation,
        &monitor.revision
    ));
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
        update_frame_for_generation(
            &current.snapshot,
            &current.samples,
            TrafficFrame { up: 7, down: 8 },
            1,
            &current.expected_generation,
            &current.revision,
        )
    });
    controller.wait_until_reached();
    let state_gate_held = monitor.snapshot.try_write().is_none();
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
}
