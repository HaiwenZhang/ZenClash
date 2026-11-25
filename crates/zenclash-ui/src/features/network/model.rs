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
    if !snapshot.succeeded() {
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
mod tests;
