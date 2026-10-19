use std::collections::BTreeMap;

use crate::{
    CapabilityState, CaptureStatus, DiagnosticData, DiagnosticFailure, DiagnosticReport,
    DiagnosticStep, DiagnosticStepKind, DnsAnswer, DnsQueryResponse, DnsQuestion,
    NetworkLatencyResult, NetworkLatencyTarget, NetworkProbeSnapshot, Observation,
    OperationalFailure, Provider, ProviderCatalog, PublicIpInfo, SupportSafe,
    SystemProxyOwnershipState, SystemProxySessionSnapshot, SystemProxyStatus, TunCaptureStatus,
    TunRuntimeObservation, VersionInfo,
};

#[test]
fn exported_network_status_reflects_partial_success_and_total_failure() {
    for (ip_ok, latency_ok) in [(false, false), (true, false), (false, true), (true, true)] {
        let report = DiagnosticReport {
            started_at_ms: 1,
            steps: vec![DiagnosticStep {
                kind: DiagnosticStepKind::NetworkDirect,
                route: super::DiagnosticRoute::Direct,
                completed_at_ms: 2,
                duration_ms: 1,
                outcome: Ok(DiagnosticData::Network(NetworkProbeSnapshot {
                    public_ip: ip_ok.then(|| PublicIpInfo {
                        ip: "203.0.113.1".into(),
                        ..Default::default()
                    }),
                    public_ip_error: (!ip_ok).then(|| "offline".into()),
                    latencies: vec![NetworkLatencyResult {
                        target: NetworkLatencyTarget::new("test", "https://example.com/").unwrap(),
                        latency_ms: latency_ok.then_some(42),
                        error: (!latency_ok).then(|| "timeout".into()),
                    }],
                    ..Default::default()
                })),
            }],
        };
        let markdown = report.export_support_bundle(SupportSafe).markdown;
        let status = if ip_ok || latency_ok {
            "成功"
        } else {
            "失败"
        };
        assert!(
            markdown.contains(&format!("| 直连网络 | 直连 | {status} |")),
            "{markdown}"
        );
        assert!(markdown.contains(&format!("延迟测试成功：{}", usize::from(latency_ok))));
    }
}

#[test]
fn completed_report_exports_only_allowlisted_facts_without_execution_services() {
    let capture = CaptureStatus {
        system_proxy: Observation::Stale {
            value: SystemProxySessionSnapshot {
                intent_enabled: true,
                actual: SystemProxyStatus {
                    service: "private-network-service".into(),
                    server: "192.0.2.5".into(),
                    auto_url: "https://private-pac.example/script?token=pac-secret".into(),
                    bypass: vec!["internal-bypass.example".into()],
                    ..Default::default()
                },
                ownership: SystemProxyOwnershipState::Owned,
            },
            observed_at_ms: 1,
            failure: OperationalFailure {
                message: "/Users/private-user/proxy.json".into(),
                occurred_at_ms: 2,
            },
        },
        tun: Observation::Fresh {
            value: TunCaptureStatus {
                requested: true,
                configured: true,
                permission: CapabilityState::Active,
                runtime: TunRuntimeObservation {
                    device_name: Some("private-tun-device".into()),
                    device: CapabilityState::Active,
                    route: CapabilityState::Active,
                    detail: "private-route-detail".into(),
                },
                observed: CapabilityState::Active,
            },
            observed_at_ms: 2,
        },
    };
    let dns = DnsQueryResponse {
        question: vec![DnsQuestion {
            name: "private-dns-question.example".into(),
            record_type: 1,
        }],
        answer: vec![DnsAnswer {
            name: "private-dns-owner.example".into(),
            record_type: 1,
            ttl: 75,
            data: "2001:db8::1234".into(),
        }],
        ..Default::default()
    };
    let network = NetworkProbeSnapshot {
        route: "private-controller.example:7890".into(),
        public_ip: Some(PublicIpInfo {
            ip: "198.51.100.27".into(),
            city: Some("private-city".into()),
            organization: Some("private-organization".into()),
            ..Default::default()
        }),
        public_ip_error: Some("private-public-ip-failure".into()),
        latencies: vec![
            NetworkLatencyResult {
                target: NetworkLatencyTarget::new(
                    "private-latency-target",
                    "https://private-probe.example/check?token=probe-secret",
                )
                .unwrap(),
                latency_ms: Some(42),
                error: None,
            },
            NetworkLatencyResult {
                target: NetworkLatencyTarget::new(
                    "failed-private-target",
                    "https://failed-probe.example/check",
                )
                .unwrap(),
                latency_ms: None,
                error: Some("private-latency-failure".into()),
            },
        ],
    };
    let providers = ProviderCatalog {
        providers: BTreeMap::from([(
            "private-provider-key".into(),
            Provider {
                name: "private-provider-name".into(),
                test_url: "https://private-provider.example/check?token=provider-secret".into(),
                rule_count: 7,
                ..Default::default()
            },
        )]),
    };
    let outcomes = [
        (DiagnosticStepKind::Controller, Ok(DiagnosticData::Controller(VersionInfo {
            meta: true,
            version: "private-controller-version".into(),
        }))),
        (DiagnosticStepKind::Capture, Ok(DiagnosticData::Capture(capture))),
        (DiagnosticStepKind::DnsA, Ok(DiagnosticData::Dns(dns.clone()))),
        (DiagnosticStepKind::DnsAaaa, Ok(DiagnosticData::Dns(dns))),
        (DiagnosticStepKind::NetworkDirect, Ok(DiagnosticData::Network(network))),
        (DiagnosticStepKind::NetworkMihomo, Err(DiagnosticFailure {
            message: "Bearer controller-secret /Users/private-user/subscription.yaml https://private-subscription.example/path?token=subscription-secret".into(),
        })),
        (DiagnosticStepKind::ProxyProviders, Ok(DiagnosticData::Providers(providers.clone()))),
        (DiagnosticStepKind::RuleProviders, Ok(DiagnosticData::Providers(providers))),
    ];
    let report = DiagnosticReport {
        started_at_ms: 1_700_000_000_000,
        steps: outcomes
            .into_iter()
            .map(|(kind, outcome)| DiagnosticStep {
                kind,
                route: kind.route(),
                completed_at_ms: 1_700_000_000_500,
                duration_ms: 500,
                outcome,
            })
            .collect(),
    };

    let bundle = report.export_support_bundle(SupportSafe);

    for sensitive in [
        "private-",
        "private-user",
        "internal-bypass",
        "192.0.2.5",
        "2001:db8::1234",
        "198.51.100.27",
        "controller-secret",
        "failed-probe",
        "token=",
        "https://",
        "/Users/",
    ] {
        assert!(!bundle.markdown.contains(sensitive), "leaked {sensitive}");
    }
    for safe_fact in [
        "检查项目：8",
        "Meta 内核：是",
        "已读取系统代理状态：是",
        "已读取 TUN 状态：是",
        "解析记录数：1",
        "TTL（秒）：75",
        "已获取公网 IP：是",
        "公网 IP 查询失败：是",
        "延迟测试成功：1",
        "延迟测试失败：1",
        "延迟（ms）：42",
        "集合数：1",
        "条目数：7",
        "| 代理网络 | Mihomo 代理 | 失败 | 500 ms |",
        "检查失败，详细原因请在应用内查看。",
    ] {
        assert!(bundle.markdown.contains(safe_fact), "missing {safe_fact}");
    }
    assert!(bundle.generated_at_ms >= report.started_at_ms);
}
