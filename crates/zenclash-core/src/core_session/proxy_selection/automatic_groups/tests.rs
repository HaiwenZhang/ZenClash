use super::super::test_support::{acknowledge, proxy_action_session, request};
use super::*;
use crate::{CoreKind, MihomoError, ProxyGroupBehavior};
use std::{future::Future, task::Context, time::Duration};
use tokio::io::AsyncWriteExt;

async fn catalog_response(stream: &mut tokio::net::TcpStream) {
    let body = r#"{"proxies":{"group":{"type":"URLTest","now":"Actual","all":["Actual"],"fixed":false},"Actual":{"type":"Shadowsocks"}}}"#;
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

async fn delays_response(stream: &mut tokio::net::TcpStream) {
    let body = r#"{"Actual":42}"#;
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
async fn group_measurement_rejects_an_equal_revision_from_another_session() {
    let (session, listener) = proxy_action_session().await;
    let foreign = CoreSession::open(CoreKind::Mihomo, session.client().clone()).unwrap();
    let identity = foreign
        .begin_observation()
        .await
        .unwrap()
        .identity()
        .clone();
    assert!(matches!(
        session
            .measure_observed_proxy_group(&identity, "group", None, 500)
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
async fn queued_automatic_restore_rechecks_the_observed_configuration_after_waiting() {
    let (session, listener) = proxy_action_session().await;
    let identity = session
        .begin_observation()
        .await
        .unwrap()
        .identity()
        .clone();
    let transition = session.transition.lock().await;
    let mut restoring = Box::pin(session.restore_observed_proxy_group(&identity, "group"));
    assert!(
        restoring
            .as_mut()
            .poll(&mut Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
    session.invalidate_runtime_observations();
    drop(transition);
    assert!(matches!(
        restoring.await,
        Err(CoreSessionError::Process(MihomoError::StaleBinding))
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancelling_automatic_restore_waiter_keeps_admitted_work_and_shutdown_waits() {
    let (session, listener) = proxy_action_session().await;
    let identity = session
        .begin_observation()
        .await
        .unwrap()
        .identity()
        .clone();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut stream = request(&listener, "DELETE /proxies/group ").await;
        entered.send(()).unwrap();
        released.await.unwrap();
        acknowledge(&mut stream).await;
        catalog_response(&mut request(&listener, "GET /proxies ").await).await;
    });
    let restoring = session.clone();
    let waiter = tokio::spawn(async move {
        restoring
            .restore_observed_proxy_group(&identity, "group")
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
}

#[tokio::test]
async fn cancelling_group_measurement_waiter_keeps_readback_owned_until_shutdown_can_finish() {
    let (session, listener) = proxy_action_session().await;
    let identity = session
        .begin_observation()
        .await
        .unwrap()
        .identity()
        .clone();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        delays_response(&mut request(&listener, "GET /group/group/delay?").await).await;
        let mut stream = request(&listener, "GET /proxies ").await;
        entered.send(()).unwrap();
        released.await.unwrap();
        catalog_response(&mut stream).await;
    });
    let measuring = session.clone();
    let waiter = tokio::spawn(async move {
        measuring
            .measure_observed_proxy_group(&identity, "group", None, 500)
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), started)
        .await
        .unwrap()
        .unwrap();
    waiter.abort();
    assert!(waiter.await.is_err_and(|error| error.is_cancelled()));
    assert!(
        session.transition.try_lock().is_err(),
        "accepted group-delay readback escaped its transition"
    );
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
}

#[tokio::test]
async fn measurement_receipt_keeps_delays_actual_selection_and_original_publication_identity() {
    let (session, listener) = proxy_action_session().await;
    let identity = session
        .begin_observation()
        .await
        .unwrap()
        .identity()
        .clone();
    let server = tokio::spawn(async move {
        delays_response(&mut request(&listener, "GET /group/group/delay?").await).await;
        catalog_response(&mut request(&listener, "GET /proxies ").await).await;
    });
    let receipt = session
        .measure_observed_proxy_group(&identity, "group", None, 500)
        .await
        .unwrap();
    assert_eq!(receipt.outcome().delays.get("Actual"), Some(&42));
    assert_eq!(
        receipt.outcome().selection.actual.as_deref(),
        Some("Actual")
    );
    assert_eq!(
        receipt
            .outcome()
            .selection
            .catalog
            .as_ref()
            .unwrap()
            .groups()[0]
            .behavior,
        ProxyGroupBehavior::Automatic { fixed: false }
    );
    assert!(
        receipt
            .operations()
            .observation_identity()
            .unwrap()
            .is_same_observation(&identity)
    );
    assert!(receipt.operations().publish_if_current(|| ()).is_ok());
    assert!(session.transition.try_lock().is_ok());
    session.invalidate_runtime_observations();
    assert!(
        receipt
            .operations()
            .publish_if_current(|| panic!("stale group result published"))
            .is_err()
    );
    server.await.unwrap();
}

#[tokio::test]
async fn restoration_readback_failure_is_a_warning_and_receipt_releases_write_authority() {
    use crate::{
        CoreConfigValidator, MihomoClient, MihomoEndpoint, data_coordinator::DataWriteLease,
    };
    let root = super::super::super::test_support::session_test_root("automatic-group-receipt");
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
    let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
    let identity = session
        .begin_observation()
        .await
        .unwrap()
        .identity()
        .clone();
    let server = tokio::spawn(async move {
        acknowledge(&mut request(&listener, "DELETE /proxies/group ").await).await;
        request(&listener, "GET /proxies ").await.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
    });
    let receipt = session
        .restore_observed_proxy_group(&identity, "group")
        .await
        .unwrap();
    assert!(receipt.outcome().actual.is_none());
    assert!(receipt.outcome().catalog.is_none());
    assert_eq!(receipt.outcome().warnings.len(), 1);
    let exclusive = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || DataWriteLease::exclusive(scopes)),
    )
    .await
    .unwrap()
    .unwrap();
    drop(exclusive);
    assert!(receipt.operations().pin_current_runtime().is_ok());
    let (operations, outcome) = receipt.into_parts();
    assert!(operations.publish_if_current(|| ()).is_ok());
    assert_eq!(outcome.warnings.len(), 1);
    server.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn observed_selector_group_delays_use_one_request_without_catalog_readback() {
    let (session, listener) = proxy_action_session().await;
    let identity = session
        .begin_observation()
        .await
        .unwrap()
        .identity()
        .clone();
    let server = tokio::spawn(async move {
        delays_response(&mut request(&listener, "GET /group/group/delay?").await).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    });
    let receipt = session
        .measure_observed_proxy_group_delays(&identity, "group", None, 500)
        .await
        .unwrap();
    assert_eq!(receipt.outcome().get("Actual"), Some(&42));
    assert!(
        receipt
            .operations()
            .observation_identity()
            .unwrap()
            .is_same_observation(&identity)
    );
    assert!(session.transition.try_lock().is_ok());
    server.await.unwrap();
}

#[tokio::test]
async fn selector_group_delays_reject_an_observation_changed_while_waiting_for_admission() {
    let (session, listener) = proxy_action_session().await;
    let identity = session
        .begin_observation()
        .await
        .unwrap()
        .identity()
        .clone();
    let transition = session.transition.lock().await;
    let mut measurement =
        Box::pin(session.measure_observed_proxy_group_delays(&identity, "group", None, 500));
    assert!(
        measurement
            .as_mut()
            .poll(&mut Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
    session.invalidate_runtime_observations();
    drop(transition);
    assert!(matches!(
        measurement.await,
        Err(CoreSessionError::Process(MihomoError::StaleBinding))
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}
