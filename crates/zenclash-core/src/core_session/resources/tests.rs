use super::*;
use crate::{CoreKind, MihomoClient, MihomoEndpoint, MihomoError};
use std::{future::Future, task::Context, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn resource_session() -> (CoreSession, ProviderOperations, tokio::net::TcpListener) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = MihomoClient::new(MihomoEndpoint::new(
        format!("http://{}", listener.local_addr().unwrap()),
        "",
    ))
    .unwrap();
    let providers = ProviderOperations::new(client.clone());
    (
        CoreSession::open(CoreKind::Mihomo, client).unwrap(),
        providers,
        listener,
    )
}

async fn request(listener: &tokio::net::TcpListener, prefix: &str) -> tokio::net::TcpStream {
    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        headers.push(stream.read_u8().await.unwrap());
        assert!(headers.len() <= 8192);
    }
    assert!(String::from_utf8_lossy(&headers).starts_with(prefix));
    stream
}

async fn acknowledge(stream: &mut tokio::net::TcpStream) {
    stream
        .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
}
async fn catalog(stream: &mut tokio::net::TcpStream) {
    let body = r#"{"providers":{"airport":{"name":"airport","proxies":[{"name":"node"}]},"second":{"name":"second","proxies":[{"name":"node"}]}}}"#;
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

#[tokio::test]
async fn resource_action_rejects_another_actual_owner_with_equal_counters() {
    let (session, providers, listener) = resource_session().await;
    let foreign = CoreSession::open(CoreKind::Mihomo, session.client().clone()).unwrap();
    let source = foreign.begin_observation().await.unwrap();
    assert!(matches!(
        session
            .apply_observed_resource_action(
                source.identity(),
                &providers,
                CoreResourceAction::GeoData
            )
            .await,
        Err(CoreSessionError::Process(MihomoError::StaleBinding))
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn queued_resource_action_rechecks_configuration_before_sending() {
    let (session, providers, listener) = resource_session().await;
    let source = session.begin_observation().await.unwrap();
    let transition = session.transition.lock().await;
    let mut action = Box::pin(session.apply_observed_resource_action(
        source.identity(),
        &providers,
        CoreResourceAction::ExternalUi,
    ));
    assert!(
        action
            .as_mut()
            .poll(&mut Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
    session.invalidate_runtime_observations();
    drop(transition);
    assert!(matches!(
        action.await,
        Err(CoreSessionError::Process(MihomoError::StaleBinding))
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn resource_receipt_retains_the_same_observation_and_rejects_config_replacement_before_readback()
 {
    let (session, providers, listener) = resource_session().await;
    let source = session.begin_observation().await.unwrap();
    let server = tokio::spawn(async move {
        acknowledge(&mut request(&listener, "POST /configs/geo ").await).await;
    });
    let receipt = session
        .apply_observed_resource_action(source.identity(), &providers, CoreResourceAction::GeoData)
        .await
        .unwrap();
    assert!(
        receipt
            .observation()
            .identity()
            .is_same_observation(source.identity())
    );
    assert!(session.transition.try_lock().is_ok());
    session.invalidate_runtime_observations();
    assert!(
        receipt
            .observation()
            .wait_for_read_eligibility()
            .await
            .is_err()
    );
    server.await.unwrap();
}

#[tokio::test]
async fn admitted_provider_batch_survives_waiter_cancellation_and_shutdown_waits_for_all_targets() {
    let (session, providers, listener) = resource_session().await;
    let source = session
        .begin_observation()
        .await
        .unwrap()
        .identity()
        .clone();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut stream = request(&listener, "PUT /providers/proxies/airport ").await;
        entered.send(()).unwrap();
        released.await.unwrap();
        acknowledge(&mut stream).await;
        catalog(&mut request(&listener, "GET /providers/proxies ").await).await;
        acknowledge(&mut request(&listener, "PUT /providers/proxies/second ").await).await;
        catalog(&mut request(&listener, "GET /providers/proxies ").await).await;
    });
    let updating = session.clone();
    let histories = providers.clone();
    let waiter = tokio::spawn(async move {
        updating
            .apply_observed_resource_action(
                &source,
                &histories,
                CoreResourceAction::UpdateProviders(vec![
                    ProviderUpdateTarget::new(ProviderKind::Proxy, "airport"),
                    ProviderUpdateTarget::new(ProviderKind::Proxy, "second"),
                ]),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), started)
        .await
        .unwrap()
        .unwrap();
    waiter.abort();
    assert!(waiter.await.is_err_and(|error| error.is_cancelled()));
    assert!(session.transition.try_lock().is_err());
    let stopping = session.clone();
    let shutdown = tokio::spawn(async move { stopping.shutdown().await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !session.is_shutting_down() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!shutdown.is_finished());
    release.send(()).unwrap();
    server.await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        providers
            .status(ProviderKind::Proxy, "airport")
            .unwrap()
            .update
            .last_success_at_ms
            .is_some()
    );
    assert!(
        providers
            .status(ProviderKind::Proxy, "second")
            .unwrap()
            .update
            .last_success_at_ms
            .is_some()
    );
}

#[tokio::test]
async fn provider_update_and_healthcheck_keep_independent_history_on_the_admitted_runtime() {
    let (session, providers, listener) = resource_session().await;
    let source = session.begin_observation().await.unwrap();
    let server = tokio::spawn(async move {
        acknowledge(&mut request(&listener, "PUT /providers/proxies/airport ").await).await;
        catalog(&mut request(&listener, "GET /providers/proxies ").await).await;
        acknowledge(&mut request(&listener, "GET /providers/proxies/airport/healthcheck ").await)
            .await;
        catalog(&mut request(&listener, "GET /providers/proxies ").await).await;
    });
    let update = session
        .apply_observed_resource_action(
            source.identity(),
            &providers,
            CoreResourceAction::UpdateProvider {
                kind: ProviderKind::Proxy,
                key: "airport".into(),
            },
        )
        .await
        .unwrap();
    let health = session
        .apply_observed_resource_action(
            source.identity(),
            &providers,
            CoreResourceAction::HealthcheckProvider {
                key: "airport".into(),
            },
        )
        .await
        .unwrap();
    assert!(
        update
            .observation()
            .identity()
            .is_same_observation(health.observation().identity())
    );
    let status = providers.status(ProviderKind::Proxy, "airport").unwrap();
    assert!(status.update.last_success_at_ms.is_some());
    assert!(status.healthcheck.last_success_at_ms.is_some());
    server.await.unwrap();
}

#[tokio::test]
async fn a_resource_receipt_retains_no_runtime_write_authority() {
    use crate::{CoreConfigValidator, data_coordinator::DataWriteLease};
    let root = super::super::test_support::session_test_root("resource-receipt");
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
    let scopes = client.write_scopes();
    let providers = ProviderOperations::new(client.clone());
    let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
    let source = session.begin_observation().await.unwrap();
    let server = tokio::spawn(async move {
        acknowledge(&mut request(&listener, "POST /upgrade/ui ").await).await;
    });
    let receipt = session
        .apply_observed_resource_action(
            source.identity(),
            &providers,
            CoreResourceAction::ExternalUi,
        )
        .await
        .unwrap();
    let exclusive = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || DataWriteLease::exclusive(scopes)),
    )
    .await
    .unwrap()
    .unwrap();
    drop(exclusive);
    assert!(
        receipt
            .observation()
            .wait_for_read_eligibility()
            .await
            .is_ok()
    );
    server.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
