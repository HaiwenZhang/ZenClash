use zenclash_core::NetworkLatencyResult;

use super::*;

#[test]
fn public_ip_time_requires_the_displayed_address_and_route() {
    use zenclash_core::{DiagnosticRoute, DiagnosticStep, DiagnosticStepKind, PublicIpInfo};
    let snapshot = NetworkProbeSnapshot {
        route: "Mihomo".into(),
        public_ip: Some(PublicIpInfo {
            ip: "203.0.113.24".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut report = DiagnosticReport {
        started_at_ms: 100,
        steps: vec![DiagnosticStep {
            kind: DiagnosticStepKind::NetworkMihomo,
            route: DiagnosticRoute::Mihomo,
            completed_at_ms: 200,
            duration_ms: 100,
            outcome: Ok(DiagnosticData::Network(snapshot.clone())),
        }],
    };
    assert_eq!(public_ip_checked_at(&snapshot, Some(&report)), Some(200));
    assert_eq!(public_ip_checked_at(&snapshot, None), None);
    let mut other = snapshot.clone();
    other.route = "DIRECT".into();
    assert_eq!(public_ip_checked_at(&other, Some(&report)), None);
    other = snapshot.clone();
    other.public_ip.as_mut().unwrap().ip = "203.0.113.25".into();
    assert_eq!(public_ip_checked_at(&other, Some(&report)), None);
    other.public_ip = None;
    assert_eq!(public_ip_checked_at(&other, Some(&report)), None);
    report.steps[0].completed_at_ms = 0;
    assert_eq!(public_ip_checked_at(&snapshot, Some(&report)), None);
    report.steps[0].completed_at_ms = 300;
    report.steps[0].outcome = Err(zenclash_core::DiagnosticFailure {
        message: "unavailable".into(),
    });
    assert_eq!(public_ip_checked_at(&snapshot, Some(&report)), None);
}

#[test]
fn public_exit_summary_tracks_failure_retry_and_recovery() {
    assert_eq!(
        public_exit_label(None, false),
        zenclash_i18n::text("network.metrics.waiting")
    );
    let mut snapshot = NetworkProbeSnapshot {
        public_ip_error: Some("request failed".into()),
        ..Default::default()
    };
    assert_eq!(
        public_exit_label(Some(&snapshot), false),
        zenclash_i18n::text("network.latency.failed")
    );
    assert_eq!(
        public_exit_label(Some(&snapshot), true),
        zenclash_i18n::text("network.public_ip.probing")
    );
    snapshot.public_ip_error = None;
    assert_eq!(
        public_exit_label(Some(&snapshot), false),
        zenclash_i18n::text("common.status.unavailable")
    );
    snapshot.public_ip = Some(zenclash_core::PublicIpInfo {
        ip: "203.0.113.24".into(),
        ..Default::default()
    });
    assert_eq!(public_exit_label(Some(&snapshot), false), "203.0.113.24");
    assert_eq!(public_exit_label(Some(&snapshot), true), "203.0.113.24");
}

#[test]
fn chooses_mixed_then_http_proxy_port() {
    let config = RuntimeConfig {
        port: 7890,
        mixed_port: 7893,
        ..Default::default()
    };
    assert_eq!(
        network_probe_route(&config, true).unwrap(),
        NetworkProbeRoute::MihomoHttp {
            host: "127.0.0.1".into(),
            port: 7893
        }
    );
    assert_eq!(
        network_probe_route(&config, false).unwrap(),
        NetworkProbeRoute::Direct
    );
}

#[test]
fn combines_default_and_unique_custom_targets() {
    let custom = vec![
        NetworkLatencyTarget::new("Custom", "https://example.com/ping").unwrap(),
        NetworkLatencyTarget::new("Duplicate", DEFAULT_NETWORK_LATENCY_TARGETS[0].1).unwrap(),
    ];

    let targets = network_latency_targets(&custom);

    assert_eq!(targets.len(), DEFAULT_NETWORK_LATENCY_TARGETS.len() + 1);
    assert_eq!(targets.last().unwrap().name, "Custom");
}

#[test]
fn average_ignores_failed_targets() {
    let snapshot = NetworkProbeSnapshot {
        latencies: vec![
            NetworkLatencyResult {
                target: NetworkLatencyTarget::new("one", "https://example.com/one").unwrap(),
                latency_ms: Some(40),
                error: None,
            },
            NetworkLatencyResult {
                target: NetworkLatencyTarget::new("two", "https://example.com/two").unwrap(),
                latency_ms: None,
                error: Some("timeout".into()),
            },
        ],
        ..Default::default()
    };

    assert_eq!(average_latency(&snapshot), Some(40));
}

#[test]
fn explicit_path_observation_requires_at_least_one_success() {
    let route = NetworkProbeRoute::MihomoHttp {
        host: "127.0.0.1".into(),
        port: 7890,
    };
    let success = NetworkProbeSnapshot {
        route: "Mihomo 127.0.0.1:7890".into(),
        latencies: vec![NetworkLatencyResult {
            target: NetworkLatencyTarget::new("one", "https://example.com/one").unwrap(),
            latency_ms: Some(42),
            error: None,
        }],
        ..Default::default()
    };
    assert_eq!(
        path_observation(&route, 7, &success).unwrap(),
        PathStatus {
            route: ObservedPathRoute::Mihomo,
            target: "Mihomo 127.0.0.1:7890".into(),
            generation: 7,
        }
    );

    let failed = NetworkProbeSnapshot {
        public_ip_error: Some("offline".into()),
        ..Default::default()
    };
    assert_eq!(path_observation(&route, 7, &failed), Err("offline".into()));
}
