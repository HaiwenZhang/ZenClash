use super::*;
use crate::pages::runtime::test_support::RuntimePageFixture as Fixture;
use zenclash_core::{
    DiagnosticRoute, DiagnosticStep, DnsQueryResponse, NetworkLatencyResult, NetworkLatencyTarget,
    PublicIpProvider,
};

struct DiagnosticFixture {
    core: CoreSession,
    status: Arc<OperationalStatus>,
    _fixture: Fixture,
}

impl DiagnosticFixture {
    fn new() -> Self {
        // Drive background sampling only inside block_on, so it cannot contend
        // with the synchronous publication guard during state-machine assertions.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        Self::with_fixture(Fixture::with_controller_and_runtime(
            "http://127.0.0.1:1".to_owned(),
            runtime,
        ))
    }

    fn with_fixture(fixture: Fixture) -> Self {
        let services = fixture.services();
        Self {
            core: services.core_session,
            status: services.operational_status,
            _fixture: fixture,
        }
    }

    fn runtime(&self) -> &tokio::runtime::Runtime {
        self._fixture.runtime.as_ref().unwrap()
    }
}

fn observed(fixture: &DiagnosticFixture, dns_name: &str) -> ObservedNetworkDiagnostic {
    let observation = fixture
        .runtime()
        .block_on(fixture.core.begin_observation())
        .unwrap();
    let latency = NetworkLatencyResult {
        target: NetworkLatencyTarget::new("local fixture", "http://127.0.0.1:1/probe").unwrap(),
        latency_ms: Some(12),
        error: None,
    };
    ObservedNetworkDiagnostic {
        identity: observation.identity().clone(),
        plan: DiagnosticPlan::new(dns_name, PublicIpProvider::IpSb, Vec::new()).unwrap(),
        mihomo_route: Ok(NetworkProbeRoute::MihomoHttp {
            host: "127.0.0.1".into(),
            port: 7890,
        }),
        report: DiagnosticReport {
            started_at_ms: 100,
            steps: vec![
                DiagnosticStep {
                    kind: DiagnosticStepKind::DnsA,
                    route: DiagnosticRoute::Controller,
                    completed_at_ms: 110,
                    duration_ms: 10,
                    outcome: Ok(DiagnosticData::Dns(DnsQueryResponse::default())),
                },
                DiagnosticStep {
                    kind: DiagnosticStepKind::NetworkDirect,
                    route: DiagnosticRoute::Direct,
                    completed_at_ms: 112,
                    duration_ms: 12,
                    outcome: Ok(DiagnosticData::Network(NetworkProbeSnapshot {
                        route: "DIRECT".into(),
                        latencies: vec![latency.clone()],
                        ..Default::default()
                    })),
                },
                DiagnosticStep {
                    kind: DiagnosticStepKind::NetworkMihomo,
                    route: DiagnosticRoute::Mihomo,
                    completed_at_ms: 113,
                    duration_ms: 13,
                    outcome: Ok(DiagnosticData::Network(NetworkProbeSnapshot {
                        route: "Mihomo".into(),
                        latencies: vec![latency],
                        ..Default::default()
                    })),
                },
            ],
        },
    }
}

#[test]
fn completed_dns_name_and_selected_route_belong_to_the_submitted_query() {
    let fixture = DiagnosticFixture::new();
    let mut query = NetworkDiagnosticQueryState::default();
    let request = query.begin(NetworkProbeRoutePreference::Mihomo);
    query.complete(
        request,
        &fixture.core,
        &fixture.status,
        Ok(observed(&fixture, " first.example. ")),
    );
    assert_eq!(query.dns_name(), Some("first.example"));
    assert_eq!(query.snapshot().unwrap().route, "Mihomo");
    assert!(
        query
            .report()
            .unwrap()
            .step(DiagnosticStepKind::DnsA)
            .is_some()
    );
    assert!(fixture.status.snapshot().path.value().is_some());
    query.begin(NetworkProbeRoutePreference::Direct);
    assert_eq!(
        query.dns_name(),
        Some("first.example"),
        "a new request must preserve the old complete facts during refresh"
    );
    query.cancel();
    assert_eq!(query.dns_name(), Some("first.example"));
}

#[test]
fn late_completion_cannot_end_a_new_request_or_publish_operational_facts() {
    let fixture = DiagnosticFixture::new();
    let mut query = NetworkDiagnosticQueryState::default();
    let old = query.begin(NetworkProbeRoutePreference::Direct);
    let old_result = observed(&fixture, "old.example");
    query.cancel();
    let current = query.begin(NetworkProbeRoutePreference::Mihomo);
    let before = fixture.status.snapshot().path;
    query.complete(old, &fixture.core, &fixture.status, Ok(old_result));
    assert!(query.is_loading());
    assert!(query.report().is_none());
    assert_eq!(fixture.status.snapshot().path, before);
    query.complete(
        current,
        &fixture.core,
        &fixture.status,
        Ok(observed(&fixture, "current.example")),
    );
    assert!(!query.is_loading());
    assert_eq!(query.dns_name(), Some("current.example"));
}

#[test]
fn same_counter_from_another_session_cannot_publish_a_diagnostic() {
    let original = DiagnosticFixture::new();
    let current = DiagnosticFixture::new();
    assert_eq!(original.core.generation(), current.core.generation());
    let mut query = NetworkDiagnosticQueryState::default();
    let request = query.begin(NetworkProbeRoutePreference::Direct);
    let before = current.status.snapshot().path;
    query.complete(
        request,
        &current.core,
        &current.status,
        Ok(observed(&original, "foreign.example")),
    );
    assert!(!query.is_loading());
    assert!(query.report().is_none());
    assert_eq!(current.status.snapshot().path, before);
}

