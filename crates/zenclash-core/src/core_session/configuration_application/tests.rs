use super::*;
use crate::core_session::service_tun::PreparedConfig;

#[cfg(unix)]
mod frozen_fallback;

#[tokio::test]
async fn local_config_applies_checked_payload_after_sources_change_and_rejects_stale_cache() {
    let fixture =
        crate::core_session::test_support::OwnedCoreFixture::new("geodata-frozen-local-config")
            .await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let home = fixture.process.launch_config().home_dir.clone();
    let store = ControlledConfigStore::new(home.join("controlled"));
    let profile = fixture.process.launch_config().config_file.clone();
    let overrides = home.join("override.yaml");
    std::fs::write(&profile, "tun: {enable: false}\nmode: rule\n").unwrap();
    std::fs::write(&overrides, "mode: direct\n").unwrap();
    let intent = EffectiveConfigIntent::ActivateProfile {
        profile: profile.clone(),
        overrides: vec![overrides.clone()],
    };
    let PreparedConfig::Local(prepared) = session
        .prepare_service_config(
            &store,
            crate::core_session::RuntimeConfigurationRevision {
                binding_generation: session.runtime_descriptor().binding_generation(),
                configuration_generation: 0,
            },
            intent.clone(),
        )
        .await
        .unwrap()
    else {
        panic!("expected Local candidate")
    };
    std::fs::write(&profile, "tun: {enable: true}\n").unwrap();
    std::fs::remove_file(&overrides).unwrap();
    let pid = fixture.process.snapshot().pid;
    let outcome = session
        .apply_prepared_local_config(&store, prepared)
        .await
        .unwrap();
    let cached: serde_yaml::Value =
        serde_yaml::from_str(&store.cached_runtime_payload().unwrap().unwrap()).unwrap();
    assert_eq!(cached["tun"]["enable"].as_bool(), Some(false));
    assert_eq!(cached["mode"].as_str(), Some("direct"));
    assert_eq!(outcome.generation, 1);
    assert_eq!(outcome.kind, CoreApplyKind::HotReloaded);
    assert_eq!(fixture.process.snapshot().pid, pid);
    assert_eq!(store.load_json().unwrap(), serde_json::json!({}));
    assert!(!store.root().join("override.yaml").exists());
    std::fs::write(&profile, "tun: {enable: false}\nmode: rule\n").unwrap();
    std::fs::write(&overrides, "mode: direct\n").unwrap();
    let PreparedConfig::Local(stale) = session
        .prepare_service_config(
            &store,
            crate::core_session::RuntimeConfigurationRevision {
                binding_generation: session.runtime_descriptor().binding_generation(),
                configuration_generation: 1,
            },
            intent,
        )
        .await
        .unwrap()
    else {
        panic!("expected Local candidate")
    };
    std::fs::write(store.runtime_path(), "mode: global\n").unwrap();
    assert!(matches!(
        session.apply_prepared_local_config(&store, stale).await,
        Err(CoreSessionError::Config(
            ControlledConfigError::ConcurrentModification
        ))
    ));
    assert_eq!(session.generation(), 1);
    assert_eq!(fixture.process.snapshot().pid, pid);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn local_config_restart_uses_frozen_cache_and_refuses_mutable_source_launch() {
    use tokio::io::AsyncWriteExt;
    for launch_cache in [false, true] {
        let fixture = crate::core_session::test_support::OwnedCoreFixture::new(
            "geodata-frozen-local-restart",
        )
        .await;
        let home = fixture.process.launch_config().home_dir.clone();
        let store = ControlledConfigStore::new(home.join("controlled"));
        let profile = fixture.process.launch_config().config_file.clone();
        std::fs::write(&profile, "tun: {enable: false}\nmode: rule\n").unwrap();
        store.materialize(&profile).unwrap();
        let process = if launch_cache {
            fixture.process.stop_async().await.unwrap();
            let mut launch = fixture.process.launch_config().clone();
            launch.config_file = store.runtime_path();
            crate::ManagedCoreProcess::spawn(launch).unwrap()
        } else {
            fixture.process.clone()
        };
        fixture.abort_responder();
        let address = process
            .endpoint()
            .controller
            .strip_prefix("http://")
            .unwrap();
        let listener = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match tokio::net::TcpListener::bind(address).await {
                    Ok(listener) => break listener,
                    Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                        tokio::task::yield_now().await
                    }
                    Err(error) => panic!("{error}"),
                }
            }
        })
        .await
        .unwrap();
        let responder = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let bytes = crate::test_support::read_controller_request(&mut stream)
                    .await
                    .unwrap()
                    .into_bytes();
                let count = bytes.len();
                if bytes[..count].starts_with(b"PUT ") {
                    continue;
                }
                let body = r#"{"meta":true,"version":"frozen-restart-fixture"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::from_process(process.clone()).unwrap(),
        )
        .unwrap();
        let PreparedConfig::Local(prepared) = session
            .prepare_service_config(
                &store,
                crate::core_session::RuntimeConfigurationRevision {
                    binding_generation: session.runtime_descriptor().binding_generation(),
                    configuration_generation: 0,
                },
                EffectiveConfigIntent::Patch {
                    profile: profile.clone(),
                    patch: serde_json::json!({"mode":"direct"}),
                    overrides: vec![],
                },
            )
            .await
            .unwrap()
        else {
            panic!("expected Local candidate")
        };
        std::fs::write(&profile, "tun: {enable: true}\nmode: global\n").unwrap();
        let pid = process.snapshot().pid;
        let outcome = session.apply_prepared_local_config(&store, prepared).await;
        let cache: serde_yaml::Value =
            serde_yaml::from_str(&store.cached_runtime_payload().unwrap().unwrap()).unwrap();
        assert_eq!(cache["tun"]["enable"].as_bool(), Some(false));
        if launch_cache {
            assert_eq!(outcome.unwrap().kind, CoreApplyKind::Restarted);
            assert_eq!(cache["mode"].as_str(), Some("direct"));
            assert_ne!(process.snapshot().pid, pid);
            assert_eq!(store.load_json().unwrap()["mode"], "direct");
        } else {
            assert!(outcome.is_err());
            assert_eq!(cache["mode"].as_str(), Some("rule"));
            assert_eq!(process.snapshot().pid, pid);
            assert_eq!(store.load_json().unwrap(), serde_json::json!({}));
        }
        session.shutdown().await.unwrap();
        responder.abort();
    }
}

