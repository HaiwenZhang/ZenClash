use std::collections::HashSet;

use zenclash_core::{
    DEFAULT_NETWORK_LATENCY_TARGETS, DiagnosticData, DiagnosticReport, NetworkLatencyTarget,
    NetworkProbeRoute, NetworkProbeSnapshot, ObservedPathRoute, PathStatus, RuntimeConfig,
};

pub(super) fn network_probe_route(
    config: &RuntimeConfig,
    through_mihomo: bool,
) -> Result<NetworkProbeRoute, String> {
    if !through_mihomo {
        return Ok(NetworkProbeRoute::Direct);
    }
    let port = if config.mixed_port != 0 {
        config.mixed_port
    } else {
        config.port
    };
    if port == 0 {
        return Err(zenclash_i18n::text("network.errors.no_proxy_port"));
    }
    Ok(NetworkProbeRoute::MihomoHttp {
        host: "127.0.0.1".into(),
        port,
    })
}

pub(super) fn network_latency_targets(
    custom: &[NetworkLatencyTarget],
) -> Vec<NetworkLatencyTarget> {
    let mut seen = HashSet::new();
    DEFAULT_NETWORK_LATENCY_TARGETS
        .iter()
        .filter_map(|(name, url)| NetworkLatencyTarget::new(*name, *url).ok())
        .chain(custom.iter().cloned())
        .filter(|target| seen.insert(target.url.clone()))
        .collect()
}

pub(super) fn average_latency(snapshot: &NetworkProbeSnapshot) -> Option<u64> {
    let values = snapshot
        .latencies
        .iter()
        .filter_map(|result| result.latency_ms)
        .collect::<Vec<_>>();
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<u64>() / u64::try_from(values.len()).unwrap_or(1))
    }
}

pub(super) fn public_ip_checked_at(
    snapshot: &NetworkProbeSnapshot,
    report: Option<&DiagnosticReport>,
) -> Option<u64> {
    let info = snapshot.public_ip.as_ref()?;
    report?
        .steps
        .iter()
        .filter_map(|step| {
            let Ok(DiagnosticData::Network(observed)) = &step.outcome else {
                return None;
            };
            (observed.route == snapshot.route
                && observed.public_ip.as_ref() == Some(info)
                && step.completed_at_ms > 0)
                .then_some(step.completed_at_ms)
        })
        .max()
}

pub(super) fn public_exit_label(snapshot: Option<&NetworkProbeSnapshot>, loading: bool) -> String {
    if let Some(info) = snapshot.and_then(|snapshot| snapshot.public_ip.as_ref()) {
        return info.ip.clone();
    }
    zenclash_i18n::text(if loading {
        "network.public_ip.probing"
    } else if snapshot.is_some_and(|snapshot| snapshot.public_ip_error.is_some()) {
        "network.latency.failed"
    } else if snapshot.is_some() {
        "common.status.unavailable"
    } else {
        "network.metrics.waiting"
    })
}

pub(super) fn path_observation(
    route: &NetworkProbeRoute,
    generation: u64,
    snapshot: &NetworkProbeSnapshot,
) -> Result<PathStatus, String> {
    let succeeded = snapshot.public_ip.is_some()
        || snapshot
            .latencies
            .iter()
            .any(|result| result.latency_ms.is_some());
    if !succeeded {
        let error = snapshot
            .public_ip_error
            .clone()
            .or_else(|| {
                snapshot
                    .latencies
                    .iter()
                    .find_map(|result| result.error.clone())
            })
            .unwrap_or_else(|| zenclash_i18n::text("network.errors.no_successful_probe"));
        return Err(error);
    }
    Ok(PathStatus {
        route: match route {
            NetworkProbeRoute::Direct => ObservedPathRoute::Direct,
            NetworkProbeRoute::MihomoHttp { .. } => ObservedPathRoute::Mihomo,
        },
        target: if snapshot.route.is_empty() {
            route.label()
        } else {
            snapshot.route.clone()
        },
        generation,
    })
}

pub(super) fn latency_color(
    latency: Option<u64>,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Hsla {
    match latency {
        Some(0..100) => theme.success,
        Some(100..300) => theme.warning,
        Some(_) => theme.danger,
        None => theme.muted_foreground,
    }
}

pub(super) fn join_present(values: &[Option<&str>]) -> String {
    values
        .iter()
        .filter_map(|value| *value)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

pub(super) fn format_asn(asn: Option<u64>, organization: Option<&str>) -> String {
    match (asn, organization.filter(|value| !value.is_empty())) {
        (Some(asn), Some(organization)) => format!("AS{asn} · {organization}"),
        (Some(asn), None) => format!("AS{asn}"),
        (None, Some(organization)) => organization.to_owned(),
        (None, None) => String::new(),
    }
}

pub(super) fn format_coordinates(latitude: Option<f64>, longitude: Option<f64>) -> String {
    match (latitude, longitude) {
        (Some(latitude), Some(longitude)) => format!("{latitude:.4}, {longitude:.4}"),
        _ => String::new(),
    }
}

pub(super) fn format_proxy_flags(is_proxy: Option<bool>, is_vpn: Option<bool>) -> String {
    match (is_proxy, is_vpn) {
        (None, None) => String::new(),
        (Some(false), Some(false)) => zenclash_i18n::text("network.public_ip.not_proxy"),
        _ => join_present(&[
            is_proxy.filter(|value| *value).map(|_| "Proxy"),
            is_vpn.filter(|value| *value).map(|_| "VPN"),
        ]),
    }
}

#[cfg(test)]
mod tests {
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

        assert_eq!(targets.len(), 4);
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
}
