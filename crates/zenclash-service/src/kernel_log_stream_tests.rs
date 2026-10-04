use super::*;
use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};
use tokio_tungstenite::{accept_hdr_async, tungstenite::Message};

struct ObserveRequest(tokio::sync::oneshot::Sender<String>);

impl Callback for ObserveRequest {
    fn on_request(self, request: &Request, response: Response) -> Result<Response, ErrorResponse> {
        assert_eq!(request.headers()["Authorization"], "Bearer fixture-secret");
        self.0.send(request.uri().to_string()).unwrap();
        Ok(response)
    }
}

struct ExpectRequest(String);

impl Callback for ExpectRequest {
    fn on_request(self, request: &Request, response: Response) -> Result<Response, ErrorResponse> {
        assert_eq!(request.uri().to_string(), self.0);
        Ok(response)
    }
}

struct RejectInvalidLevel;

impl Callback for RejectInvalidLevel {
    fn on_request(self, request: &Request, _response: Response) -> Result<Response, ErrorResponse> {
        assert_eq!(request.uri(), "/logs?level=info&format=structured");
        Err(tokio_tungstenite::tungstenite::http::Response::builder()
            .status(400)
            .body(Some("invalid log level".to_owned()))
            .unwrap())
    }
}

#[tokio::test]
async fn log_stream_default_requests_info_and_structured_in_actual_upgrade() {
    let (client, server) = tokio::io::duplex(4096);
    let (observed, received) = tokio::sync::oneshot::channel();
    let fixture = tokio::spawn(async move {
        let _socket = accept_hdr_async(server, ObserveRequest(observed))
            .await
            .unwrap();
    });
    let events = subscribe_verified(
        Box::new(client),
        KernelStream::Logs(LogStreamOptions::default()),
        "fixture-secret",
        Path::new("fixture"),
        12,
    )
    .await
    .unwrap();
    let request = received.await.unwrap();
    fixture.await.unwrap();
    drop(events);
    assert_eq!(request, "/logs?level=info&format=structured");
}

#[tokio::test]
async fn log_stream_rejects_frame_above_the_core_log_budget() {
    let (client, server) = tokio::io::duplex(4096);
    let fixture = tokio::spawn(async move {
        let mut socket = tokio_tungstenite::accept_async(server).await.unwrap();
        let body = serde_json::json!({"type":"info", "payload":"x".repeat(129 * 1024)}).to_string();
        // A rejected frame can close the other end while this writer is still sending.
        let _ = socket.send(Message::Text(body)).await;
    });
    let mut events = subscribe_verified(
        Box::new(client),
        KernelStream::Logs(LogStreamOptions::default()),
        "fixture-secret",
        Path::new("fixture"),
        12,
    )
    .await
    .unwrap();
    let result = events.next().await;
    drop(events);
    fixture.await.unwrap();
    assert!(result.is_err(), "oversized log frame was accepted");
}

#[tokio::test]
async fn log_stream_typed_options_select_exact_upgrade_and_event_representation() {
    for (level, wire) in [
        (ServiceLogLevel::Silent, "silent"),
        (ServiceLogLevel::Error, "error"),
        (ServiceLogLevel::Warning, "warning"),
        (ServiceLogLevel::Info, "info"),
        (ServiceLogLevel::Debug, "debug"),
    ] {
        for (format, wire_format) in [
            (ServiceLogFormat::Plain, "plain"),
            (ServiceLogFormat::Structured, "structured"),
        ] {
            let (client, server) = tokio::io::duplex(4096);
            let expected = format!("/logs?level={wire}&format={wire_format}");
            let event = if format == ServiceLogFormat::Structured {
                serde_json::json!({"time":"12:00:00", "level":wire, "message":"event"})
            } else {
                serde_json::json!({"type":wire, "payload":"event"})
            };
            let body = event.to_string();
            let fixture = tokio::spawn(async move {
                let mut socket = accept_hdr_async(server, ExpectRequest(expected))
                    .await
                    .unwrap();
                socket.send(Message::Text(body)).await.unwrap();
            });
            let mut events = subscribe_verified(
                Box::new(client),
                KernelStream::Logs(LogStreamOptions { level, format }),
                "fixture-secret",
                Path::new("fixture"),
                12,
            )
            .await
            .unwrap();
            assert_eq!(events.next().await.unwrap(), Some(event));
            fixture.await.unwrap();
        }
    }
}

#[tokio::test]
async fn log_stream_http_400_does_not_retry_plain_or_another_upgrade() {
    use tokio::io::AsyncReadExt;
    let (client, server) = tokio::io::duplex(4096);
    let fixture = tokio::spawn(async move {
        let mut server = server;
        let rejected = accept_hdr_async(&mut server, RejectInvalidLevel).await;
        assert!(rejected.is_err());
        let mut bytes = [0; 1];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), server.read(&mut bytes))
                .await
                .unwrap()
                .unwrap(),
            0,
            "a second upgrade was attempted"
        );
    });
    let result = subscribe_verified(
        Box::new(client),
        KernelStream::Logs(LogStreamOptions::default()),
        "fixture-secret",
        Path::new("fixture"),
        12,
    )
    .await;
    assert!(result.is_err());
    fixture.await.unwrap();
}
