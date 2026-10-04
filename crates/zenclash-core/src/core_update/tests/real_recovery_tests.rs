//! Real ordinary recovery-owner upgrades; no privileged service or TUN is started.

use super::*;
use crate::{
    ControlledConfigStore, MihomoClient, MihomoLaunchConfig, MihomoProcess, ServiceRuntimeBundle,
};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

struct Fixture {
    root: PathBuf,
    process: Option<Arc<MihomoProcess>>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(process) = &self.process
            && let Err(error) = process.stop()
        {
            eprintln!("real recovery update cleanup failed: {error}");
            return;
        }
        if let Err(error) = std::fs::remove_dir_all(&self.root) {
            eprintln!("real recovery update directory cleanup failed: {error}");
        }
    }
}

async fn update_recovery_owner(reject_version: bool) {
    let binary = std::env::var_os("ZENCLASH_MIHOMO_BINARY")
        .expect("set ZENCLASH_MIHOMO_BINARY to a real ordinary Mihomo executable");
    let root = unique_directory("real-recovery");
    std::fs::create_dir(&root).unwrap();
    let mut fixture = Fixture {
        root: root.clone(),
        process: None,
    };
    let home = root.join("home");
    std::fs::create_dir(&home).unwrap();
    let target = home.join(if cfg!(windows) {
        "mihomo.exe"
    } else {
        "mihomo"
    });
    std::fs::copy(binary, &target).unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    let provider = home.join("provider.yaml");
    std::fs::write(
        &provider,
        "proxies:\n- name: held-node\n  type: http\n  server: 127.0.0.1\n  port: 1\n",
    )
    .unwrap();
    let payload = format!(
        "external-controller: {address}\nmixed-port: 0\nmode: rule\nlog-level: silent\ndns:\n  enable: false\ntun:\n  enable: true\nproxy-providers:\n  held:\n    type: file\n    path: provider.yaml\nproxy-groups:\n- name: recovered\n  type: select\n  use: [held]\nrules:\n- MATCH,DIRECT\n"
    );
    let bundle = Arc::new(
        ServiceRuntimeBundle::prepare(&payload, home.clone())
            .await
            .unwrap(),
    );
    std::fs::remove_file(&provider).unwrap();
    let store = ControlledConfigStore::new(root.join("controlled"));
    let config = bundle
        .materialize_local_runtime(&store, None)
        .await
        .unwrap();
    let process = MihomoProcess::prepare_stopped(
        MihomoLaunchConfig::new(target.clone(), config, home.clone()).unwrap(),
    );
    fixture.process = Some(process.clone());
    drop(reservation);
    let started = process.clone();
    bundle
        .with_local_geodata(&store, home.clone(), move |recovery| async move {
            let result = recovery
                .restart_local_process(&started, Duration::from_secs(10), None)
                .await;
            if result.is_err() {
                started.stop_async().await?;
            }
            result
        })
        .await
        .unwrap();
    let client = MihomoClient::from_process(process.clone()).unwrap();
    assert_eq!(
        client.proxy_group_selection("recovered").await.unwrap(),
        "held-node"
    );
    let version = client.version().await.unwrap();
    let before = process.snapshot().pid;
    let staging = sibling_path(&target, "real-candidate").unwrap();
    std::fs::copy(&target, &staging).unwrap();
    let prepared = PreparedCoreUpdate {
        staging: Some(staging.clone()),
        target: target.clone(),
        tag: if reject_version {
            "v0.0.0-zenclash-test".into()
        } else {
            version.version.clone()
        },
    };
    let lease = store
        .acquire_write_lease_for_paths(process.write_scopes())
        .await
        .unwrap();
    let service = MihomoReleaseService::with_base("http://127.0.0.1/", true).unwrap();
    let installed = service
        .install_prepared(
            prepared,
            process.clone(),
            client.clone(),
            Arc::new(AtomicBool::new(false)),
            &lease,
        )
        .await;
    let after = process.snapshot().pid;
    let selected = client.proxy_group_selection("recovered").await;
    let final_version = client.version().await;
    process.stop_async().await.unwrap();
    if reject_version {
        let error = installed
            .err()
            .expect("candidate version must be rejected")
            .to_string();
        assert!(error.contains("/version"), "{error}");
        assert!(error.contains("旧内核"), "{error}");
    } else {
        let installed =
            installed.unwrap_or_else(|error| panic!("recovery-owner update failed: {error}"));
        assert!(installed.cleanup_error.is_none());
        assert_eq!(installed.version.version, version.version);
    }
    assert!(before.is_some() && after.is_some() && before != after);
    assert_eq!(selected.unwrap(), "held-node");
    assert_eq!(final_version.unwrap().version, version.version);
    assert_eq!(process.launch_config().home_dir, home);
    assert!(!provider.exists());
    assert!(!staging.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires ZENCLASH_MIHOMO_BINARY; upgrades an isolated TUN-off real core"]
async fn real_recovery_owner_upgrade_preserves_held_provider_and_original_home() {
    update_recovery_owner(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires ZENCLASH_MIHOMO_BINARY; rolls back an isolated TUN-off real core"]
async fn real_recovery_owner_version_rejection_rolls_back_with_held_provider() {
    update_recovery_owner(true).await;
}
