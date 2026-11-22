use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::AsyncWriteExt;
use zenclash_core::{
    AppPreferences, ControlledConfigStore, CoreKind, CoreSessionError, MihomoClient,
    MihomoEndpoint, TrafficHistoryStore,
};

use super::*;

#[tokio::test]
async fn native_shutdown_waits_for_capture_release_before_stopping_the_core() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let core = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::new(
            format!("http://{}", listener.local_addr().unwrap()),
            "",
        ))
        .unwrap(),
    )
    .unwrap();
    let capture = TrafficCaptureSession::new(
        core.clone(),
        ControlledConfigStore::new(std::env::temp_dir()),
        None,
        None,
    );
    let stopping = core.clone();
    let generation = core.generation();
    let shutdown =
        tokio::spawn(async move { finish_native_shutdown(&stopping, Some(&capture), None).await });
    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let request = zenclash_core::test_support::read_controller_request(&mut stream)
        .await
        .unwrap();
    assert!(request.into_parts().0.starts_with("GET /configs "));
    assert!(!shutdown.is_finished());
    assert_eq!(
        core.generation(),
        generation,
        "the core must wait for capture readback"
    );
    let body = r#"{"tun":{"enable":false}}"#;
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
    tokio::time::timeout(Duration::from_secs(2), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(core.generation() > generation);
}

#[tokio::test]
async fn failed_capture_release_prevents_core_shutdown_and_success_allows_it() {
    let core = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::new("http://127.0.0.1:1", "")).unwrap(),
    )
    .unwrap();
    let store = ControlledConfigStore::new(std::env::temp_dir());
    let capture = TrafficCaptureSession::new(core.clone(), store.clone(), None, None);
    let released = capture.release_owned().await.unwrap();
    let coordinator = ShutdownCoordinator::new(core.clone(), Some(capture), None);
    let partial = CaptureOutcome::ReconcileNeeded {
        plan: None,
        snapshot: released.snapshot().clone(),
        failure: "native permission rejected".into(),
    };
    for failure in [
        Ok(partial),
        Err(TrafficCaptureError::Controller(
            zenclash_core::MihomoError::Api {
                status: 503,
                message: "release task failed".into(),
            },
        )),
    ] {
        let outcome = coordinator.execute_interactive_with_release(failure).await;
        assert!(matches!(outcome, ShutdownOutcome::Blocked(_)));
        assert!(!core.is_shutting_down());
        assert!(!matches!(
            core.set_mode(&store, "rule").await,
            Err(CoreSessionError::ShuttingDown)
        ));
    }
    assert_eq!(
        coordinator
            .execute_interactive_with_release(Ok(released))
            .await,
        ShutdownOutcome::ReadyToExit
    );
    assert!(matches!(
        core.set_mode(&store, "rule").await,
        Err(CoreSessionError::ShuttingDown)
    ));
}

#[tokio::test]
async fn failed_history_flush_blocks_shutdown_without_poisoning_core_session() {
    let root = std::env::temp_dir().join(format!(
        "zenclash-shutdown-history-blocked-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db_path = root.join("traffic.sqlite3");
    // Create a directory at the SQLite file path so final flush fails deterministically.
    std::fs::create_dir_all(&db_path).unwrap();
    let client = MihomoClient::new(MihomoEndpoint::new("http://127.0.0.1:1", "")).unwrap();
    let core = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
    let store = TrafficHistoryStore::new(&db_path);
    let history = TrafficHistorySession::start(
        &tokio::runtime::Handle::current(),
        client,
        store,
        &AppPreferences::default(),
    );
    let coordinator = ShutdownCoordinator::new(core.clone(), None, Some(history));
    let outcome = coordinator.execute_interactive().await;
    assert!(matches!(outcome, ShutdownOutcome::Blocked(_)));
    assert!(
        !core.is_shutting_down(),
        "Stage 1 history failure must not poison CoreSession"
    );
    let _ = std::fs::remove_dir_all(root);
}
