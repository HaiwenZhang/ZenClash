//! Isolated proxy controller/session construction and bounded HTTP protocol fixtures.

use super::CoreSession;
use crate::{CoreKind, MihomoClient, MihomoEndpoint};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

pub(super) async fn proxy_action_session() -> (CoreSession, tokio::net::TcpListener) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = MihomoClient::new(MihomoEndpoint::new(
        format!("http://{}", listener.local_addr().unwrap()),
        "",
    ))
    .unwrap();
    (
        CoreSession::open(CoreKind::Mihomo, client).unwrap(),
        listener,
    )
}

pub(super) async fn request(
    listener: &tokio::net::TcpListener,
    prefix: &str,
) -> tokio::net::TcpStream {
    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let (headers, payload) = crate::test_support::read_controller_request(&mut stream)
        .await
        .unwrap()
        .into_parts();
    assert!(headers.len() <= 8192);
    assert!(
        headers.starts_with(prefix),
        "unexpected selection request: {headers}"
    );
    if prefix.starts_with("PUT") {
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&payload).unwrap(),
            serde_json::json!({"name":"Requested"})
        );
    }
    stream
}

pub(super) async fn acknowledge(stream: &mut tokio::net::TcpStream) {
    stream
        .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
}

pub(super) async fn actual_member(stream: &mut tokio::net::TcpStream) {
    let body = r#"{"now":"Actual","type":"Selector"}"#;
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
}
