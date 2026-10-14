use super::test_support::{acknowledge, actual_member, proxy_action_session, request};
use super::*;
use crate::{
    CoreKind, MihomoClient, MihomoEndpoint, MihomoError, ProxyCatalog, ProxyGroup,
    ProxySelectionReadback,
};
use std::{future::Future, task::Context, time::Duration};

#[tokio::test]
async fn observed_selection_rejects_an_equal_revision_from_a_foreign_session() {
    let (session, listener) = proxy_action_session().await;
    let foreign = CoreSession::open(CoreKind::Mihomo, session.client().clone()).unwrap();
    let observation = foreign.begin_observation().await.unwrap();
    assert!(matches!(
        session
            .accept_observed_proxy_selection(
                observation.identity(),
                "group",
                "Requested",
                ConnectionPolicy::KeepExisting
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
async fn queued_observed_selection_rechecks_configuration_after_transition_admission() {
    let (session, listener) = proxy_action_session().await;
    let observation = session.begin_observation().await.unwrap();
    let transition = session.transition.lock().await;
    let mut selection = Box::pin(session.accept_observed_proxy_selection(
        observation.identity(),
        "group",
        "Requested",
        ConnectionPolicy::KeepExisting,
    ));
    assert!(
        selection
            .as_mut()
            .poll(&mut Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
    session.invalidate_runtime_observations();
    drop(transition);
    assert!(matches!(
        selection.await,
        Err(CoreSessionError::Process(MihomoError::StaleBinding))
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancelling_selection_waiter_preserves_admitted_acknowledgement_and_shutdown_waits() {
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
        let mut stream = request(&listener, "PUT /proxies/group ").await;
        entered.send(()).unwrap();
        released.await.unwrap();
        acknowledge(&mut stream).await;
    });
    let selecting = session.clone();
    let waiter = tokio::spawn(async move {
        selecting
            .accept_observed_proxy_selection(
                &identity,
                "group",
                "Requested",
                ConnectionPolicy::KeepExisting,
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
}

#[tokio::test]
async fn observed_selection_receipt_reads_actual_member_and_rejects_later_publication() {
    let (session, listener) = proxy_action_session().await;
    let observation = session.begin_observation().await.unwrap();
    let server = tokio::spawn(async move {
        acknowledge(&mut request(&listener, "PUT /proxies/group ").await).await;
        actual_member(&mut request(&listener, "GET /proxies/group ").await).await;
    });
    let accepted = session
        .accept_observed_proxy_selection(
            observation.identity(),
            "group",
            "Requested",
            ConnectionPolicy::KeepExisting,
        )
        .await
        .unwrap();
    assert!(session.transition.try_lock().is_ok());
    assert_eq!(accepted.requested_member(), "Requested");
    let ProxySelectionReadback::Observed(observed) = accepted.read_actual().await else {
        panic!("missing controller fact");
    };
    assert_eq!(observed.member(), "Actual");
    let mut catalog = ProxyCatalog::from_group_nodes(
        vec![(
            ProxyGroup::new("group").with_current_member("Previous"),
            Vec::new(),
        )],
        0,
    );
    assert!(observed.apply_to(&mut catalog));
    assert_eq!(catalog.groups()[0].now, "Actual");
    session.invalidate_runtime_observations();
    catalog.set_group_selection("group", "New configuration".into());
    assert!(!observed.apply_to(&mut catalog));
    assert_eq!(catalog.groups()[0].now, "New configuration");
    assert!(!accepted.is_current());
    assert!(matches!(
        accepted.read_actual().await,
        ProxySelectionReadback::RuntimeChanged
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn selection_readback_in_flight_cannot_confirm_a_replaced_configuration() {
    let (session, listener) = proxy_action_session().await;
    let observation = session.begin_observation().await.unwrap();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        acknowledge(&mut request(&listener, "PUT /proxies/group ").await).await;
        let mut stream = request(&listener, "GET /proxies/group ").await;
        entered.send(()).unwrap();
        released.await.unwrap();
        actual_member(&mut stream).await;
    });
    let accepted = session
        .accept_observed_proxy_selection(
            observation.identity(),
            "group",
            "Requested",
            ConnectionPolicy::KeepExisting,
        )
        .await
        .unwrap();
    let readback = tokio::spawn(async move { accepted.read_actual().await });
    tokio::time::timeout(Duration::from_secs(2), started)
        .await
        .unwrap()
        .unwrap();
    session.invalidate_runtime_observations();
    release.send(()).unwrap();
    assert!(matches!(
        readback.await.unwrap(),
        ProxySelectionReadback::RuntimeChanged
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn a_selection_receipt_retains_no_runtime_write_lease() {
    use crate::{CoreConfigValidator, data_coordinator::DataWriteLease};
    let root = super::super::test_support::session_test_root("proxy-selection-receipt");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = MihomoClient::new(MihomoEndpoint::new(
        format!("http://{}", listener.local_addr().unwrap()),
        "",
    ))
    .unwrap()
    .with_config_validator(CoreConfigValidator::new(
        CoreKind::Mihomo,
        root.join("unused-kernel"),
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
        acknowledge(&mut request(&listener, "PUT /proxies/group ").await).await;
    });
    let accepted = session
        .accept_observed_proxy_selection(
            &identity,
            "group",
            "Requested",
            ConnectionPolicy::KeepExisting,
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
    assert!(accepted.is_current());
    assert!(session.transition.try_lock().is_ok());
    drop(accepted);
    server.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn selection_confirmation_and_diagnostics_publish_only_under_the_original_source_guard() {
    let (session, listener) = proxy_action_session().await;
    let observation = session.begin_observation().await.unwrap();
    let server = tokio::spawn(async move {
        acknowledge(&mut request(&listener, "PUT /proxies/group ").await).await;
        actual_member(&mut request(&listener, "GET /proxies/group ").await).await;
    });
    let accepted = session
        .accept_observed_proxy_selection(
            observation.identity(),
            "group",
            "Requested",
            ConnectionPolicy::KeepExisting,
        )
        .await
        .unwrap();
    let ProxySelectionReadback::Observed(observed) = accepted.read_actual().await else {
        panic!("missing actual selection");
    };
    let mut catalog = ProxyCatalog::from_group_nodes(
        vec![(
            ProxyGroup::new("group").with_current_member("Previous"),
            Vec::new(),
        )],
        0,
    );
    let mut confirmed = None;
    assert!(observed.apply_to_with(&mut catalog, |actual| {
        assert!(
            session.transition.try_lock().is_err(),
            "confirmation escaped its source guard"
        );
        confirmed = Some(actual.to_owned());
    }));
    assert_eq!(confirmed.as_deref(), Some("Actual"));
    assert_eq!(catalog.groups()[0].now, "Actual");
    assert!(
        accepted
            .publish_if_current(|| {
                assert!(
                    session.transition.try_lock().is_err(),
                    "diagnostic escaped its source guard"
                );
            })
            .is_ok()
    );
    session.invalidate_runtime_observations();
    catalog.set_group_selection("group", "Current configuration".into());
    assert!(!observed.apply_to_with(&mut catalog, |_| panic!("old confirmation published")));
    assert!(
        accepted
            .publish_if_current(|| panic!("old diagnostic published"))
            .is_err()
    );
    assert_eq!(catalog.groups()[0].now, "Current configuration");
    server.await.unwrap();
}
