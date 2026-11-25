use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn request() -> Request<Full<Bytes>> {
    Request::builder()
        .method("PUT")
        .uri("/configs?force=false")
        .header("host", "localhost")
        .header("authorization", "Bearer private-secret")
        .body(Full::new(Bytes::from_static(b"private-body")))
        .unwrap()
}

async fn receive_request<T: AsyncRead + Unpin + Send>(stream: &mut T) -> Vec<u8> {
    crate::test_support::read_controller_request(stream)
        .await
        .unwrap()
        .into_bytes()
}

#[tokio::test]
async fn chunked_responses_preserve_non_success_status_and_body() {
    let (client, mut server) = tokio::io::duplex(4096);
    let fixture = tokio::spawn(async move {
        let bytes = receive_request(&mut server).await;
        assert!(bytes.windows(14).any(|part| part == b"private-secret"));
        server.write_all(b"HTTP/1.1 409 Conflict\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\ntest\r\n0\r\n\r\n").await.unwrap();
    });
    let response = exchange(client, request(), &mut false).await.unwrap();
    assert_eq!(response.status, 409);
    assert_eq!(response.body, b"test");
    fixture.await.unwrap();
}

#[tokio::test]
async fn chunked_body_cannot_bypass_the_response_byte_limit() {
    let (client, mut server) = tokio::io::duplex(4096);
    let fixture = tokio::spawn(async move {
        receive_request(&mut server).await;
        server
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await
            .unwrap();
        let chunk = vec![b'x'; 32 * 1024];
        for _ in 0..=MAX_RESPONSE_BYTES / chunk.len() {
            if server.write_all(b"8000\r\n").await.is_err()
                || server.write_all(&chunk).await.is_err()
                || server.write_all(b"\r\n").await.is_err()
            {
                return;
            }
        }
        let _ = server.write_all(b"0\r\n\r\n").await;
    });
    assert!(matches!(
        exchange(client, request(), &mut false).await,
        Err(NativeHttpError::BudgetExceeded { request_sent: true })
    ));
    fixture.await.unwrap();
}

#[tokio::test]
async fn response_headers_cannot_exhaust_the_body_budget_first() {
    let (client, mut server) = tokio::io::duplex(4096);
    let fixture = tokio::spawn(async move {
        receive_request(&mut server).await;
        let response = format!(
            "HTTP/1.1 200 OK\r\nX-Huge: {}\r\nContent-Length: 0\r\n\r\n",
            "x".repeat(MAX_HEADER_BYTES)
        );
        let _ = server.write_all(response.as_bytes()).await;
        let mut tail = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), server.read_to_end(&mut tail))
            .await
            .unwrap()
            .unwrap();
        assert!(tail.is_empty());
    });
    assert!(matches!(
        exchange(client, request(), &mut false).await,
        Err(NativeHttpError::BudgetExceeded { request_sent: true })
            | Err(NativeHttpError::OutcomeUnknown(_))
    ));
    fixture.await.unwrap();
}

