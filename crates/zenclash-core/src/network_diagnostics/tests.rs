use super::execution::{DiagnosticBackend, DiagnosticFuture, DiagnosticRequest};
use crate::DnsRecordType;
use std::collections::BTreeMap;
use std::sync::Arc;

use super::*;
use crate::{DnsAnswer, NetworkLatencyResult, Provider};

struct FakeBackend;

impl DiagnosticBackend for FakeBackend {
    fn execute(&self, request: DiagnosticRequest) -> DiagnosticFuture<'_> {
        Box::pin(async move {
            match request {
                DiagnosticRequest::Controller => Ok(DiagnosticData::Controller(VersionInfo {
                    meta: true,
                    version: "v-test".into(),
                })),
                DiagnosticRequest::Capture => Ok(DiagnosticData::Capture(CaptureStatus::default())),
                DiagnosticRequest::Dns {
                    record_type: DnsRecordType::A,
                    ..
                } => Err("A resolver unavailable".into()),
                DiagnosticRequest::Dns {
                    record_type: DnsRecordType::Aaaa,
                    ..
                } => Ok(DiagnosticData::Dns(DnsQueryResponse {
                    answer: vec![DnsAnswer {
                        ttl: 60,
                        data: "2001:db8::1".into(),
                        ..DnsAnswer::default()
                    }],
                    ..DnsQueryResponse::default()
                })),
                DiagnosticRequest::Network { route: None, .. } => {
                    Err("Mihomo route unavailable".into())
                }
                DiagnosticRequest::Network {
                    route: Some(route), ..
                } => Ok(DiagnosticData::Network(NetworkProbeSnapshot {
                    route: route.label(),
                    latencies: vec![NetworkLatencyResult {
                        target: NetworkLatencyTarget::new(
                            "probe",
                            "https://example.com/secret?token=raw",
                        )
                        .unwrap(),
                        latency_ms: Some(25),
                        error: None,
                    }],
                    ..NetworkProbeSnapshot::default()
                })),
                DiagnosticRequest::ProxyProviders | DiagnosticRequest::RuleProviders => {
                    Ok(DiagnosticData::Providers(ProviderCatalog {
                        providers: BTreeMap::from([(
                            "private-provider".into(),
                            Provider {
                                name: "private-provider".into(),
                                test_url: "https://token@example.com/check?token=raw".into(),
                                rule_count: 2,
                                ..Provider::default()
                            },
                        )]),
                    }))
                }
            }
        })
    }
}

fn test_plan() -> DiagnosticPlan {
    DiagnosticPlan::new(
        "secret.example",
        PublicIpProvider::IpSb,
        vec![
            NetworkLatencyTarget::new("target-with-secret", "https://example.com/secret?token=raw")
                .unwrap(),
        ],
    )
    .unwrap()
}

#[tokio::test]
async fn every_step_retains_independent_status_time_and_route() {
    let diagnostics = NetworkDiagnostics::from_backend(Arc::new(FakeBackend));

    let report = diagnostics.run(test_plan()).await;

    assert_eq!(report.steps.len(), 8);
    assert_eq!(
        report
            .steps
            .iter()
            .map(|step| step.kind)
            .collect::<Vec<_>>(),
        vec![
            DiagnosticStepKind::Controller,
            DiagnosticStepKind::Capture,
            DiagnosticStepKind::DnsA,
            DiagnosticStepKind::DnsAaaa,
            DiagnosticStepKind::NetworkDirect,
            DiagnosticStepKind::NetworkMihomo,
            DiagnosticStepKind::ProxyProviders,
            DiagnosticStepKind::RuleProviders,
        ]
    );
    assert!(
        report
            .step(DiagnosticStepKind::DnsA)
            .unwrap()
            .outcome
            .is_err()
    );
    assert!(
        report
            .step(DiagnosticStepKind::DnsAaaa)
            .unwrap()
            .outcome
            .is_ok()
    );
    assert_eq!(
        report
            .step(DiagnosticStepKind::NetworkDirect)
            .unwrap()
            .route,
        DiagnosticRoute::Direct
    );
    assert_eq!(
        report
            .step(DiagnosticStepKind::NetworkMihomo)
            .unwrap()
            .route,
        DiagnosticRoute::Mihomo
    );
    assert!(
        report
            .steps
            .iter()
            .all(|step| step.completed_at_ms >= report.started_at_ms)
    );
}

#[tokio::test]
async fn support_bundle_uses_only_allow_listed_facts() {
    let diagnostics = NetworkDiagnostics::from_backend(Arc::new(FakeBackend));
    let mut report = diagnostics.run(test_plan()).await;
    report
        .step(DiagnosticStepKind::DnsA)
        .expect("DNS step")
        .outcome
        .as_ref()
        .unwrap_err();
    report.steps[2].outcome = Err(DiagnosticFailure {
            message: "Bearer controller-secret /Users/alice/config.yaml https://subscription.example/path?token=raw".into(),
        });

    let bundle = diagnostics.export(&report, SupportSafe);

    for secret in [
        "controller-secret",
        "/Users/alice",
        "subscription.example",
        "token=raw",
        "secret.example",
        "2001:db8::1",
        "private-provider",
        "target-with-secret",
    ] {
        assert!(!bundle.markdown.contains(secret), "leaked {secret}");
    }
    assert!(bundle.markdown.starts_with("# ZenClash 网络诊断报告\n"));
    assert!(bundle.markdown.contains("| 直连网络 | 直连 | 成功 |"));
    assert!(
        bundle
            .markdown
            .contains("| DNS IPv4（A） | 内核控制器 | 失败 |")
    );
    assert!(bundle.markdown.contains("## 检查详情"));
}
