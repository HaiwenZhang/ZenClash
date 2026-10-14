use super::*;
use crate::{CoreConfigValidator, CoreKind, MihomoEndpoint, data_coordinator::DataWriteLease};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn builtin_request(listener: tokio::net::TcpListener, path: &str) -> tokio::net::TcpStream {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") {
        request.push(stream.read_u8().await.unwrap());
        assert!(request.len() <= 8192);
    }
    assert!(String::from_utf8_lossy(&request).starts_with(&format!("POST {path} ")));
    stream
}

#[tokio::test]
async fn builtin_receipt_releases_transition_and_runtime_write_scope() {
    let root = super::super::test_support::session_test_root("builtin-receipt");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = MihomoClient::new(MihomoEndpoint::new(
        format!("http://{}", listener.local_addr().unwrap()),
        "",
    ))
    .unwrap()
    .with_config_validator(CoreConfigValidator::new(
        CoreKind::Mihomo,
        root.join("unused-core"),
        root.join("home"),
    ))
    .unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
    let server = tokio::spawn(async move {
        let mut stream = builtin_request(listener, "/configs/geo").await;
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
    });
    let receipt = session.update_geodata_with_receipt().await.unwrap();
    assert!(receipt.client().verify_current_runtime().is_ok());
    assert!(session.transition.try_lock().is_ok());
    let scopes = receipt.client().write_scopes();
    let exclusive = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::task::spawn_blocking(move || DataWriteLease::exclusive(scopes)),
    )
    .await
    .unwrap()
    .unwrap();
    drop(exclusive);
    drop(receipt);
    server.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancelling_builtin_waiter_preserves_admitted_completion_and_shutdown_waits() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = MihomoClient::new(MihomoEndpoint::new(
        format!("http://{}", listener.local_addr().unwrap()),
        "",
    ))
    .unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
    let (entered, received) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut stream = builtin_request(listener, "/configs/geo").await;
        entered.send(()).unwrap();
        released.await.unwrap();
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
    });
    let updating = session.clone();
    let waiter = tokio::spawn(async move { updating.update_geodata_with_receipt().await });
    received.await.unwrap();
    waiter.abort();
    assert!(waiter.await.is_err_and(|error| error.is_cancelled()));
    assert!(session.transition.try_lock().is_err());
    let stopping = session.clone();
    let shutdown = tokio::spawn(async move { stopping.shutdown().await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !session.is_shutting_down() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!shutdown.is_finished());
    release.send(()).unwrap();
    server.await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn legacy_external_ui_update_keeps_its_unit_return_contract() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = MihomoClient::new(MihomoEndpoint::new(
        format!("http://{}", listener.local_addr().unwrap()),
        "",
    ))
    .unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
    let server = tokio::spawn(async move {
        let mut stream = builtin_request(listener, "/upgrade/ui").await;
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
    });
    let result: Result<(), CoreSessionError> = session.update_external_ui().await;
    assert!(result.is_ok());
    server.await.unwrap();
}