#[tokio::test]
async fn oversized_response_is_unknown_after_a_mutating_request_was_sent() {
    let (client, mut server) = tokio::io::duplex(4096);
    let fixture = tokio::spawn(async move {
        receive_request(&mut server).await;
        server
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                    MAX_RESPONSE_BYTES + 1
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let result = exchange(client, request(), &mut false).await;
    assert!(matches!(
        result,
        Err(NativeHttpError::BudgetExceeded { request_sent: true })
    ));
    fixture.await.unwrap();
}

#[tokio::test]
async fn malformed_responses_do_not_report_definitive_mutation_failure() {
    let (client, mut server) = tokio::io::duplex(4096);
    let fixture = tokio::spawn(async move {
        receive_request(&mut server).await;
        server
            .write_all(b"not an HTTP response\r\n\r\n")
            .await
            .unwrap();
    });
    assert!(matches!(
        exchange(client, request(), &mut false).await,
        Err(NativeHttpError::OutcomeUnknown(_))
    ));
    fixture.await.unwrap();
}

#[tokio::test]
async fn cancelling_an_exchange_closes_its_driver_and_native_stream() {
    let (client, mut server) = tokio::io::duplex(4096);
    let exchange_task = tokio::spawn(async move { exchange(client, request(), &mut false).await });
    receive_request(&mut server).await;
    exchange_task.abort();
    let _ = exchange_task.await;
    let mut tail = Vec::new();
    tokio::time::timeout(Duration::from_secs(1), server.read_to_end(&mut tail))
        .await
        .unwrap()
        .unwrap();
    assert!(tail.is_empty());
}

#[cfg(windows)]
#[tokio::test]
async fn a_busy_pipe_wait_is_cancelled_by_the_request_deadline() {
    use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
    let name = format!(r"\\.\pipe\ZenClash.Kernel.Busy.{}", std::process::id());
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .max_instances(1)
        .create(&name)
        .unwrap();
    let _occupied = ClientOptions::new().open(&name).unwrap();
    server.connect().await.unwrap();
    let controller =
        NativeController::new(name.into(), std::process::id(), "private-secret".into());
    let result = controller
        .request("GET", "/version", None, Duration::from_millis(40))
        .await;
    assert!(matches!(result, Err(NativeHttpError::BeforeSend(ref error))
        if error.kind() == io::ErrorKind::TimedOut));
}

#[cfg(windows)]
#[tokio::test]
async fn an_actual_pipe_with_the_wrong_pid_receives_neither_body_nor_secret() {
    use tokio::net::windows::named_pipe::ServerOptions;
    let name = format!(r"\\.\pipe\ZenClash.Kernel.Impostor.{}", std::process::id());
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&name)
        .unwrap();
    let fixture = tokio::spawn(async move {
        server.connect().await.unwrap();
        let mut bytes = Vec::new();
        let result = server.read_to_end(&mut bytes).await;
        if let Err(error) = result {
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        }
        bytes
    });
    let controller = NativeController::new(
        name.into(),
        std::process::id().wrapping_add(1),
        "private-secret".into(),
    );
    let result = controller
        .request(
            "PUT",
            "/configs?force=false",
            Some(&serde_json::json!({"payload":"private-body"})),
            Duration::from_secs(1),
        )
        .await;
    assert!(
        matches!(result, Err(NativeHttpError::BeforeSend(ref error)) if error.kind() == io::ErrorKind::PermissionDenied)
    );
    assert!(fixture.await.unwrap().is_empty());
}

#[cfg(windows)]
#[tokio::test]
async fn a_verified_native_pipe_supports_json_escaping_and_deadline_cleanup() {
    use tokio::net::windows::named_pipe::ServerOptions;
    let name = format!(
        r"\\.\pipe\ZenClash.Kernel.Authenticated.{}",
        std::process::id()
    );
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&name)
        .unwrap();
    let fixture = tokio::spawn(async move {
        server.connect().await.unwrap();
        let request = crate::test_support::read_controller_request_with_limit(
            &mut server,
            MAX_REQUEST_BYTES + MAX_HEADER_BYTES,
        )
        .await
        .unwrap();
        assert!(request.headers().contains("Bearer private-secret"));
        assert!(request.body().len() > 4 * 1024 * 1024);
        let body: Value = serde_json::from_slice(request.body()).unwrap();
        assert_eq!(
            body["payload"].as_str().unwrap(),
            "\"".repeat(3 * 1024 * 1024)
        );
        let mut tail = Vec::new();
        let result = server.read_to_end(&mut tail).await;
        if let Err(error) = result {
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        }
        assert!(tail.is_empty());
    });
    let controller =
        NativeController::new(name.into(), std::process::id(), "private-secret".into());
    let body = serde_json::json!({"payload":"\"".repeat(3 * 1024 * 1024)});
    let result = controller
        .request(
            "PUT",
            "/configs?force=false",
            Some(&body),
            Duration::from_secs(2),
        )
        .await;
    assert!(
        matches!(result, Err(NativeHttpError::OutcomeUnknown(ref error)) if error.kind() == io::ErrorKind::TimedOut)
    );
    tokio::time::timeout(Duration::from_secs(1), fixture)
        .await
        .unwrap()
        .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn a_local_socket_accepts_the_application_users_owned_pid() {
    let directory =
        std::env::temp_dir().join(format!("zenclash-controller-local-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("controller.sock");
    let server = tokio::net::UnixListener::bind(&path).unwrap();
    let fixture = tokio::spawn(async move {
        let (mut stream, _) = server.accept().await.unwrap();
        receive_request(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .await
            .unwrap();
    });
    let controller =
        NativeController::new_local(path.clone(), std::process::id(), "private-secret".into());
    let response = controller
        .request("GET", "/version", None, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    fixture.await.unwrap();
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[cfg(all(unix, not(feature = "service-ipc-tests")))]
#[tokio::test]
async fn a_service_socket_rejects_an_ordinary_user_before_sending_credentials() {
    // A root test runner cannot model an ordinary-user impostor.
    // SAFETY: `geteuid` has no preconditions or memory effects.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let directory =
        std::env::temp_dir().join(format!("zenclash-controller-root-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("controller.sock");
    let server = tokio::net::UnixListener::bind(&path).unwrap();
    let fixture = tokio::spawn(async move {
        let (mut stream, _) = server.accept().await.unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await.unwrap();
        bytes
    });
    let controller =
        NativeController::new(path.clone(), std::process::id(), "private-secret".into());
    let result = controller
        .request("GET", "/version", None, Duration::from_secs(1))
        .await;
    assert!(matches!(result, Err(NativeHttpError::BeforeSend(ref error))
        if error.kind() == io::ErrorKind::PermissionDenied));
    assert!(fixture.await.unwrap().is_empty());
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn an_actual_socket_with_the_wrong_pid_receives_neither_body_nor_secret() {
    let directory =
        std::env::temp_dir().join(format!("zenclash-controller-peer-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("controller.sock");
    let server = tokio::net::UnixListener::bind(&path).unwrap();
    let fixture = tokio::spawn(async move {
        let (mut stream, _) = server.accept().await.unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await.unwrap();
        bytes
    });
    let controller = NativeController::new_local(
        path.clone(),
        std::process::id().wrapping_add(1),
        "private-secret".into(),
    );
    let result = controller
        .request(
            "PUT",
            "/configs?force=false",
            Some(&serde_json::json!({"payload":"private-body"})),
            Duration::from_secs(1),
        )
        .await;
    assert!(
        matches!(result, Err(NativeHttpError::BeforeSend(ref error)) if error.kind() == io::ErrorKind::PermissionDenied)
    );
    assert!(fixture.await.unwrap().is_empty());
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}
