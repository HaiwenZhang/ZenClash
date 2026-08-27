use std::collections::HashSet;

use zenclash_core::{
    DEFAULT_NETWORK_LATENCY_TARGETS, NetworkLatencyTarget, NetworkProbeRoute, NetworkProbeSnapshot,
    RuntimeConfig,
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

pub(super) fn latency_color(latency: Option<u64>, theme: &gpui_component::Theme) -> gpui::Hsla {
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
}
