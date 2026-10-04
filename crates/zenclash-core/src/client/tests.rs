use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
    time::{Duration, Instant},
};

use super::{api::encode_path_segment, *};
use crate::DnsRecordType;

#[tokio::test]
async fn replacing_an_external_binding_does_not_run_its_previous_validator() {
    let directory = std::env::temp_dir().join(format!(
        "zenclash-stale-validator-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let client = MihomoClient::new(MihomoEndpoint::default())
        .unwrap()
        .with_config_validator(crate::CoreConfigValidator::new(
            crate::CoreKind::Mihomo,
            directory.join("previous-binary-does-not-exist"),
            directory.join("previous-home"),
        ))
        .unwrap();
    let clone = client.clone();
    client
        .switch_to_direct(MihomoEndpoint::new("http://127.0.0.1:1", "replacement"))
        .await
        .unwrap();

    let result = clone
        .validate_config_payload("rules:\n  - MATCH,DIRECT\n")
        .await;
    let previous_home_created = directory.join("previous-home").exists();
    if directory.exists() {
        std::fs::remove_dir_all(&directory).unwrap();
    }

    assert!(
        result.is_ok(),
        "new external binding used the old validator: {result:?}"
    );
    assert!(!previous_home_created);
}

#[test]
fn service_preflight_rejection_is_definitive_but_lost_response_is_unknown() {
    use zenclash_service::{FrameError, ServiceClientError, ServiceErrorCode};
    let rejected = MihomoError::Service(ServiceClientError::Rejected(
        ServiceErrorCode::InvalidConfiguration,
    ));
    let lost = MihomoError::Service(ServiceClientError::Frame(FrameError::Timeout));
    let connection = MihomoError::Service(ServiceClientError::Connection(std::io::Error::other(
        "unavailable",
    )));
    assert!(!rejected.mutation_result_unknown());
    assert!(!connection.mutation_result_unknown());
    assert!(lost.mutation_result_unknown());
}

#[tokio::test]
async fn clones_use_the_committed_transport_after_a_switch() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2048];
        let size = stream.read(&mut request).unwrap();
        assert!(String::from_utf8_lossy(&request[..size]).contains("Bearer replacement"));
        let body = r#"{"version":"replacement"}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();
    let clone = client.clone();
    let mut changed = clone.binding.subscribe();
    client
        .switch_to_direct(MihomoEndpoint::new(
            format!("http://{address}"),
            "replacement",
        ))
        .await
        .unwrap();
    changed.changed().await.unwrap();
    assert_eq!(clone.endpoint().unwrap().secret, "replacement");
    assert_eq!(clone.version().await.unwrap().version, "replacement");
    server.join().unwrap();
}

#[tokio::test]
async fn response_from_the_previous_transport_is_rejected() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let (received, wait_received) = tokio::sync::oneshot::channel();
    let (resume, wait_resume) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 2048];
        assert!(stream.read(&mut request).await.unwrap() > 0);
        received.send(()).unwrap();
        wait_resume.await.unwrap();
        let body = r#"{"version":"stale"}"#;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let clone = client.clone();
    let request = tokio::spawn(async move { clone.version().await });
    wait_received.await.unwrap();
    client
        .switch_to_direct(MihomoEndpoint::default())
        .await
        .unwrap();
    resume.send(()).unwrap();
    assert!(matches!(
        request.await.unwrap(),
        Err(MihomoError::StaleTransport)
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn connection_consumers_share_a_request_and_close_invalidates_the_snapshot() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for body in [
            r#"{"connections":[{"id":"one","download":42}],"downloadTotal":42}"#,
            "",
            r#"{"connections":[],"downloadTotal":42}"#,
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let size = stream.read(&mut request).unwrap();
            requests.push(
                String::from_utf8_lossy(&request[..size])
                    .lines()
                    .next()
                    .unwrap()
                    .to_owned(),
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
        requests
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let other = client.clone();
    let (live, summary, history) = tokio::join!(
        client.connections_snapshot(),
        other.connections_summary(),
        client.traffic_accounting_snapshot()
    );
    assert_eq!(live.unwrap().connections[0].download, 42);
    assert_eq!(summary.unwrap().active_connections, 1);
    assert_eq!(history.unwrap().connections[0].download, 42);
    client.close_connection("one").await.unwrap();
    assert!(
        client
            .connections_snapshot()
            .await
            .unwrap()
            .connections
            .is_empty()
    );
    assert_eq!(
        server.join().unwrap(),
        [
            "GET /connections HTTP/1.1",
            "DELETE /connections/one HTTP/1.1",
            "GET /connections HTTP/1.1"
        ]
    );
}

#[test]
fn encodes_proxy_names_as_single_path_segments() {
    assert_eq!(
        encode_path_segment("HK/香港 #1"),
        "HK%2F%E9%A6%99%E6%B8%AF%20%231"
    );
}

#[tokio::test]
async fn version_preserves_mihomo_api_error_message() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2_048];
        let _ = stream.read(&mut request).unwrap();
        let body = r#"{"message":"authentication required"}"#;
        write!(
            stream,
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    let error = client.version().await.unwrap_err();
    server.join().unwrap();

    assert!(matches!(
        error,
        MihomoError::Api {
            status: 401,
            ref message
        } if message == "authentication required"
    ));
}

#[tokio::test]
async fn set_mode_rejects_unknown_mode_before_network_request() {
    let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();
    let error = client.set_mode("script").await.unwrap_err();
    assert!(matches!(error, MihomoError::InvalidInput(_)));
}

#[tokio::test]
async fn proxy_delay_rejects_non_http_test_url_before_network_request() {
    let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();
    let error = client
        .proxy_delay("node", Some("file:///tmp/probe"), 5_000)
        .await
        .unwrap_err();
    assert!(matches!(error, MihomoError::InvalidInput(_)));
}

#[tokio::test]
async fn provider_proxy_delay_uses_the_provider_healthcheck_endpoint() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2_048];
        let bytes = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..bytes]).into_owned();
        let body = r#"{"delay":42}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        request
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    let result = client
        .proxy_delay_with_provider(
            "香港/01",
            Some("https://example.com/generate_204"),
            5_000,
            Some("机场 A"),
        )
        .await
        .unwrap();
    let request = server.join().unwrap();

    assert_eq!(result.delay, 42);
    assert!(request.starts_with(
        "GET /providers/proxies/%E6%9C%BA%E5%9C%BA%20A/%E9%A6%99%E6%B8%AF%2F01/healthcheck?"
    ));
}