#[test]
fn runtime_replacement_rejects_the_original_diagnostic_result() {
    let fixture = DiagnosticFixture::new();
    let mut query = NetworkDiagnosticQueryState::default();
    let request = query.begin(NetworkProbeRoutePreference::Direct);
    let result = observed(&fixture, "retired.example");
    fixture
        .runtime()
        .block_on(
            fixture
                .core
                .switch_to_direct(zenclash_core::MihomoEndpoint::new("http://127.0.0.1:2", "")),
        )
        .unwrap();
    let before = fixture.status.snapshot().path;
    query.complete(request, &fixture.core, &fixture.status, Ok(result));
    assert!(query.report().is_none());
    assert_eq!(fixture.status.snapshot().path, before);
}

#[test]
fn admitted_data_application_blocks_both_presentation_and_path_publication() {
    let fixture = DiagnosticFixture::new();
    let mut query = NetworkDiagnosticQueryState::default();
    let request = query.begin(NetworkProbeRoutePreference::Direct);
    let result = observed(&fixture, "original.example");
    let admission = fixture
        .runtime()
        .block_on(fixture.core.begin_data_application())
        .unwrap();
    let before = fixture.status.snapshot().path;
    query.complete(request, &fixture.core, &fixture.status, Ok(result));
    assert!(query.report().is_none());
    assert_eq!(fixture.status.snapshot().path, before);
    drop(admission);
}

#[test]
fn current_query_failure_cannot_pair_a_new_error_with_an_old_report() {
    let fixture = DiagnosticFixture::new();
    let mut query = NetworkDiagnosticQueryState::default();
    let first = query.begin(NetworkProbeRoutePreference::Direct);
    query.complete(
        first,
        &fixture.core,
        &fixture.status,
        Ok(observed(&fixture, "old.example")),
    );
    assert!(query.report().is_some());
    let next = query.begin(NetworkProbeRoutePreference::Direct);
    query.complete(
        next,
        &fixture.core,
        &fixture.status,
        Err("new task failed".into()),
    );
    assert!(query.report().is_none());
    assert!(query.dns_name().is_none());
    assert_eq!(
        query.snapshot().unwrap().public_ip_error.as_deref(),
        Some("new task failed")
    );
}

#[test]
fn query_drop_aborts_its_owned_read_task() {
    let fixture = DiagnosticFixture::new();
    let pending = fixture.runtime().spawn(std::future::pending::<()>());
    let mut query = NetworkDiagnosticQueryState::default();
    query.begin_pending_for_test(&pending);
    drop(query);
    assert!(
        fixture
            .runtime()
            .block_on(pending)
            .unwrap_err()
            .is_cancelled()
    );
}

#[test]
fn starting_a_diagnostic_reads_its_pinned_runtime_configuration_before_probing() {
    use std::time::Duration;

    let fixture = DiagnosticFixture::new();
    let listener = fixture
        .runtime()
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let session = CoreSession::open(
        zenclash_core::CoreKind::Mihomo,
        zenclash_core::MihomoClient::new(zenclash_core::MihomoEndpoint::new(
            format!("http://{}", listener.local_addr().unwrap()),
            "",
        ))
        .unwrap(),
    )
    .unwrap();
    let mut query = NetworkDiagnosticQueryState::default();
    let (_, task) = query.start(
        fixture.runtime().handle(),
        session,
        fixture.status.clone(),
        DiagnosticPlan::new("submitted.example", PublicIpProvider::IpSb, Vec::new()).unwrap(),
        NetworkProbeRoutePreference::Direct,
    );
    fixture.runtime().block_on(async {
        let (mut stream, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let request = tokio::time::timeout(
            Duration::from_secs(2),
            zenclash_core::test_support::read_controller_request(&mut stream),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(request.headers().starts_with("GET /configs HTTP/1.1\r\n"));
        // No reply: cancellation interrupts the actual configuration read before any
        // public-IP request is sent, so this fixture never contacts the Internet.
        query.cancel();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    });
}

#[gpui_kit::test]
fn dns_details_keep_the_completed_query_name_after_the_input_draft_changes(
    cx: &mut gpui_kit::TestAppContext,
) {
    use crate::pages::runtime::{Page, PageSnapshotData};
    use gpui_kit::AppContext;
    use gpui_kit::test::TestWindowExt;

    let fixture = DiagnosticFixture::with_fixture(Fixture::new());
    let (window, page) = crate::pages::runtime::test_support::open_runtime_page_window(
        cx,
        &fixture._fixture,
        Page::Network,
    );
    fixture
        ._fixture
        .settle(cx, &page, |page| !page.persistent_loading);
    let result = observed(&fixture, " submitted.example. ");
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.page_observation
                .replace_fixture_data(PageSnapshotData::Network {
                    config: Default::default(),
                    system: Default::default(),
                });
            let request = page
                .network_diagnostics
                .query
                .begin(NetworkProbeRoutePreference::Direct);
            page.complete_network_diagnostics(request, Ok(result), cx);
            page.network_diagnostics.dns_name.update(cx, |input, cx| {
                input.set_value("new-unsent-draft.example", window, cx);
            });
            assert_eq!(
                page.network_diagnostics.dns_name.read(cx).value(),
                "new-unsent-draft.example"
            );
            cx.notify();
        });
        window.render_frame(cx);
        window.click("diagnostic-details:DnsA", cx);
        window.render_frame(cx);
        assert_eq!(
            window.find("diagnostic-dns-name").value(),
            Some("submitted.example")
        );
        window.press("escape", cx);
        window.remove_window();
    })
    .unwrap();
}
