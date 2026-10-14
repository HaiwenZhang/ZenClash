use super::super::test_support::{
    session_test_root, spawn_test_core, spawn_test_core_with_endpoint, write_test_core_script,
};
use super::*;
use crate::MihomoEndpoint;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

use crate::CoreLaunchConfig;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_cancels_a_restart_waiter_and_prevents_a_late_child() {
    let root = session_test_root("shutdown-cancel");
    let launches = root.join("launches");
    let binary = root.join("mihomo");
    write_test_core_script(
        &binary,
        &format!(
            "printf x >> '{}'; trap 'exit 0' TERM; while true; do sleep 0.05; done",
            launches.display()
        ),
    );
    let process = spawn_test_core(&root, binary);
    // Spawning returns before the shell has written its launch marker. Restart
    // must follow that marker, otherwise stopping the initial child can erase
    // the first launch from the count this test uses to identify its candidate.
    tokio::time::timeout(Duration::from_secs(5), async {
        while std::fs::read_to_string(&launches).map_or(0, |value| value.len()) < 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("initial child did not write its launch marker");
    assert_eq!(std::fs::read_to_string(&launches).unwrap().len(), 1);

    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(process.clone()).unwrap(),
    )
    .unwrap();
    let restarting = {
        let session = session.clone();
        tokio::spawn(async move {
            session
                .maintain_with_timeout(CoreMaintenanceIntent::Restart, Duration::from_secs(2))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        while std::fs::read_to_string(&launches).map_or(0, |value| value.len()) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("restart did not launch its candidate child");

    tokio::time::timeout(Duration::from_secs(1), session.shutdown())
        .await
        .expect("shutdown waited for the full readiness timeout")
        .unwrap();
    let restart_error = restarting.await.unwrap().unwrap_err().to_string();
    let launch_count = std::fs::read_to_string(&launches).unwrap().len();
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(restart_error.contains("停止请求"));
    assert_eq!(launch_count, 2, "shutdown must not permit a later restart");
    assert!(!process.is_running());
    assert_eq!(
        session.lifecycle_snapshot().phase,
        CoreLifecyclePhase::Stopped
    );
    assert!(matches!(
        session.maintain(CoreMaintenanceIntent::Restart).await,
        Err(CoreSessionError::ShuttingDown)
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_during_configuration_validation_does_not_launch_a_candidate_or_recovery_child() {
    let _ports = fixed_listener_ports_guard().await;
    use tokio::io::AsyncWriteExt;
    let root = session_test_root("config-restart-shutdown");
    let source = root.join("source.yaml");
    let candidate = root.join("candidate.yaml");
    let launches = root.join("launches");
    let armed = root.join("armed");
    let validating = root.join("validating");
    let release = root.join("release");
    std::fs::write(&source, "mixed-port: 8011\nmode: rule\n").unwrap();
    std::fs::write(&candidate, "mixed-port: 8012\nmode: rule\n").unwrap();
    let store = ControlledConfigStore::new(root.join("controlled"));
    store
        .materialize_with_overrides_for_core(&source, &[], CoreKind::Mihomo)
        .unwrap();
    let previous_cache = std::fs::read(store.runtime_path()).unwrap();
    let binary = root.join("mihomo");
    std::fs::write(&binary, format!("#!/bin/sh\nif [ \"$1\" = '-t' ]; then\n  if [ -f '{}' ]; then touch '{}'; while [ ! -f '{}' ]; do sleep 0.01; done; fi\n  exit 0\nfi\nprintf x >> '{}'\ntrap 'exit 0' TERM INT\nwhile true; do sleep 0.01; done\n", armed.display(), validating.display(), release.display(), launches.display())).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let endpoint = MihomoEndpoint::new(format!("http://{}", listener.local_addr().unwrap()), "");
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
    let server_launches = launches.clone();
    let server = tokio::spawn(async move {
        loop {
            let accepted = tokio::select! { accepted = listener.accept() => accepted.unwrap(), _ = &mut stop_rx => break };
            let (mut stream, _) = accepted;
            let request = crate::test_support::read_controller_request(&mut stream)
                .await
                .unwrap()
                .into_bytes();
            if request.starts_with(b"PUT ") {
                continue;
            } // A lost reload response forces managed restart.
            assert!(request.starts_with(b"GET /version "));
            tokio::time::timeout(Duration::from_secs(5), async {
                while std::fs::read(&server_launches).unwrap().len() < 2 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            let body = r#"{"meta":true,"version":"fixture"}"#;
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
    });
    let process = ManagedCoreProcess::spawn(CoreLaunchConfig {
        kind: CoreKind::Mihomo,
        binary,
        config_file: store.runtime_path(),
        home_dir: root.join("data"),
        endpoint: endpoint.clone(),
        controller_override: None,
    })
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !launches.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let session = CoreSession::open_with_config(
        CoreKind::Mihomo,
        MihomoClient::from_process(process.clone()).unwrap(),
        Some(source),
        vec![],
    )
    .unwrap();
    std::fs::write(&armed, []).unwrap();
    let apply_session = session.clone();
    let apply_store = store.clone();
    let apply = tokio::spawn(async move {
        apply_session
            .apply(
                &apply_store,
                EffectiveConfigIntent::ActivateProfile {
                    profile: candidate,
                    overrides: vec![],
                },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !validating.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    session.request_shutdown();
    let shutdown_session = session.clone();
    let shutdown = tokio::spawn(async move { shutdown_session.shutdown().await });
    std::fs::write(&release, []).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), apply)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    stop_tx.send(()).unwrap();
    server.await.unwrap();
    let launch_count = std::fs::read(&launches).unwrap().len();
    let cache = std::fs::read(store.runtime_path()).unwrap();
    let running = process.is_running();
    std::fs::remove_dir_all(root).unwrap();
    assert!(
        result.is_err(),
        "configuration restart was accepted after shutdown: {result:?}"
    );
    assert_eq!(
        launch_count, 1,
        "shutdown launched a candidate or recovery child"
    );
    assert_eq!(cache, previous_cache);
    assert!(!running);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_restarts_serialize_without_overlapping_children() {
    let root = session_test_root("serialized-restarts");
    let launches = root.join("launches");
    let guard = root.join("running");
    let overlaps = root.join("overlaps");
    let binary = root.join("mihomo");
    write_test_core_script(
        &binary,
        &format!(
            "printf x >> '{}'; if ! mkdir '{}' 2>/dev/null; then printf x >> '{}'; exit 99; fi; cleanup() {{ rmdir '{}' 2>/dev/null || true; }}; trap 'cleanup; exit 0' TERM INT; trap cleanup EXIT; while true; do sleep 0.05; done",
            launches.display(),
            guard.display(),
            overlaps.display(),
            guard.display(),
        ),
    );
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let endpoint = MihomoEndpoint::new(format!("http://{}", listener.local_addr().unwrap()), "");
    let server = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2_048];
            let _ = stream.read(&mut request).unwrap();
            let body = r#"{"meta":true,"version":"test"}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .unwrap();
        }
    });
    let process = spawn_test_core_with_endpoint(&root, binary, endpoint.clone());
    tokio::time::timeout(Duration::from_secs(5), async {
        while !guard.is_dir() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("initial managed child did not acquire its process guard");
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(process.clone()).unwrap(),
    )
    .unwrap();

    let first = {
        let session = session.clone();
        tokio::spawn(async move { session.maintain(CoreMaintenanceIntent::Restart).await })
    };
    let second = {
        let session = session.clone();
        tokio::spawn(async move { session.maintain(CoreMaintenanceIntent::Restart).await })
    };
    let (first, second) = tokio::join!(first, second);

    let generations = [first.unwrap().unwrap(), second.unwrap().unwrap()];
    tokio::time::timeout(Duration::from_secs(5), async {
        while !guard.is_dir() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("latest managed child did not acquire its process guard");
    let launch_count = std::fs::read_to_string(&launches).unwrap().len();
    assert!(generations.contains(&1));
    assert!(generations.contains(&2));
    assert_eq!(session.snapshot().generation, 2);
    assert!(
        (2..=3).contains(&launch_count),
        "the initial and latest child must run; an immediately replaced child may not be scheduled"
    );
    assert!(!overlaps.exists(), "two managed children overlapped");
    assert!(process.is_running());
    session.shutdown().await.unwrap();
    server.join().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