#[tokio::test]
async fn blank_provider_proxy_delay_uses_the_regular_proxy_endpoint() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2_048];
        let bytes = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..bytes]).into_owned();
        let body = r#"{"delay":21}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        request
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    let result = client
        .proxy_delay_with_provider(
            "DIRECT",
            Some("https://example.com/generate_204"),
            5_000,
            Some("  "),
        )
        .await
        .unwrap();
    let request = server.join().unwrap();

    assert_eq!(result.delay, 21);
    assert!(request.starts_with("GET /proxies/DIRECT/delay?"));
}

#[tokio::test]
async fn proxy_group_selection_reads_only_the_encoded_group_endpoint() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2_048];
        let bytes = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..bytes]).into_owned();
        let body = r#"{"name":"机场/A","now":"香港 01","all":["香港 01","美国 02"]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        request
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    let selected = client.proxy_group_selection("机场/A").await.unwrap();
    let request = server.join().unwrap();

    assert_eq!(
        (selected.as_str(), request.lines().next()),
        (
            "香港 01",
            Some("GET /proxies/%E6%9C%BA%E5%9C%BA%2FA HTTP/1.1")
        )
    );
}

#[tokio::test]
async fn dns_a_and_aaaa_queries_preserve_independent_answers() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for body in [
            r#"{"Status":0,"Question":[{"name":"example.com.","type":1}],"Answer":[{"name":"example.com.","type":1,"TTL":120,"data":"192.0.2.1"}]}"#,
            r#"{"Status":0,"Question":[{"name":"example.com.","type":28}],"Answer":[{"name":"example.com.","type":28,"TTL":60,"data":"2001:db8::1"}]}"#,
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2_048];
            let bytes = stream.read(&mut request).unwrap();
            requests.push(String::from_utf8_lossy(&request[..bytes]).into_owned());
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
        requests
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    let a = client
        .dns_query("example.com", DnsRecordType::A)
        .await
        .unwrap();
    let aaaa = client
        .dns_query("example.com", DnsRecordType::Aaaa)
        .await
        .unwrap();
    let requests = server.join().unwrap();

    assert_eq!(
        (a.status, a.answer[0].ttl, a.answer[0].data.as_str()),
        (0, 120, "192.0.2.1")
    );
    assert_eq!(
        (
            aaaa.status,
            aaaa.answer[0].ttl,
            aaaa.answer[0].data.as_str()
        ),
        (0, 60, "2001:db8::1")
    );
    assert!(requests[0].starts_with("GET /dns/query?name=example.com&type=A HTTP/1.1"));
    assert!(requests[1].starts_with("GET /dns/query?name=example.com&type=AAAA HTTP/1.1"));
}

#[tokio::test]
async fn dns_and_fake_ip_cache_flushes_use_separate_endpoints() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut first_lines = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2_048];
            let bytes = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..bytes]);
            first_lines.push(request.lines().next().unwrap_or_default().to_owned());
            write!(
                stream,
                "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        }
        first_lines
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    client.flush_dns_cache().await.unwrap();
    client.flush_fake_ip_cache().await.unwrap();

    assert_eq!(
        server.join().unwrap(),
        [
            "POST /cache/dns/flush HTTP/1.1",
            "POST /cache/fakeip/flush HTTP/1.1",
        ]
    );
}