#[tokio::test]
async fn catalog_restart_reuses_final_payload_when_sources_change_after_hot_reload_attempt() {
    use tokio::io::AsyncWriteExt;
    let fixture =
        crate::core_session::test_support::OwnedCoreFixture::new("geodata-catalog-frozen-restart")
            .await;
    let home = fixture.process.launch_config().home_dir.clone();
    let store = ControlledConfigStore::new(home.join("controlled"));
    let profile = fixture.process.launch_config().config_file.clone();
    let override_path = home.join("catalog-override.yaml");
    std::fs::write(&profile, "tun: {enable: false}\nmode: rule\n").unwrap();
    std::fs::write(&override_path, "mode: direct\n").unwrap();
    store
        .materialize_with_overrides(&profile, std::slice::from_ref(&override_path))
        .unwrap();
    fixture.process.stop_async().await.unwrap();
    let mut launch = fixture.process.launch_config().clone();
    launch.config_file = store.runtime_path();
    let process = crate::ManagedCoreProcess::spawn(launch).unwrap();
    fixture.abort_responder();
    let address = process
        .endpoint()
        .controller
        .strip_prefix("http://")
        .unwrap();
    let listener = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match tokio::net::TcpListener::bind(address).await {
                Ok(listener) => break listener,
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                    tokio::task::yield_now().await
                }
                Err(error) => panic!("{error}"),
            }
        }
    })
    .await
    .unwrap();
    let changed_profile = profile.clone();
    let changed_override = override_path.clone();
    let controlled_patch = store.root().join("override.yaml");
    let (observed, received) = tokio::sync::oneshot::channel();
    let responder = tokio::spawn(async move {
        let mut observed = Some(observed);
        while let Ok((mut stream, _)) = listener.accept().await {
            let request = crate::test_support::read_controller_request(&mut stream)
                .await
                .unwrap()
                .into_bytes();
            if request.starts_with(b"PUT ") {
                let header_end = request
                    .windows(4)
                    .position(|bytes| bytes == b"\r\n\r\n")
                    .unwrap()
                    + 4;
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let length: usize = headers
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .unwrap()
                    .1
                    .trim()
                    .parse()
                    .unwrap();

                let payload = serde_json::from_slice::<serde_json::Value>(
                    &request[header_end..header_end + length],
                )
                .unwrap()["payload"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                std::fs::write(&changed_profile, "tun: {enable: true}\nmode: global\n").unwrap();
                std::fs::write(&changed_override, "tun: {enable: true}\nmode: global\n").unwrap();
                std::fs::write(&controlled_patch, "tun: {enable: true}\nmode: global\n").unwrap();
                if let Some(observed) = observed.take() {
                    observed.send(payload).unwrap();
                }
                continue;
            }
            let body = r#"{"meta":true,"version":"catalog-restart-fixture"}"#;
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
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(process.clone()).unwrap(),
    )
    .unwrap();
    let pid = process.snapshot().pid;
    let staged = session
        .stage_profile_application(&store, profile.clone(), None, vec![override_path], true)
        .await
        .unwrap();
    let outcome = staged.commit(profile).await.unwrap().unwrap();
    let attempted: serde_yaml::Value = serde_yaml::from_str(&received.await.unwrap()).unwrap();
    let cached: serde_yaml::Value =
        serde_yaml::from_str(&store.cached_runtime_payload().unwrap().unwrap()).unwrap();
    assert_eq!(attempted["tun"]["enable"].as_bool(), Some(false));
    assert_eq!(attempted["mode"].as_str(), Some("direct"));
    assert_eq!(
        cached, attempted,
        "restart must consume the final payload from the failed hot reload"
    );
    assert_eq!(outcome.kind, CoreApplyKind::Restarted);
    assert_ne!(process.snapshot().pid, pid);
    session.shutdown().await.unwrap();
    responder.abort();
}

use super::super::test_support::session_test_root;
use crate::MihomoEndpoint;

#[tokio::test]
async fn admitted_mode_rejected_before_send_does_not_advance_generation() {
    use std::{future::Future, task::Context};
    use tokio::io::AsyncWriteExt;

    let root = session_test_root("admitted-mode-binding");
    std::fs::create_dir_all(&root).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (received, waiting) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _request = crate::test_support::read_controller_request(&mut stream)
            .await
            .unwrap();
        received.send(()).unwrap();
        released.await.unwrap();
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
    let blocker = {
        let client = client.clone();
        tokio::spawn(async move { client.patch_configs(&serde_json::json!({})).await })
    };
    waiting.await.unwrap();
    let replacement = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut switch = Box::pin(client.switch_to_direct(MihomoEndpoint::new(
        format!("http://{}", replacement.local_addr().unwrap()),
        "",
    )));
    assert!(
        switch
            .as_mut()
            .poll(&mut Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
    let lease = DataWriteLease::shared([root.clone()]);
    let store = ControlledConfigStore::new(root.join("store")).with_write_lease(&lease);
    let pinned = client
        .pin_binding()
        .unwrap()
        .with_write_lease(&lease)
        .unwrap();
    let transition = session.transition.clone().lock_owned().await;
    let mut operation = Box::pin(session.set_mode_admitted(store, pinned, transition, "global"));
    assert!(
        operation
            .as_mut()
            .poll(&mut Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
    release.send(()).unwrap();
    blocker.await.unwrap().unwrap();
    switch.await.unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), operation)
            .await
            .unwrap(),
        Err(CoreSessionError::Process(MihomoError::StaleBinding))
    ));
    assert!(
        replacement
            .poll_accept(&mut Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
    assert_eq!(session.generation(), 0);
    server.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
