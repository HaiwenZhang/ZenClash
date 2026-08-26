use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
    time::{Duration, Instant},
};

use super::{api::encode_path_segment, *};

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
async fn reload_payload_rejects_oversized_config_before_network_request() {
    let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();
    let payload = "a".repeat(crate::profiles::MAX_PROFILE_BYTES + 1);

    let error = client.reload_payload(payload, false).await.unwrap_err();

    assert!(matches!(error, MihomoError::InvalidInput(_)));
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
    assert!(requests.iter().all(|request| request
        .to_ascii_lowercase()
        .contains("authorization: bearer maintenance-secret")));
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
