use super::*;
use crate::{MihomoEndpoint, MihomoError};
use std::{future::Future, task::Context};
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn queued_measurements_reject_a_changed_configuration_before_sending() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = MihomoClient::new(MihomoEndpoint::new(
        format!("http://{}", listener.local_addr().unwrap()),
        "",
    ))
    .unwrap();
    let session = crate::CoreSession::open(crate::CoreKind::Mihomo, client.clone()).unwrap();
    let observation = session.begin_observation().await.unwrap();
    let operations = ProxyOperations::from_observation(&observation)
        .pin_current_runtime()
        .unwrap();
    let blocked = client
        .delay_gate
        .clone()
        .acquire_many_owned(16)
        .await
        .unwrap();
    let target = ProxyDelayTarget {
        name: "node".into(),
        provider: None,
    };
    let mut queued = Box::pin(operations.measure(&target, None, 500));
    assert!(
        queued
            .as_mut()
            .poll(&mut Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
    session.invalidate_runtime_observations();
    drop(blocked);
    assert!(matches!(queued.await, Err(MihomoError::StaleBinding)));
    assert!(operations.clone().pin_current_runtime().is_err());
    assert!(
        operations
            .publish_if_current(|| panic!("stale publication ran"))
            .is_err()
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn an_inflight_measurement_rejects_its_response_after_configuration_replacement() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = MihomoClient::new(MihomoEndpoint::new(
        format!("http://{}", listener.local_addr().unwrap()),
        "",
    ))
    .unwrap();
    let session = crate::CoreSession::open(crate::CoreKind::Mihomo, client).unwrap();
    let operations = ProxyOperations::from_observation(&session.begin_observation().await.unwrap());
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let headers = crate::test_support::read_controller_request(&mut stream)
            .await
            .unwrap()
            .into_bytes();
        assert!(String::from_utf8_lossy(&headers).starts_with("GET /proxies/node/delay?"));
        entered.send(()).unwrap();
        released.await.unwrap();
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\n{\"delay\":42}",
            )
            .await
            .unwrap();
    });
    let measurement = tokio::spawn(async move {
        operations
            .measure(
                &ProxyDelayTarget {
                    name: "node".into(),
                    provider: None,
                },
                None,
                500,
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), started)
        .await
        .unwrap()
        .unwrap();
    session.invalidate_runtime_observations();
    release.send(()).unwrap();
    assert!(matches!(
        measurement.await.unwrap(),
        Err(MihomoError::StaleBinding)
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn a_measurement_batch_pin_rejects_later_clones_after_controller_replacement() {
    let original = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let replacement = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = MihomoClient::new(MihomoEndpoint::new(
        format!("http://{}", original.local_addr().unwrap()),
        "",
    ))
    .unwrap();
    let blocked = client
        .delay_gate
        .clone()
        .acquire_many_owned(16)
        .await
        .unwrap();
    let operations = ProxyOperations::new(client.clone())
        .pin_current_runtime()
        .unwrap();
    let first = ProxyDelayTarget {
        name: "first".into(),
        provider: None,
    };
    let mut queued = Box::pin(operations.measure(&first, None, 500));
    assert!(
        queued
            .as_mut()
            .poll(&mut Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
    let url = format!("http://{}", replacement.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let Ok(Ok((mut stream, _))) =
            tokio::time::timeout(std::time::Duration::from_millis(200), replacement.accept()).await
        else {
            return false;
        };
        let _headers = crate::test_support::read_controller_request(&mut stream)
            .await
            .unwrap()
            .into_bytes();
        let body = "{\"delay\":42}";
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
        true
    });
    client
        .switch_to_direct(MihomoEndpoint::new(url, ""))
        .await
        .unwrap();
    drop(blocked);
    assert!(matches!(queued.await, Err(MihomoError::StaleBinding)));
    let second = ProxyDelayTarget {
        name: "second".into(),
        provider: None,
    };
    let result = operations.clone().measure(&second, None, 500).await;
    let contacted_replacement = server.await.unwrap();
    assert!(matches!(result, Err(MihomoError::StaleBinding)));
    assert!(!contacted_replacement);
    let mut published = false;
    assert!(operations.publish_if_current(|| published = true).is_err());
    assert!(!published);
}
