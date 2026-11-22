use std::{
    future::Future,
    io::{Read, Write},
    net::TcpListener,
    pin::Pin,
    task::{Context, Poll, Wake, Waker},
    thread,
    time::Duration,
};

use super::*;
use crate::MihomoEndpoint;

type SwitchFuture = Pin<Box<dyn Future<Output = crate::MihomoResult<()>> + Send>>;

struct SwitchOnUnlock(parking_lot::Mutex<Option<SwitchFuture>>);

impl Wake for SwitchOnUnlock {
    fn wake(self: Arc<Self>) {
        let mut future = self.0.lock();
        if let Some(switch) = future.as_mut()
            && let Poll::Ready(result) = switch
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
        {
            result.unwrap();
            *future = None;
        }
    }
}

fn queue_runtime_switch(client: MihomoClient, endpoint: MihomoEndpoint) -> Arc<SwitchOnUnlock> {
    let switch = Arc::new(SwitchOnUnlock(parking_lot::Mutex::new(Some(Box::pin(
        async move { client.switch_to_direct(endpoint).await },
    )))));
    let waker = Waker::from(switch.clone());
    // Publish synchronously when the PUT releases admission, before its catalog readback.
    assert!(
        switch
            .0
            .lock()
            .as_mut()
            .unwrap()
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    switch
}

async fn finish_runtime_switch(switch: Arc<SwitchOnUnlock>) {
    let retirement = switch.0.lock().take();
    if let Some(retirement) = retirement {
        tokio::time::timeout(Duration::from_secs(2), retirement)
            .await
            .unwrap()
            .unwrap();
    }
}

fn proxy_catalog(count: usize) -> ProviderCatalog {
    ProviderCatalog {
        providers: BTreeMap::from([(
            "airport".into(),
            Provider {
                name: "airport".into(),
                proxies: (0..count).map(serde_json::Value::from).collect(),
                ..Provider::default()
            },
        )]),
    }
}

#[test]
fn duplicate_provider_labels_keep_independent_controller_keys() {
    let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();
    let operations = ProviderOperations::new(client);
    let provider = Provider {
        name: "Visible label".into(),
        ..Provider::default()
    };
    let catalog = ProviderCatalog {
        providers: BTreeMap::from([
            ("first-key".into(), provider.clone()),
            ("second-key".into(), provider),
        ]),
    };
    operations.observe_catalog(ProviderKind::Proxy, &catalog);
    assert_eq!(
        operations
            .status(ProviderKind::Proxy, "first-key")
            .unwrap()
            .name,
        "first-key"
    );
    assert_eq!(
        operations
            .status(ProviderKind::Proxy, "second-key")
            .unwrap()
            .name,
        "second-key"
    );
    assert!(
        operations
            .status(ProviderKind::Proxy, "Visible label")
            .is_none()
    );
}

#[tokio::test]
async fn provider_update_readback_and_history_remain_bound_to_the_original_runtime() {
    use tokio::io::AsyncWriteExt;
    let original = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = MihomoClient::new(MihomoEndpoint::new(
        format!("http://{}", original.local_addr().unwrap()),
        "",
    ))
    .unwrap();
    let replacement = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let replacement_url = format!("http://{}", replacement.local_addr().unwrap());
    let operations = ProviderOperations::new(client.clone());
    operations.observe_catalog(ProviderKind::Proxy, &proxy_catalog(3));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = original.accept().await.unwrap();
        let request = crate::test_support::read_controller_request(&mut stream)
            .await
            .unwrap()
            .into_bytes();
        assert!(String::from_utf8_lossy(&request).starts_with("PUT /providers/proxies/airport "));
        entered_tx.send(()).unwrap();
        release_rx.await.unwrap();
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        drop(stream);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), original.accept())
                .await
                .is_err(),
            "provider readback or remaining targets reached the retired controller"
        );
    });
    let updating_operations = operations.clone();
    let updating = tokio::spawn(async move {
        updating_operations
            .update(ProviderKind::Proxy, "airport")
            .await
    });
    entered_rx.await.unwrap();
    let switching = queue_runtime_switch(client, MihomoEndpoint::new(replacement_url, ""));
    release_tx.send(()).unwrap();
    server.await.unwrap();
    finish_runtime_switch(switching).await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), updating)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(operations.status(ProviderKind::Proxy, "airport").is_none());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), replacement.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn batch_stops_remaining_targets_and_readback_on_runtime_replacement() {
    use tokio::io::AsyncWriteExt;
    let original = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = MihomoClient::new(MihomoEndpoint::new(
        format!("http://{}", original.local_addr().unwrap()),
        "",
    ))
    .unwrap();
    let replacement = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let replacement_url = format!("http://{}", replacement.local_addr().unwrap());
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = original.accept().await.unwrap();
        let headers = crate::test_support::read_controller_request(&mut stream)
            .await
            .unwrap()
            .into_bytes();
        assert!(String::from_utf8_lossy(&headers).starts_with("PUT /providers/proxies/first "));
        entered_tx.send(()).unwrap();
        release_rx.await.unwrap();
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        drop(stream);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), original.accept())
                .await
                .is_err(),
            "provider readback or remaining targets reached the retired controller"
        );
    });
    let operations = ProviderOperations::new(client.clone());
    let updating = tokio::spawn(async move {
        operations
            .update_many(vec![
                ProviderUpdateTarget::new(ProviderKind::Proxy, "first"),
                ProviderUpdateTarget::new(ProviderKind::Proxy, "second"),
            ])
            .await
    });
    entered_rx.await.unwrap();
    let switching = queue_runtime_switch(client, MihomoEndpoint::new(replacement_url, ""));
    release_tx.send(()).unwrap();
    server.await.unwrap();
    finish_runtime_switch(switching).await;
    let receipt = tokio::time::timeout(std::time::Duration::from_secs(2), updating)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!receipt.failures().is_empty());
    assert!(receipt.client().verify_current_runtime().is_err());
    assert!(receipt.client().runtime_config().await.is_err());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), replacement.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn failed_update_keeps_last_known_good_provider_and_count() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2_048];
        let _ = stream.read(&mut request).unwrap();
        let body = r#"{"message":"download failed"}"#;
        write!(
            stream,
            "HTTP/1.1 502 Bad Gateway\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let operations = ProviderOperations::new(client);
    operations.observe_catalog(ProviderKind::Proxy, &proxy_catalog(3));

    let result = operations.update(ProviderKind::Proxy, "airport").await;
    server.join().unwrap();

    assert!(result.is_err());
    let status = operations.status(ProviderKind::Proxy, "airport").unwrap();
    assert_eq!(status.item_count, 3);
    assert_eq!(status.last_known_good.unwrap().proxies.len(), 3);
    assert!(status.last_success_at_ms.is_some());
    assert!(status.last_failure.is_some());
    assert!(status.update.last_failure.is_some());
    assert!(status.healthcheck.last_failure.is_none());
}

#[tokio::test]
async fn healthcheck_has_independent_success_and_readback() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut first_lines = Vec::new();
        for body in [
            None,
            Some(r#"{"providers":{"airport":{"name":"airport","proxies":[{},{}]}}}"#),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let request =
                crate::test_support::read_controller_request_blocking(&mut stream).unwrap();
            let request = request.headers();
            first_lines.push(request.lines().next().unwrap_or_default().to_owned());
            if let Some(body) = body {
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            } else {
                write!(
                    stream,
                    "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
            }
        }
        first_lines
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let operations = ProviderOperations::new(client);

    operations.healthcheck_proxy("airport").await.unwrap();
    let requests = server.join().unwrap();

    assert_eq!(
        requests,
        [
            "GET /providers/proxies/airport/healthcheck HTTP/1.1",
            "GET /providers/proxies HTTP/1.1",
        ]
    );
    let status = operations.status(ProviderKind::Proxy, "airport").unwrap();
    assert_eq!(status.item_count, 2);
    assert!(status.healthcheck.last_success_at_ms.is_some());
    assert!(status.update.last_success_at_ms.is_none());
}
