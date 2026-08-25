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