#[tokio::test]
async fn reload_payload_rejects_oversized_config_before_network_request() {
    let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();
    let payload = "a".repeat(crate::profiles::MAX_PROFILE_BYTES + 1);

    let error = client.reload_payload(payload, false).await.unwrap_err();

    assert!(matches!(error, MihomoError::InvalidInput(_)));
}

#[tokio::test]
async fn reload_payload_adds_mihomo_geodata_fallbacks_before_the_request() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 8_192];
        let length = stream.read(&mut request).unwrap();
        write!(
            stream,
            "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        String::from_utf8_lossy(&request[..length]).into_owned()
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    client
        .reload_payload("rules: [MATCH,DIRECT]\n", true)
        .await
        .unwrap();

    let request = server.join().unwrap();
    assert!(request.contains("geox-url"));
    assert!(request.contains("testingcf.jsdelivr.net"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloned_clients_serialize_mutating_requests() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let (first_received_tx, first_received_rx) = tokio::sync::oneshot::channel();
    let (overlap_tx, overlap_rx) = tokio::sync::oneshot::channel();
    let server = thread::spawn(move || {
        let (mut first, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2_048];
        let _ = first.read(&mut request).unwrap();
        first_received_tx.send(()).unwrap();

        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_millis(250);
        let mut second = None;
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((stream, _)) => {
                    second = Some(stream);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("failed to probe second request: {error}"),
            }
        }
        overlap_tx.send(second.is_some()).unwrap();
        write!(
            first,
            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        drop(first);

        let mut second = second.unwrap_or_else(|| {
            listener.set_nonblocking(false).unwrap();
            listener.accept().unwrap().0
        });
        let _ = second.read(&mut request).unwrap();
        write!(
            second,
            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    let first_client = client.clone();
    let first = tokio::spawn(async move {
        first_client
            .patch_configs(&serde_json::json!({"mode": "rule"}))
            .await
    });
    first_received_rx.await.unwrap();
    let second = tokio::spawn(async move {
        client
            .patch_configs(&serde_json::json!({"mode": "global"}))
            .await
    });
    let overlapped = overlap_rx.await.unwrap();

    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    server.join().unwrap();
    assert!(
        !overlapped,
        "a second mutation started before the first completed"
    );
}

#[tokio::test]
async fn maintenance_operations_use_real_mihomo_post_endpoints() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2_048];
            let bytes = stream.read(&mut request).unwrap();
            requests.push(String::from_utf8_lossy(&request[..bytes]).into_owned());
            write!(
                stream,
                "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        }
        requests
    });
    let client = MihomoClient::new(MihomoEndpoint::new(
        format!("http://{address}"),
        "maintenance-secret",
    ))
    .unwrap();

    client.upgrade_core().await.unwrap();
    client.update_geodata().await.unwrap();
    client.update_external_ui().await.unwrap();
    let requests = server.join().unwrap();
    let first_lines = requests
        .iter()
        .map(|request| request.lines().next().unwrap_or_default())
        .collect::<Vec<_>>();

    assert_eq!(
        first_lines,
        [
            "POST /upgrade HTTP/1.1",
            "POST /configs/geo HTTP/1.1",
            "POST /upgrade/ui HTTP/1.1"
        ]
    );
    assert!(requests.iter().all(|request| {
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer maintenance-secret")
    }));
}

#[tokio::test]
async fn rule_disable_uses_indexed_patch_and_requires_matching_readback() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut patch_stream, _) = listener.accept().unwrap();
        let mut patch_request = [0_u8; 2_048];
        let bytes = patch_stream.read(&mut patch_request).unwrap();
        let patch_request = String::from_utf8_lossy(&patch_request[..bytes]).into_owned();
        write!(
            patch_stream,
            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n"
        )
        .unwrap();

        let (mut readback_stream, _) = listener.accept().unwrap();
        let mut readback_request = [0_u8; 2_048];
        let bytes = readback_stream.read(&mut readback_request).unwrap();
        let readback_request = String::from_utf8_lossy(&readback_request[..bytes]).into_owned();
        let body = r#"{"rules":[{"type":"Domain","payload":"example.com","proxy":"DIRECT","size":-1,"index":12,"extra":{"disabled":true,"hitCount":1,"hitAt":"now","missCount":0,"missAt":""}}]}"#;
        write!(
            readback_stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        (patch_request, readback_request)
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    let catalog = client.set_rule_disabled(12, true).await.unwrap();
    let (patch_request, readback_request) = server.join().unwrap();

    assert!(patch_request.starts_with("PATCH /rules/disable HTTP/1.1"));
    assert!(patch_request.contains(r#""12":true"#));
    assert!(readback_request.starts_with("GET /rules HTTP/1.1"));
    assert!(catalog.rules[0].extra.as_ref().unwrap().disabled);
}

#[tokio::test]
async fn acknowledged_rule_disable_returns_without_catalog_readback() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut patch_stream, _) = listener.accept().unwrap();
        let mut patch_request = [0_u8; 2_048];
        let bytes = patch_stream.read(&mut patch_request).unwrap();
        let patch_request = String::from_utf8_lossy(&patch_request[..bytes]).into_owned();
        write!(
            patch_stream,
            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        patch_request
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    client.apply_rule_disabled(7, false).await.unwrap();
    let patch_request = server.join().unwrap();

    assert!(patch_request.starts_with("PATCH /rules/disable HTTP/1.1"));
    assert!(patch_request.contains(r#""7":false"#));
}

#[tokio::test]
async fn local_tun_reload_is_rejected_before_controller_dispatch() {
    let fixture =
        crate::core_session::ownership_tests::ChildFixture::new("geodata-local-tun-reload-gate")
            .await;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = count.clone();
    let server = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut request = [0; 8192];
            assert!(stream.read(&mut request).await.unwrap() > 0);
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            stream
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        }
    });
    let launch = fixture
        .process
        .launch_config()
        .clone()
        .with_controller_endpoint(MihomoEndpoint::new(format!("http://{address}"), ""));
    let process = crate::MihomoProcess::prepare_stopped(launch);
    let client = MihomoClient::from_process(process).unwrap();
    for payload in [
        "tun: {enable: true}\n",
        "defaults: &base {enable: true}\ntun:\n  <<: *base\n",
        "defaults: &base {tun: {enable: true}}\n<<: *base\n",
        "a: &a {enable: true}\nb: &b {<<: *a}\ntun: {<<: *b}\n",
    ] {
        let result = client.reload_payload(payload, true).await;
        assert!(matches!(result, Err(MihomoError::ServiceRequired)));
    }
    server.abort();
    assert!(!MihomoError::ServiceRequired.mutation_result_unknown());
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn local_tun_patch_is_rejected_before_controller_dispatch() {
    let fixture =
        crate::core_session::ownership_tests::ChildFixture::new("geodata-local-tun-patch-gate")
            .await;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = count.clone();
    let server = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut request = [0; 8192];
            assert!(stream.read(&mut request).await.unwrap() > 0);
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            stream
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        }
    });
    let launch = fixture
        .process
        .launch_config()
        .clone()
        .with_controller_endpoint(MihomoEndpoint::new(format!("http://{address}"), ""));
    let process = crate::MihomoProcess::prepare_stopped(launch);
    let client = MihomoClient::from_process(process).unwrap();
    for patch in [
        serde_json::json!({"tun":{"enable":true}}),
        serde_json::json!({"Tun":{"Enable":true}}),
        serde_json::json!({"tun":{"ENABLE":true}}),
        serde_json::json!({"tun":{"enable":false},"TUN":{"enable":true}}),
        serde_json::json!({"tun":{"enable":false,"Enable":true}}),
    ] {
        let result = client.patch_configs(&patch).await;
        assert!(matches!(result, Err(MihomoError::ServiceRequired)));
    }
    server.abort();
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn external_tun_patch_keeps_its_controller_path() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 8192];
        let length = stream.read(&mut request).unwrap();
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
        String::from_utf8_lossy(&request[..length]).into_owned()
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    client
        .patch_configs(&serde_json::json!({"tun":{"enable":true}}))
        .await
        .unwrap();
    let request = server.join().unwrap();
    assert!(request.starts_with("PATCH /configs "));
    assert!(request.contains(r#""enable":true"#));
}

#[tokio::test]
async fn meow_owned_tun_patch_does_not_claim_mihomo_service_admission() {
    let fixture =
        crate::core_session::ownership_tests::ChildFixture::new("geodata-meow-tun-policy-kind")
            .await;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 8192];
        let length = stream.read(&mut request).unwrap();
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
        String::from_utf8_lossy(&request[..length]).into_owned()
    });
    let mut launch = fixture
        .process
        .launch_config()
        .clone()
        .with_controller_endpoint(MihomoEndpoint::new(format!("http://{address}"), ""));
    launch.kind = crate::CoreKind::Meow;
    let client = MihomoClient::from_process(crate::MihomoProcess::prepare_stopped(launch)).unwrap();
    client
        .patch_configs(&serde_json::json!({"tun":{"enable":true}}))
        .await
        .unwrap();
    assert!(server.join().unwrap().starts_with("PATCH /configs "));
}
