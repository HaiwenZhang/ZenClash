//! Automatic lifecycle behavior against an isolated real Mihomo, without changing host capture.

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use zenclash_core::{
    ControlledConfigStore, CoreKind, CoreLifecyclePhase, CoreMaintenanceIntent, CoreSession,
    MihomoClient, MihomoLaunchConfig, MihomoProcess, TrafficCaptureSession,
};

struct Fixture {
    root: PathBuf,
    source: PathBuf,
    store: ControlledConfigStore,
    process: Arc<MihomoProcess>,
    session: CoreSession,
    capture: TrafficCaptureSession,
}

impl Fixture {
    async fn start(name: &str) -> Self {
        let binary =
            std::env::var_os("ZENCLASH_MIHOMO_BINARY").expect("set ZENCLASH_MIHOMO_BINARY");
        let root = std::env::temp_dir().join(format!(
            "zenclash-real-automatic-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let source = root.join("source.yaml");
        std::fs::write(&source, format!("external-controller: {address}\nmixed-port: 0\nmode: rule\nlog-level: silent\ntun:\n  enable: false\nrules:\n  - MATCH,DIRECT\n")).unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        let runtime = store
            .materialize_with_overrides_for_core(&source, &[], CoreKind::Mihomo)
            .unwrap();
        let launch = MihomoLaunchConfig::new(binary, runtime, root.join("home")).unwrap();
        let process = MihomoProcess::spawn(launch).unwrap();
        process
            .wait_until_ready(Duration::from_secs(10))
            .await
            .unwrap();
        let client = MihomoClient::new(process.endpoint().clone())
            .unwrap()
            .with_config_validator(process.config_validator());
        let session = CoreSession::open(CoreKind::Mihomo, client, Some(process.clone()));
        let capture = TrafficCaptureSession::new(
            session.clone(),
            store.clone(),
            None,
            None,
            Some(source.clone()),
        );
        assert!(session.start_supervisor(&tokio::runtime::Handle::current()));
        Self {
            root,
            source,
            store,
            process,
            session,
            capture,
        }
    }

    async fn finish(self) {
        self.session.shutdown().await.unwrap();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires ZENCLASH_MIHOMO_BINARY; starts an isolated real core"]
async fn network_pause_does_not_trigger_crash_recovery_and_manual_stop_prevents_resume() {
    let fixture = Fixture::start("network").await;
    assert!(
        fixture
            .session
            .suspend_for_network(&fixture.capture)
            .await
            .unwrap()
    );
    tokio::time::sleep(Duration::from_millis(750)).await;
    assert!(!fixture.process.is_running());
    assert_eq!(
        fixture.session.lifecycle_snapshot().phase,
        CoreLifecyclePhase::NetworkSuspended
    );
    let runtime = fixture.store.runtime_path();
    let saved_runtime = std::fs::read(&runtime).unwrap();
    std::fs::write(&runtime, "rules: [").unwrap();
    assert!(
        fixture
            .session
            .resume_after_network(&fixture.capture)
            .await
            .is_err()
    );
    assert!(!fixture.process.is_running());
    std::fs::write(&runtime, saved_runtime).unwrap();
    assert!(
        fixture
            .session
            .resume_after_network(&fixture.capture)
            .await
            .unwrap()
    );
    assert!(fixture.process.is_running());
    fixture
        .session
        .maintain(CoreMaintenanceIntent::Stop)
        .await
        .unwrap();
    assert!(
        !fixture
            .session
            .resume_after_network(&fixture.capture)
            .await
            .unwrap()
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!fixture.process.is_running());
    fixture
        .session
        .maintain(CoreMaintenanceIntent::Restart)
        .await
        .unwrap();
    fixture
        .session
        .suspend_for_network(&fixture.capture)
        .await
        .unwrap();
    fixture.session.request_shutdown();
    assert!(
        fixture
            .session
            .resume_after_network(&fixture.capture)
            .await
            .is_err()
    );
    assert!(!fixture.process.is_running());
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires ZENCLASH_MIHOMO_BINARY; starts an isolated real core"]
async fn changed_source_restarts_once_and_rejected_source_preserves_the_live_process() {
    let fixture = Fixture::start("source").await;
    let before = fixture.process.snapshot().pid;
    let payload = std::fs::read_to_string(&fixture.source).unwrap();
    std::fs::write(
        &fixture.source,
        payload.replace("mode: rule", "mode: direct"),
    )
    .unwrap();
    let revision = fixture
        .store
        .pending_source_revision(CoreKind::Mihomo, &fixture.source, &[])
        .unwrap()
        .unwrap();
    assert!(
        fixture
            .session
            .restart_changed_source(
                &fixture.store,
                fixture.source.clone(),
                vec![],
                fixture.session.snapshot().generation,
                revision
            )
            .await
            .unwrap()
    );
    let after = fixture.process.snapshot().pid;
    assert_ne!(before, after);
    assert_eq!(
        fixture
            .session
            .client()
            .runtime_config()
            .await
            .unwrap()
            .mode,
        "direct"
    );
    assert_eq!(
        fixture
            .store
            .pending_source_revision(CoreKind::Mihomo, &fixture.source, &[])
            .unwrap(),
        None
    );
    std::fs::write(
        &fixture.source,
        payload.replace("MATCH,DIRECT", "NOT-A-RULE,DIRECT"),
    )
    .unwrap();
    let revision = fixture
        .store
        .pending_source_revision(CoreKind::Mihomo, &fixture.source, &[])
        .unwrap()
        .unwrap();
    assert!(
        fixture
            .session
            .restart_changed_source(
                &fixture.store,
                fixture.source.clone(),
                vec![],
                fixture.session.snapshot().generation,
                revision
            )
            .await
            .is_err()
    );
    assert_eq!(fixture.process.snapshot().pid, after);
    assert_eq!(
        fixture
            .session
            .client()
            .runtime_config()
            .await
            .unwrap()
            .mode,
        "direct"
    );
    fixture.finish().await;
}
