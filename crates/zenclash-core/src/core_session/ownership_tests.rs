//! Real child ownership regressions; the HTTP responder is only a readiness fixture.

use std::{path::PathBuf, process::Command, sync::Arc, time::Duration};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    task::JoinHandle,
};

use crate::{
    CoreKind, CoreMaintenanceIntent, CoreSession, MihomoClient, MihomoEndpoint, MihomoLaunchConfig,
    MihomoProcess,
};

#[tokio::test]
async fn local_recovery_launch_tracks_actual_owner_and_clears_for_external_binding() {
    let directory =
        std::env::temp_dir().join(format!("zenclash-recovery-launch-{}", std::process::id()));
    let process = MihomoProcess::prepare_stopped(MihomoLaunchConfig {
        kind: CoreKind::Mihomo,
        binary: directory.join("ordinary-mihomo"),
        config_file: directory.join("original.yaml"),
        home_dir: directory.join("home"),
        endpoint: MihomoEndpoint::default(),
        controller_override: None,
    });
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(process.clone()).unwrap(),
    )
    .unwrap();
    let count = Arc::strong_count(&process);
    let launch = session.clone().local_recovery_launch().unwrap();
    assert_eq!(launch.binary, process.launch_config().binary);
    assert_eq!(Arc::strong_count(&process), count);
    session
        .switch_to_direct(MihomoEndpoint::default())
        .await
        .unwrap();
    assert!(session.local_recovery_launch().is_none());
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn maintenance_restart_failure_invalidates_an_observation_of_the_old_child() {
    let fixture = ChildFixture::new("maintenance-restart-failure").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let previous_version = session.generation();
    fixture.responder.abort();
    assert!(
        session
            .maintain_with_timeout(CoreMaintenanceIntent::Restart, Duration::from_millis(100))
            .await
            .is_err()
    );
    assert!(fixture.process.snapshot().pid.is_none());
    assert!(
        session.generation() > previous_version,
        "a modified child retained the old read version"
    );
    assert_eq!(
        session.lifecycle_snapshot().phase,
        crate::CoreLifecyclePhase::Unknown
    );
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn rejected_local_restart_preserves_running_lifecycle_and_pid() {
    let fixture = ChildFixture::new("rejected-restart-running").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let pid = fixture.process.snapshot().pid;
    std::fs::write(
        &fixture.process.launch_config().config_file,
        "tun: {enable: true}\n",
    )
    .unwrap();
    let result = session.maintain(CoreMaintenanceIntent::Restart).await;
    let lifecycle = session.lifecycle_snapshot();
    let current = fixture.process.snapshot().pid;
    let generation = session.generation();
    session.shutdown().await.unwrap();
    assert!(matches!(
        result,
        Err(crate::CoreSessionError::Process(
            crate::MihomoError::ServiceRequired
        ))
    ));
    assert_eq!(current, pid);
    assert_eq!(generation, 0);
    assert_eq!(lifecycle.phase, crate::CoreLifecyclePhase::Stable);
    assert!(!lifecycle.stop_requested);
}

#[tokio::test]
async fn rejected_local_restart_preserves_explicit_stop_intent() {
    let fixture = ChildFixture::new("rejected-restart-stopped").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    session.maintain(CoreMaintenanceIntent::Stop).await.unwrap();
    let generation = session.generation();
    std::fs::write(
        &fixture.process.launch_config().config_file,
        "tun: {enable: true}\n",
    )
    .unwrap();
    let result = session.maintain(CoreMaintenanceIntent::Restart).await;
    let lifecycle = session.lifecycle_snapshot();
    let current = session.generation();
    let running = fixture.process.snapshot().running;
    session.shutdown().await.unwrap();
    assert!(result.is_err());
    assert!(!running);
    assert_eq!(current, generation);
    assert_eq!(lifecycle.phase, crate::CoreLifecyclePhase::Stopped);
    assert!(lifecycle.stop_requested);
}

#[tokio::test]
async fn rejected_local_restart_preserves_network_resume_eligibility() {
    let fixture = ChildFixture::new("rejected-restart-network").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let capture = crate::TrafficCaptureSession::new(
        session.clone(),
        crate::ControlledConfigStore::new(fixture.directory.join("store")),
        None,
        None,
    );
    assert!(session.suspend_for_network(&capture).await.unwrap());
    let generation = session.generation();
    std::fs::write(
        &fixture.process.launch_config().config_file,
        "tun: {enable: true}\n",
    )
    .unwrap();
    let result = session.maintain(CoreMaintenanceIntent::Restart).await;
    let lifecycle = session.lifecycle_snapshot();
    let rejected_generation = session.generation();
    std::fs::write(
        &fixture.process.launch_config().config_file,
        "rules:\n  - MATCH,DIRECT\n",
    )
    .unwrap();
    let resumed = session.resume_after_network(&capture).await.unwrap();
    let running = fixture.process.snapshot().running;
    session.shutdown().await.unwrap();
    assert!(result.is_err());
    assert_eq!(rejected_generation, generation);
    assert_eq!(lifecycle.phase, crate::CoreLifecyclePhase::NetworkSuspended);
    assert!(!lifecycle.stop_requested);
    assert!(resumed, "rejection lost the accepted network suspension");
    assert!(running);
}

#[tokio::test]
async fn cancelled_maintenance_queue_does_not_stop_the_owned_child() {
    let fixture = ChildFixture::new("maintenance-queue-cancel").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let previous_pid = fixture.process.snapshot().pid;
    let held = session.transition.clone().lock_owned().await;
    let queued_session = session.clone();
    let queued =
        tokio::spawn(async move { queued_session.maintain(CoreMaintenanceIntent::Stop).await });
    tokio::task::yield_now().await;
    assert!(!queued.is_finished());
    queued.abort();
    assert!(queued.await.unwrap_err().is_cancelled());
    drop(held);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        fixture.process.snapshot().pid,
        previous_pid,
        "queued cancellation still admitted a Stop"
    );
    assert_eq!(session.generation(), 0);
    assert!(!session.lifecycle_snapshot().stop_requested);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_stop_intent_survives_failed_switch_and_is_replaced_only_by_admitted_runtime() {
    let first = ChildFixture::new("stop-intent-first").await;
    let second = ChildFixture::new("stop-intent-second").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(first.process.clone()).unwrap(),
    )
    .unwrap();
    let clone = session.clone();
    session.maintain(CoreMaintenanceIntent::Stop).await.unwrap();
    assert!(clone.lifecycle_snapshot().stop_requested);
    // Missing confirmation must remain distinct from the user's stopped intention.
    session.lifecycle.write().phase = crate::CoreLifecyclePhase::Unknown;
    assert!(clone.ensure_running_operations_allowed().is_err());
    let mut wrong_kind = second.process.launch_config().clone();
    wrong_kind.kind = CoreKind::Meow;
    let rejected = MihomoProcess::spawn(wrong_kind).unwrap();
    assert!(clone.switch_to_process(rejected.clone()).await.is_err());
    assert!(session.lifecycle_snapshot().stop_requested);
    assert!(first.process.snapshot().pid.is_none());
    rejected.stop_async().await.unwrap();

    clone
        .maintain(CoreMaintenanceIntent::Restart)
        .await
        .unwrap();
    assert!(!session.lifecycle_snapshot().stop_requested);
    assert!(first.process.snapshot().pid.is_some());
    session.maintain(CoreMaintenanceIntent::Stop).await.unwrap();
    clone
        .switch_to_process(second.process.clone())
        .await
        .unwrap();
    assert!(!session.lifecycle_snapshot().stop_requested);
    assert!(second.process.snapshot().pid.is_some());
    session.request_shutdown();
    assert!(clone.lifecycle_snapshot().stop_requested);
    assert!(clone.ensure_running_operations_allowed().is_err());
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelled_permission_queue_never_reaches_native_authorization() {
    let fixture = ChildFixture::new("grant-queue-cancel").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let effects = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let held = session.transition.clone().lock_owned().await;
    let queued_session = session.clone();
    let effects_for_grant = effects.clone();
    let queued = tokio::spawn(async move {
        queued_session
            .ensure_tun_permission_with(move |_| {
                effects_for_grant.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(true)
            })
            .await
    });
    tokio::task::yield_now().await;
    assert!(!queued.is_finished());
    queued.abort();
    assert!(queued.await.unwrap_err().is_cancelled());
    drop(held);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(effects.load(std::sync::atomic::Ordering::SeqCst), 0);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn permission_grant_does_not_restart_an_explicitly_stopped_runtime() {
    let fixture = ChildFixture::new("grant-stopped-intent").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    session.maintain(CoreMaintenanceIntent::Stop).await.unwrap();
    let version = session.generation();
    let client = session.client.pin_binding().unwrap();
    let lease = session.acquire_process_write_lease(&client).await.unwrap();
    // The helper is entered after authorization has succeeded. No native grant is invoked.
    session
        .restart_after_tun_grant(&fixture.process, &lease, Duration::from_secs(1))
        .await
        .unwrap();
    assert!(fixture.process.snapshot().pid.is_none());
    assert_eq!(session.generation(), version);
    assert_eq!(
        session.lifecycle_snapshot().phase,
        crate::CoreLifecyclePhase::Stopped
    );
    assert!(session.lifecycle_snapshot().stop_requested);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn permission_restart_cancelled_before_stop_preserves_the_current_read_version() {
    let fixture = ChildFixture::new("grant-restart-cancel-before-stop").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let client = session.client.pin_binding().unwrap();
    let lease = session.acquire_process_write_lease(&client).await.unwrap();
    let previous_pid = fixture.process.snapshot().pid;
    let previous_version = session.generation();
    session
        .shutdown_requested
        .store(true, std::sync::atomic::Ordering::Release);
    assert!(
        session
            .restart_after_tun_grant(&fixture.process, &lease, Duration::from_millis(100))
            .await
            .is_err()
    );
    assert_eq!(fixture.process.snapshot().pid, previous_pid);
    assert_eq!(session.generation(), previous_version);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn granted_permission_restart_failure_invalidates_the_old_runtime_version() {
    let fixture = ChildFixture::new("grant-restart-failure").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let old_version = session.generation();
    let client = session.client.pin_binding().unwrap();
    let lease = session.acquire_process_write_lease(&client).await.unwrap();
    fixture.responder.abort();
    // Authorization is already acknowledged at this boundary; no native grant is invoked.
    let result = session
        .restart_after_tun_grant(&fixture.process, &lease, Duration::from_millis(100))
        .await;
    assert!(result.is_err());
    assert!(fixture.process.snapshot().pid.is_none());
    assert!(
        session.generation() > old_version,
        "a stopped child retained the old read version"
    );
    assert_eq!(
        session.lifecycle_snapshot().phase,
        crate::CoreLifecyclePhase::Unknown
    );
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_stops_the_current_child_after_a_binding_switch() {
    let first = ChildFixture::new("shutdown-first").await;
    let second = ChildFixture::new("shutdown-second").await;
    let client = MihomoClient::from_process(first.process.clone()).unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
    session
        .switch_to_process(second.process.clone())
        .await
        .unwrap();

    session.shutdown().await.unwrap();

    assert!(first.process.snapshot().pid.is_none());
    assert!(
        second.process.snapshot().pid.is_none(),
        "shutdown leaked the current child"
    );
}

#[tokio::test]
async fn maintenance_stop_controls_the_current_child_after_a_binding_switch() {
    let first = ChildFixture::new("stop-first").await;
    let second = ChildFixture::new("stop-second").await;
    let client = MihomoClient::from_process(first.process.clone()).unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
    session
        .switch_to_process(second.process.clone())
        .await
        .unwrap();

    session.maintain(CoreMaintenanceIntent::Stop).await.unwrap();

    assert!(first.process.snapshot().pid.is_none());
    assert!(
        second.process.snapshot().pid.is_none(),
        "maintenance stopped the retired child"
    );
}

#[tokio::test]
async fn maintenance_restart_never_revives_the_retired_child_after_a_binding_switch() {
    let first = ChildFixture::new("restart-first").await;
    let second = ChildFixture::new("restart-second").await;
    let client = MihomoClient::from_process(first.process.clone()).unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
    session
        .switch_to_process(second.process.clone())
        .await
        .unwrap();
    let previous_pid = second.process.snapshot().pid;

    session
        .maintain(CoreMaintenanceIntent::Restart)
        .await
        .unwrap();

    assert!(
        first.process.snapshot().pid.is_none(),
        "maintenance revived the retired child"
    );
    let current_pid = second.process.snapshot().pid;
    assert!(current_pid.is_some());
    assert_ne!(
        current_pid, previous_pid,
        "maintenance did not restart the current child"
    );
}

#[tokio::test]
async fn session_observation_follows_the_current_child_after_a_binding_switch() {
    let first = ChildFixture::new("snapshot-first").await;
    let second = ChildFixture::new("snapshot-second").await;
    let client = MihomoClient::from_process(first.process.clone()).unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
    session
        .switch_to_process(second.process.clone())
        .await
        .unwrap();

    let snapshot = session.snapshot();

    assert!(snapshot.managed);
    assert!(
        snapshot.running == Some(true),
        "session observed the stopped retired child"
    );
    assert_eq!(
        session.managed_process_snapshot().unwrap().pid,
        second.process.snapshot().pid
    );
}

#[tokio::test]
async fn shutdown_closes_publication_for_all_session_clones() {
    let first = ChildFixture::new("admission-first").await;
    let second = ChildFixture::new("admission-second").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(first.process.clone()).unwrap(),
    )
    .unwrap();
    let clone = session.clone();
    session.request_shutdown();
    assert!(matches!(
        clone.switch_to_process(second.process.clone()).await,
        Err(crate::CoreSessionError::ShuttingDown)
    ));
    session.shutdown().await.unwrap();
    assert!(first.process.snapshot().pid.is_none());
    assert_eq!(
        session.runtime_descriptor().binary(),
        Some(first.process.launch_config().binary.as_path())
    );
}

#[tokio::test]
async fn current_owner_network_pause_and_resume_never_revive_retired_child() {
    let first = ChildFixture::new("network-first").await;
    let second = ChildFixture::new("network-second").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(first.process.clone()).unwrap(),
    )
    .unwrap();
    session
        .switch_to_process(second.process.clone())
        .await
        .unwrap();
    let capture = crate::TrafficCaptureSession::new(
        session.clone(),
        crate::ControlledConfigStore::new(second.directory.join("store")),
        None,
        None,
    );
    assert!(session.suspend_for_network(&capture).await.unwrap());
    assert!(second.process.snapshot().pid.is_none());
    assert!(session.resume_after_network(&capture).await.unwrap());
    assert!(second.process.snapshot().pid.is_some());
    assert!(first.process.snapshot().pid.is_none());
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn supervisor_recovers_only_the_current_owned_child() {
    let first = ChildFixture::new("supervisor-first").await;
    let second = ChildFixture::new("supervisor-second").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(first.process.clone()).unwrap(),
    )
    .unwrap();
    session
        .switch_to_process(second.process.clone())
        .await
        .unwrap();
    assert!(session.start_supervisor(&tokio::runtime::Handle::current()));
    second.process.stop_async().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while second.process.snapshot().pid.is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(first.process.snapshot().pid.is_none());
    session.shutdown().await.unwrap();
    assert!(second.process.snapshot().pid.is_none());
}

#[test]
fn shared_direct_builder_cannot_change_kind_or_generation() {
    let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();
    let clone = client.clone();
    let before = clone.runtime_descriptor();
    assert!(client.with_core_kind(CoreKind::Meow).is_err());
    let after = clone.runtime_descriptor();
    assert_eq!(after.kind(), CoreKind::Mihomo);
    assert_eq!(after.binding_generation(), before.binding_generation());
    assert!(CoreSession::open(CoreKind::Meow, clone).is_err());
}

#[test]
fn declared_external_meow_kind_is_fixed_before_publication() {
    let client = MihomoClient::new(MihomoEndpoint::default())
        .unwrap()
        .with_core_kind(CoreKind::Meow)
        .unwrap();
    let clone = client.clone();
    let session = CoreSession::open(CoreKind::Meow, client).unwrap();
    assert_eq!(session.kind(), CoreKind::Meow);
    assert_eq!(session.runtime_descriptor().kind(), CoreKind::Meow);
    assert_eq!(clone.runtime_descriptor().kind(), CoreKind::Meow);
    assert!(!session.is_managed());
    assert_eq!(session.snapshot().running, None);
    drop(session);
    // Unique again is not enough: a published binding stays immutable.
    assert!(clone.with_core_kind(CoreKind::Mihomo).is_err());
}

#[tokio::test]
async fn mismatched_owned_kind_is_rejected_without_touching_the_child() {
    let fixture = ChildFixture::new("wrong-kind").await;
    let pid = fixture.process.snapshot().pid;
    assert!(
        CoreSession::open(
            CoreKind::Meow,
            MihomoClient::from_process(fixture.process.clone()).unwrap()
        )
        .is_err()
    );
    assert_eq!(fixture.process.snapshot().pid, pid);
}

#[tokio::test]
async fn permission_observation_uses_current_binary_and_external_is_unsupported() {
    let first = ChildFixture::new("permission-first").await;
    let second = ChildFixture::new("permission-second").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(first.process.clone()).unwrap(),
    )
    .unwrap();
    session
        .switch_to_process(second.process.clone())
        .await
        .unwrap();
    let crate::Observation::Fresh {
        value: crate::CoreTunPermissionStatus::Local(status),
        ..
    } = session.tun_permission_status().await.unwrap()
    else {
        panic!("actual local authority unavailable");
    };
    assert_eq!(
        status.binary,
        std::fs::canonicalize(&second.process.launch_config().binary).unwrap()
    );
    assert_ne!(
        status.binary,
        std::fs::canonicalize(&first.process.launch_config().binary).unwrap()
    );
    session
        .switch_to_direct(MihomoEndpoint::default())
        .await
        .unwrap();
    assert!(matches!(
        session.tun_permission_status().await.unwrap(),
        crate::Observation::Failed {
            recovery: crate::RecoveryAction::Unsupported,
            ..
        }
    ));
    assert!(first.process.snapshot().pid.is_none());
    assert!(second.process.snapshot().pid.is_none());
    session.shutdown().await.unwrap();
}

pub(crate) struct ChildFixture {
    pub(crate) process: Arc<MihomoProcess>,
    directory: PathBuf,
    responder: JoinHandle<()>,
}

impl ChildFixture {
    pub(crate) fn responder_for_test_abort(&self) {
        self.responder.abort();
    }

    pub(crate) async fn new(label: &str) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "zenclash-current-child-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(directory.join("home")).unwrap();
        let source = directory.join("child.rs");
        std::fs::write(
            &source,
            r#"fn main() {
            if std::env::args().any(|arg| arg == "-t") { return; }
            std::thread::sleep(std::time::Duration::from_secs(30));
        }"#,
        )
        .unwrap();
        let binary = directory.join(if cfg!(windows) {
            "mihomo.exe"
        } else {
            "mihomo"
        });
        let compilation = Command::new("rustc")
            .arg("--edition=2024")
            .arg(source)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compilation.status.success(),
            "{}",
            String::from_utf8_lossy(&compilation.stderr)
        );
        let config = directory.join("profile.yaml");
        std::fs::write(&config, "rules:\n  - MATCH,DIRECT\n").unwrap();
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let responder = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = Vec::new();
                while request.len() < 4096 && !request.windows(4).any(|tail| tail == b"\r\n\r\n") {
                    let mut bytes = [0_u8; 1024];
                    let read = stream.read(&mut bytes).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&bytes[..read]);
                }
                let body = r#"{"meta":true,"version":"owned-child-fixture"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let process = MihomoProcess::spawn(MihomoLaunchConfig {
            kind: CoreKind::Mihomo,
            binary,
            config_file: config,
            home_dir: directory.join("home"),
            endpoint: MihomoEndpoint::new(format!("http://{address}"), ""),
            controller_override: None,
        })
        .unwrap();
        Self {
            process,
            directory,
            responder,
        }
    }
}

impl Drop for ChildFixture {
    fn drop(&mut self) {
        self.responder.abort();
        self.process.stop().unwrap();
        for attempt in 0..20 {
            match std::fs::remove_dir_all(&self.directory) {
                Ok(()) => return,
                Err(error) if attempt == 19 => panic!("child fixture cleanup failed: {error}"),
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }
}
