//! Ordinary Mihomo consumption of held recovery resources; no service or TUN is started.

use std::{path::PathBuf, sync::Arc, time::Duration};

use zenclash_core::{
    ControlledConfigStore, MihomoClient, MihomoLaunchConfig, MihomoProcess, ServiceRuntimeBundle,
};

struct RecoveryFixture {
    root: PathBuf,
    process: Option<Arc<MihomoProcess>>,
}

impl Drop for RecoveryFixture {
    fn drop(&mut self) {
        if let Some(process) = &self.process
            && let Err(error) = process.stop()
        {
            eprintln!("real recovery child cleanup failed: {error}");
            return;
        }
        if let Err(error) = std::fs::remove_dir_all(&self.root) {
            eprintln!("real recovery directory cleanup failed: {error}");
        }
    }
}

#[derive(Clone, Copy)]
enum ProviderInput {
    Held,
    InvalidGroup,
    OutsideSlot,
}

async fn recover_with_real_mihomo(input: ProviderInput) {
    let source_binary = std::env::var_os("ZENCLASH_MIHOMO_BINARY")
        .expect("set ZENCLASH_MIHOMO_BINARY to an ordinary real Mihomo executable");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "zenclash-real-local-recovery-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();
    let mut fixture = RecoveryFixture {
        root: root.clone(),
        process: None,
    };
    let home = root.join("home");
    std::fs::create_dir(&home).unwrap();
    let binary = home.join(if cfg!(windows) {
        "mihomo.exe"
    } else {
        "mihomo"
    });
    std::fs::copy(source_binary, &binary).unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    let source = home.join("source.yaml");
    let provider = home.join("provider.yaml");
    let geoip = home.join("GeoIP.dat");
    let geosite = home.join("geosite.dat");
    std::fs::write(&geoip, b"held geoip bytes").unwrap();
    std::fs::write(&geosite, b"held geosite bytes").unwrap();
    std::fs::write(
        &provider,
        "proxies:\n- name: held-node\n  type: http\n  server: 127.0.0.1\n  port: 1\n",
    )
    .unwrap();
    let payload = format!(
        "external-controller: {address}\nmixed-port: 0\nmode: rule\nlog-level: silent\ndns:\n  enable: false\ntun:\n  enable: true\nproxy-providers:\n  held:\n    type: file\n    path: provider.yaml\nproxy-groups:\n- name: recovered\n  type: select\n  use: [held]\nrules:\n- MATCH,DIRECT\n"
    );
    std::fs::write(&source, &payload).unwrap();
    let bundle = Arc::new(
        ServiceRuntimeBundle::prepare(&payload, home.clone())
            .await
            .unwrap(),
    );
    std::fs::write(&geoip, b"previous ordinary geoip bytes").unwrap();
    std::fs::remove_file(&geosite).unwrap();
    std::fs::remove_file(&source).unwrap();
    std::fs::remove_file(&provider).unwrap();
    let store = ControlledConfigStore::new(root.join("controlled"));
    let config = bundle
        .materialize_local_runtime(&store, None)
        .await
        .unwrap();
    let mut generated: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(generated["tun"]["enable"].as_bool(), Some(false));
    if matches!(input, ProviderInput::InvalidGroup) {
        generated["proxy-groups"][0]["type"] = serde_yaml::Value::from("zenclash-invalid");
        std::fs::write(&config, serde_yaml::to_string(&generated).unwrap()).unwrap();
    }
    if matches!(input, ProviderInput::OutsideSlot) {
        let outside = root.join("unapproved-provider.yaml");
        let held = generated["proxy-providers"]["held"]["path"]
            .as_str()
            .unwrap();
        std::fs::copy(held, &outside).unwrap();
        generated["proxy-providers"]["held"]["path"] =
            serde_yaml::Value::from(outside.to_str().unwrap());
        std::fs::write(&config, serde_yaml::to_string(&generated).unwrap()).unwrap();
    }
    let launch = MihomoLaunchConfig::new(binary, config, home.clone()).unwrap();
    let process = MihomoProcess::prepare_stopped(launch);
    fixture.process = Some(process.clone());
    drop(reservation);
    let restart = process.clone();
    let activated_home = home.clone();
    let result = bundle
        .with_local_geodata(&store, home.clone(), move |recovery| async move {
            assert_eq!(
                std::fs::read(activated_home.join("GeoIP.dat")).unwrap(),
                b"held geoip bytes"
            );
            assert_eq!(
                std::fs::read(activated_home.join("geosite.dat")).unwrap(),
                b"held geosite bytes"
            );
            let result = recovery
                .restart_local_process(&restart, Duration::from_secs(10), None)
                .await;
            if result.is_err() {
                restart.stop_async().await?;
            }
            result
        })
        .await;
    if !matches!(input, ProviderInput::Held) {
        let error = result.unwrap_err().to_string();
        assert!(error.contains("重启前配置预检失败"), "{error}");
        assert!(
            error.contains(if matches!(input, ProviderInput::InvalidGroup) {
                "zenclash-invalid"
            } else {
                "path is not subpath of home directory or SAFE_PATHS"
            }),
            "{error}"
        );
        assert!(!process.snapshot().running);
        assert!(process.snapshot().pid.is_none());
        assert_eq!(
            std::fs::read(&geoip).unwrap(),
            b"previous ordinary geoip bytes"
        );
        assert!(!geosite.exists());
    } else {
        result.unwrap();
        assert_eq!(std::fs::read(&geoip).unwrap(), b"held geoip bytes");
        assert_eq!(std::fs::read(&geosite).unwrap(), b"held geosite bytes");
        let client = MihomoClient::from_process(process.clone()).unwrap();
        let selected = client.proxy_group_selection("recovered").await;
        let before = process.snapshot().pid;
        let restarted = process.restart_and_wait(Duration::from_secs(10)).await;
        let after = process.snapshot().pid;
        let selected_after_restart = client.proxy_group_selection("recovered").await;
        let alternate = bundle
            .materialize_local_runtime(&store, Some(process.launch_config().config_file.clone()))
            .await
            .unwrap();
        let alternate_yaml: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&alternate).unwrap()).unwrap();
        let alternate_provider = alternate_yaml["proxy-providers"]["held"]["path"]
            .as_str()
            .unwrap();
        let bytes = std::fs::read_to_string(alternate_provider).unwrap();
        std::fs::write(alternate_provider, bytes.replace("held-node", "held-next")).unwrap();
        let reloaded = client.reload_config(&alternate, true).await;
        let selected_after_reload = client.proxy_group_selection("recovered").await;
        let after_reload = process.snapshot().pid;
        process.stop_async().await.unwrap();
        assert_eq!(selected.unwrap(), "held-node");
        restarted.unwrap();
        assert!(before.is_some() && after.is_some() && before != after);
        assert_eq!(selected_after_restart.unwrap(), "held-node");
        reloaded.unwrap();
        assert_eq!(after_reload, after);
        assert_eq!(selected_after_reload.unwrap(), "held-next");
        assert_eq!(process.launch_config().home_dir, home);
        assert!(!process.snapshot().running);
    }
    assert!(!source.exists());
    assert!(!provider.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires ZENCLASH_MIHOMO_BINARY; starts an isolated TUN-off real core"]
async fn held_recovery_resources_start_real_mihomo_after_original_sources_are_deleted() {
    recover_with_real_mihomo(ProviderInput::Held).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires ZENCLASH_MIHOMO_BINARY; validates with an isolated real core"]
async fn invalid_generated_recovery_config_is_rejected_without_a_live_child() {
    recover_with_real_mihomo(ProviderInput::InvalidGroup).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires ZENCLASH_MIHOMO_BINARY; validates with an isolated real core"]
async fn recovery_does_not_allow_provider_paths_outside_its_slot_and_original_home() {
    recover_with_real_mihomo(ProviderInput::OutsideSlot).await;
}
