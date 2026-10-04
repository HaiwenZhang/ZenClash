//! Real ordinary Local backup restoration; no privileged service or TUN is started.

use super::*;
use crate::{MihomoLaunchConfig, owned_core::OwnedCore};

struct Fixture {
    root: PathBuf,
    process: Option<Arc<MihomoProcess>>,
    client: Option<MihomoClient>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let current = self.client.as_ref().and_then(MihomoClient::owned_core);
        let processes = self.process.iter().cloned().chain(match current {
            Some(OwnedCore::Local(process)) => Some(process),
            _ => None,
        });
        for process in processes {
            if let Err(error) = process.stop() {
                eprintln!("real backup recovery child cleanup failed: {error}");
                return;
            }
        }
        if let Err(error) = std::fs::remove_dir_all(&self.root) {
            eprintln!("real backup recovery directory cleanup failed: {error}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires ZENCLASH_MIHOMO_BINARY; starts an isolated TUN-off real core"]
async fn ordinary_local_backup_recovers_held_provider_after_original_sources_are_deleted() {
    let binary = std::env::var_os("ZENCLASH_MIHOMO_BINARY")
        .expect("set ZENCLASH_MIHOMO_BINARY to a real ordinary Mihomo executable");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "zenclash-real-backup-recovery-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();
    let mut fixture = Fixture {
        root: root.clone(),
        process: None,
        client: None,
    };
    let home = root.join("home");
    std::fs::create_dir(&home).unwrap();
    let local_binary = home.join(if cfg!(windows) {
        "mihomo.exe"
    } else {
        "mihomo"
    });
    std::fs::copy(binary, &local_binary).unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    let source = home.join("source.yaml");
    let provider = home.join("provider.yaml");
    std::fs::write(
        &provider,
        "proxies:\n- name: held-node\n  type: http\n  server: 127.0.0.1\n  port: 1\n",
    )
    .unwrap();
    let payload = format!(
        "external-controller: {address}\nmixed-port: 0\nmode: rule\nlog-level: silent\ndns:\n  enable: false\ntun:\n  enable: false\nproxy-providers:\n  held:\n    type: file\n    path: provider.yaml\nproxy-groups:\n- name: recovered\n  type: select\n  use: [held]\nrules:\n- MATCH,DIRECT\n"
    );
    std::fs::write(&source, &payload).unwrap();
    let bundle = Arc::new(
        crate::ServiceRuntimeBundle::prepare(&payload, home.clone())
            .await
            .unwrap(),
    );
    let store = ControlledConfigStore::new(root.join("controlled"));
    let runtime = store.materialize(&source).unwrap();
    let launch = MihomoLaunchConfig::new(local_binary, runtime, home.clone()).unwrap();
    drop(reservation);
    let process = MihomoProcess::spawn(launch).unwrap();
    fixture.process = Some(process.clone());
    process
        .wait_until_ready(Duration::from_secs(10))
        .await
        .unwrap();
    let client = MihomoClient::from_process(process.clone()).unwrap();
    fixture.client = Some(client.clone());
    let session =
        CoreSession::open_with_config(CoreKind::Mihomo, client, Some(source.clone()), vec![])
            .unwrap();
    let mut snapshot = session.capture_restore_snapshot(&store).unwrap();
    // Backup preparation retains this exact bundle before replacing live data.
    snapshot.service_bundle = Some(bundle);
    let candidate = home.join("candidate.yaml");
    std::fs::write(
        &candidate,
        format!(
            "external-controller: {address}\nmixed-port: 0\nmode: rule\nlog-level: silent\ndns:\n  enable: false\ntun:\n  enable: false\nproxies:\n- name: candidate-node\n  type: http\n  server: 127.0.0.1\n  port: 1\nproxy-groups:\n- name: recovered\n  type: select\n  proxies: [candidate-node]\nrules:\n- MATCH,DIRECT\n"
        ),
    )
    .unwrap();
    session
        .apply(
            &store,
            EffectiveConfigIntent::ActivateProfile {
                profile: candidate,
                overrides: vec![],
            },
        )
        .await
        .unwrap();
    assert_eq!(
        session
            .client()
            .proxy_group_selection("recovered")
            .await
            .unwrap(),
        "candidate-node"
    );
    let previous_pid = process.snapshot().pid;
    let previous_cache = store.cached_runtime_payload().unwrap();
    std::fs::remove_file(&source).unwrap();
    std::fs::remove_file(&provider).unwrap();
    let admission = session.begin_backup_restore().await.unwrap();
    let restored = session
        .restore_backup_snapshot(&store, &snapshot, &admission)
        .await;
    let selected = session.client().proxy_group_selection("recovered").await;
    let restored_running = session.snapshot().running;
    let retained_pid = process.snapshot().pid;
    let retained_cache = store.cached_runtime_payload().unwrap();
    drop(admission);
    session.shutdown().await.unwrap();
    if restored.is_err() {
        assert_eq!(retained_pid, previous_pid);
        assert_eq!(retained_cache, previous_cache);
        assert_eq!(selected.as_ref().unwrap(), "candidate-node");
        assert_eq!(restored_running, Some(true));
    }
    assert!(
        restored.is_ok(),
        "held Local backup restoration failed: {restored:?}"
    );
    assert_eq!(selected.unwrap(), "held-node");
    assert_eq!(restored_running, Some(true));
    assert!(!source.exists());
    assert!(!provider.exists());
}
