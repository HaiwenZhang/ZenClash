use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Wake, Waker},
    time::Duration,
};

use parking_lot::Mutex;
use tokio::io::AsyncWriteExt;

use super::{
    MAX_DELAY_TRANSPORT_TIMEOUT, PROVIDER_HEALTHCHECK_TIMEOUT, PROVIDER_UPDATE_TIMEOUT,
    json_contains, proxy_delay_transport_timeout, proxy_group_delay_transport_timeout,
};
use crate::{
    MihomoClient, MihomoEndpoint, MihomoError, MihomoResult,
    runtime::client::request::DEFAULT_IPC_REQUEST_TIMEOUT,
};

type SwitchFuture = Pin<Box<dyn Future<Output = MihomoResult<()>> + Send>>;

#[tokio::test]
async fn memory_snapshot_reads_one_frame_without_waiting_for_stream_end() {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (release, waiter) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
        socket
            .send(Message::Text(r#"{"inuse":47000000,"oslimit":0}"#.into()))
            .await
            .unwrap();
        let _ = waiter.await;
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let memory = tokio::time::timeout(Duration::from_secs(1), client.memory_snapshot())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(memory.inuse, 47_000_000);
    let _ = release.send(());
    server.await.unwrap();
}

#[tokio::test]
async fn memory_stream_decode_error_identifies_its_endpoint_and_field() {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
        socket
            .send(Message::Text(r#"{"inuse":"bad-memory"}"#.into()))
            .await
            .unwrap();
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let error = client.memory_snapshot().await.unwrap_err();
    server.await.unwrap();
    assert!(!error.mutation_result_unknown());
    assert!(
        matches!(error, MihomoError::ResponseDecode { endpoint, field_path, .. }
        if endpoint == "/memory" && field_path == "inuse")
    );
}

struct SwitchOnUnlock(Mutex<Option<SwitchFuture>>);

impl Wake for SwitchOnUnlock {
    fn wake(self: Arc<Self>) {
        let mut future = self.0.lock();
        if let Some(switch) = future.as_mut()
            && switch
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_ready()
        {
            *future = None;
        }
    }
}

#[tokio::test]
async fn verified_patch_binding_switch_after_send_keeps_unknown_outcome() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (received, received_waiter) = tokio::sync::oneshot::channel();
    let (release, release_waiter) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let (headers, body) = crate::test_support::read_controller_request(&mut stream)
            .await
            .unwrap()
            .into_parts();
        assert!(headers.as_bytes().starts_with(b"PATCH /configs "));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({"mode":"global"})
        );
        received.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), release_waiter)
            .await
            .unwrap()
            .unwrap();
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(300), listener.accept())
                .await
                .is_err(),
            "verification was sent to the retired controller"
        );
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let pinned = client.pin_binding().unwrap();
    let body = serde_json::json!({"mode":"global"});
    let mut patch = Box::pin(pinned.patch_configs_verified(&body));
    tokio::select! {
        result = &mut patch => panic!("patch finished before response: {result:?}"),
        result = received_waiter => result.unwrap(),
    }
    let replacement = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let switch_client = client.clone();
    let endpoint = MihomoEndpoint::new(format!("http://{}", replacement.local_addr().unwrap()), "");
    let switch = Arc::new(SwitchOnUnlock(Mutex::new(Some(Box::pin(async move {
        switch_client.switch_to_direct(endpoint).await
    })))));
    let waker = Waker::from(switch.clone());
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
    release.send(()).unwrap();
    let error = tokio::time::timeout(Duration::from_secs(5), patch)
        .await
        .unwrap()
        .unwrap_err();
    let retirement = switch.0.lock().take();
    if let Some(retirement) = retirement {
        tokio::time::timeout(Duration::from_secs(5), retirement)
            .await
            .unwrap()
            .unwrap();
    }
    assert!(
        matches!(error, MihomoError::StaleTransport),
        "PATCH was already sent: {error}"
    );
    assert!(error.mutation_result_unknown());
    assert!(
        replacement
            .poll_accept(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    server.await.unwrap();
}

#[test]
fn matches_nested_runtime_patch_as_a_subset() {
    let actual = serde_json::json!({
        "ipv6": true,
        "tun": {"enable": false, "stack": "mixed"}
    });

    assert!(json_contains(
        &actual,
        &serde_json::json!({"tun": {"enable": false}})
    ));
    assert!(!json_contains(
        &actual,
        &serde_json::json!({"tun": {"enable": true}})
    ));
}

#[test]
fn delay_and_provider_transport_timeouts_exceed_ipc_default_and_scale_safely() {
    assert!(proxy_delay_transport_timeout(500) > DEFAULT_IPC_REQUEST_TIMEOUT);
    assert_eq!(
        proxy_delay_transport_timeout(5_000),
        Duration::from_secs(15)
    );
    assert_eq!(
        proxy_delay_transport_timeout(12_000),
        Duration::from_secs(22)
    );
    assert_eq!(
        proxy_delay_transport_timeout(30_000),
        Duration::from_secs(40)
    );
    assert_eq!(
        proxy_delay_transport_timeout(u64::MAX),
        MAX_DELAY_TRANSPORT_TIMEOUT
    );

    assert!(proxy_group_delay_transport_timeout(500) > DEFAULT_IPC_REQUEST_TIMEOUT);
    assert_eq!(
        proxy_group_delay_transport_timeout(5_000),
        Duration::from_secs(35)
    );
    assert_eq!(
        proxy_group_delay_transport_timeout(15_000),
        Duration::from_secs(45)
    );
    assert_eq!(
        proxy_group_delay_transport_timeout(u64::MAX),
        MAX_DELAY_TRANSPORT_TIMEOUT
    );

    assert!(PROVIDER_UPDATE_TIMEOUT > DEFAULT_IPC_REQUEST_TIMEOUT);
    assert!(PROVIDER_HEALTHCHECK_TIMEOUT > DEFAULT_IPC_REQUEST_TIMEOUT);
}
