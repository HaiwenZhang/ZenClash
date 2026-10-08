// ZenClash application IPC integration, 2026-10-05. GPL-3.0-only; see NOTICE.md.
#![cfg(feature = "service-ipc-tests")]

use std::{
    io::{Read as _, Write as _},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, ensure};
use zenclash_core::service::{RunningMode, RuntimeBundle, ServiceSession};
use zenclash_core::{
    ControlledConfigStore, CoreKind, CoreLifecyclePhase, CoreRuntimeBackend, CoreSession,
    EffectiveConfigIntent, MihomoClient, PacServer, ServiceHealthKind, default_pac_script,
};

async fn pac_status(server: &PacServer) -> Result<String> {
    let address = server
        .status()
        .context("PAC fixture is not listening")?
        .address;
    tokio::task::spawn_blocking(move || {
        let mut stream = std::net::TcpStream::connect(address)?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.write_all(b"GET /pac HTTP/1.1\r\n\r\n")?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        Ok::<_, anyhow::Error>(response)
    })
    .await?
}

#[tokio::test]
async fn application_start_reload_displacement_and_shutdown_use_the_actual_native_owner()
-> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "zenclash-application-native-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
    ));
    std::fs::create_dir(&root)?;
    // Native service authentication requires the caller's SID even on elevated CI.
    #[cfg(windows)]
    zenclash_core::service::repair_app_data_root_owner(&root)?;
    let home = root.join("home");
    std::fs::create_dir(&home)?;
    let profile = root.join("profile.yaml");
    std::fs::write(
        &profile,
        "mode: rule\ntun: {enable: false}\nrules: [MATCH,DIRECT]\n",
    )?;
    let binary =
        std::path::PathBuf::from(env!("CARGO_BIN_EXE_zenclash-core-integration-test-core"));
    let store = ControlledConfigStore::new(root.join("controlled"));
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(zenclash_service::run_ipc_supervisor_until_shutdown(async {
        let _ = shutdown_rx.await;
    }));
    let mut original = None;
    let mut replacement = None;
    let pac = PacServer::default();
    let workflow = Box::pin(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while zenclash_service::service_lifecycle_state()
                != zenclash_service::ServiceLifecycleState::Running
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .context("native supervisor did not become ready")?;
        let client = tokio::time::timeout(
            Duration::from_secs(10),
            MihomoClient::connect_service_with_core(home.clone(), Some(binary.clone())),
        )
        .await??;
        let session = CoreSession::open(CoreKind::Mihomo, client.clone())?;
        original = Some(session.clone());
        session.attach_pac_server(pac.clone());
        let listener = pac.start("127.0.0.1", default_pac_script(), 17_890)?;
        ensure!(session.run_state().mode == RunningMode::NotRunning);
        ensure!(pac_status(&pac).await?.starts_with("HTTP/1.1 503"));

        let initialized = session
            .initialize_service_runtime(&store, profile.clone(), Vec::new())
            .await?;
        ensure!(
            initialized.failure().is_none(),
            "initialization failed: {:?}",
            initialized.failure()
        );
        ensure!(initialized.saved().is_some());
        ensure!(!initialized.commit_pending());
        session.record_startup_service_health(ServiceHealthKind::Ready);
        ensure!(session.runtime_descriptor().backend() == CoreRuntimeBackend::Service);
        ensure!(
            session.run_state().mode == RunningMode::Service,
            "Service mode missing: snapshot={:?}, lifecycle={:?}, state={:?}",
            session.snapshot(),
            session.lifecycle_snapshot(),
            session.run_state()
        );
        ensure!(session.snapshot().running == Some(true));
        ensure!(session.lifecycle_snapshot().phase == CoreLifecyclePhase::Stable);
        ensure!(session.run_state().service_usable());
        ensure!(pac_status(&pac).await?.starts_with("HTTP/1.1 200"));
        ensure!(client.version().await?.version == "integration-fixture");
        ensure!(zenclash_core::service::reserve_sidecar().await.is_err());

        let observer = Arc::new(ServiceSession::connect(&home).await?);
        let before = observer.status().await?;
        let pid = before
            .core_pid
            .context("native Start returned no running core")?;
        let owner_generation = before.active_generation;
        let applied = session.set_mode(&store, "direct").await?;
        ensure!(
            applied
                > initialized
                    .saved()
                    .context("saved receipt missing")?
                    .generation
        );
        ensure!(client.runtime_config().await?.mode == "direct");
        let after = observer.status().await?;
        ensure!(
            after.core_pid == Some(pid),
            "Stage/Reload unexpectedly restarted the core"
        );
        ensure!(after.active_generation == owner_generation);
        // Exercise actual native Stage -> core reload -> readback without a
        // physical adapter: this fixture implements only the controller API.
        session
            .apply(
                &store,
                EffectiveConfigIntent::Patch {
                    profile: profile.clone(),
                    patch: serde_json::json!({"tun": {"enable": true}}),
                    overrides: Vec::new(),
                },
            )
            .await?;
        let tun = client.runtime_config().await?.tun;
        ensure!(tun.enable && tun.auto_route && tun.auto_detect_interface);
        ensure!(tun.stack == "gvisor" && tun.dns_hijack == ["any:53"]);
        let saved: serde_yaml::Value =
            serde_yaml::from_slice(&std::fs::read(store.runtime_path())?)?;
        ensure!(saved["dns"]["enable"] == true);
        ensure!(saved["dns"]["fake-ip-range"] == "198.18.0.1/16");
        ensure!(observer.status().await?.core_pid == Some(pid));
        session
            .apply(
                &store,
                EffectiveConfigIntent::Patch {
                    profile: profile.clone(),
                    patch: serde_json::json!({"tun": {"enable": false}}),
                    overrides: Vec::new(),
                },
            )
            .await?;
        ensure!(!client.runtime_config().await?.tun.enable);
        ensure!(observer.status().await?.active_generation == owner_generation);
        ensure!(
            session.run_state().mode == RunningMode::Service,
            "Service mode missing: snapshot={:?}, lifecycle={:?}, state={:?}",
            session.snapshot(),
            session.lifecycle_snapshot(),
            session.run_state()
        );
        ensure!(pac.status() == Some(listener));
        ensure!(pac_status(&pac).await?.starts_with("HTTP/1.1 200"));

        ensure!(session.start_supervisor(&tokio::runtime::Handle::current()));
        replacement = Some(observer.clone());
        let replaced = observer
            .start(RuntimeBundle {
                yaml: "mode: global\ntun: {enable: false}\n".into(),
                assets: Vec::new(),
                remote_providers: Vec::new(),
                core_path: binary.to_string_lossy().into_owned(),
            })
            .await?;
        ensure!(Some(replaced.session.generation) != owner_generation);
        let replacement_pid = observer.status().await?.core_pid;
        ensure!(replacement_pid.is_some());
        ensure!(
            client.version().await.is_err(),
            "displaced controller unexpectedly retained authority"
        );
        tokio::time::timeout(Duration::from_secs(8), async {
            while session.run_state().mode != RunningMode::NotRunning {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .context("application did not retire the displaced Service mode")?;
        ensure!(session.lifecycle_snapshot().phase == CoreLifecyclePhase::Unknown);
        ensure!(pac_status(&pac).await?.starts_with("HTTP/1.1 503"));
        session.shutdown().await?;
        let surviving = observer.status().await?;
        ensure!(observer.owns_status(&surviving));
        ensure!(
            surviving.core_pid == replacement_pid,
            "old application shutdown stopped its replacement"
        );
        observer.stop().await?;
        ensure!(!observer.status().await?.is_active);
        drop(zenclash_core::service::reserve_sidecar().await?);
        Ok::<_, anyhow::Error>(())
    })
    .await;

    // Error-return assertions above keep cleanup reachable after failed acceptance.
    let original_cleanup = if let Some(session) = original {
        session.shutdown().await
    } else {
        Ok(())
    };
    let replacement_cleanup = if let Some(session) = replacement {
        session.stop().await
    } else {
        Ok(())
    };
    let _ = shutdown_tx.send(());
    let server_cleanup = server.await;
    original_cleanup.context("cleanup original application")?;
    replacement_cleanup.context("cleanup replacement owner")?;
    server_cleanup.context("join native IPC listener")??;
    std::fs::remove_dir_all(root)?;
    workflow
}
