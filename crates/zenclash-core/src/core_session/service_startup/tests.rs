use super::*;
use crate::service::NativeHttpResponse;
use crate::service_runtime::FrozenRuntime;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use tokio::sync::Notify;
use zenclash_service::{
    RuntimeFileOutcome, RuntimeFileRequest, ServiceLifecycleState, ServiceStatusSnapshot,
    StageRuntimeOutcome,
};

#[derive(Default)]
struct Service {
    running: AtomicBool,
    committed: AtomicBool,
    lose_commit: AtomicBool,
    lose_status: AtomicBool,
    block_commit: AtomicBool,
    get_count: AtomicUsize,
    commit_entered: Notify,
    commit_continue: Notify,
    calls: parking_lot::Mutex<Vec<&'static str>>,
}

fn unknown() -> MihomoError {
    MihomoError::RuntimeOutcomeUnknown
}

impl RuntimeTransport for Service {
    async fn prepare_snapshot(
        &self,
        bundle: Arc<crate::ServiceRuntimeBundle>,
        home: PathBuf,
        core: Option<PathBuf>,
    ) -> crate::MihomoResult<FrozenRuntime> {
        self.calls.lock().push("prepare");
        let binary =
            core.unwrap_or_else(|| home.join(format!("mihomo{}", std::env::consts::EXE_SUFFIX)));
        std::fs::write(&binary, b"test core").unwrap();
        FrozenRuntime::prepare(bundle, home, Some(binary), false).await
    }
    async fn start(&self, _runtime: zenclash_service::RuntimeBundle) -> crate::MihomoResult<()> {
        assert!(!self.running.swap(true, Ordering::SeqCst));
        self.calls.lock().push("start");
        Ok(())
    }
    async fn stage_runtime(
        &self,
        _runtime: &zenclash_service::RuntimeBundle,
    ) -> crate::MihomoResult<StageRuntimeOutcome> {
        self.calls.lock().push("stage");
        Ok(StageRuntimeOutcome::Staged {
            config_path: "fixture-runtime.yaml".into(),
        })
    }
    async fn request(
        &self,
        method: &str,
        _path: &str,
        _body: Option<&serde_json::Value>,
        _secret: &str,
    ) -> crate::MihomoResult<NativeHttpResponse> {
        if method == "GET" {
            let call = self.get_count.fetch_add(1, Ordering::SeqCst) + 1;
            if call == 2 {
                // The second confirmation follows durable cache/source publication.
                self.calls.lock().push("confirm_saved");
                self.commit_entered.notify_one();
                if self.block_commit.load(Ordering::SeqCst) {
                    self.commit_continue.notified().await;
                }
                self.committed.store(true, Ordering::SeqCst);
                if self.lose_commit.load(Ordering::SeqCst) {
                    return Err(unknown());
                }
            }
        }
        Ok(NativeHttpResponse {
            status: if method == "GET" { 200 } else { 204 },
            body: b"{}".to_vec(),
        })
    }
    async fn status(&self) -> crate::MihomoResult<ServiceStatusSnapshot> {
        self.calls.lock().push("status");
        if self.lose_status.load(Ordering::SeqCst) {
            return Err(unknown());
        }
        let running = self.running.load(Ordering::SeqCst);
        Ok(ServiceStatusSnapshot {
            is_active: running,
            active_generation: running.then_some(1),
            core_pid: running.then_some(123),
            service_state: ServiceLifecycleState::Running,
            core_started_at: None,
            last_core_exit_reason: None,
            restart_count: 0,
            last_recovery_at: None,
            desired_core_should_be_running: running,
            desired_generation: 1,
            desired_updated_at: 0,
        })
    }
    fn owns_status(&self, status: &ServiceStatusSnapshot) -> bool {
        status.is_active && status.active_generation == Some(1)
    }
    fn active_generation(&self) -> Option<u64> {
        self.running.load(Ordering::SeqCst).then_some(1)
    }
    async fn stop(&self) -> crate::MihomoResult<()> {
        self.calls.lock().push("stop");
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }
    async fn read_runtime_file(
        &self,
        _request: &RuntimeFileRequest,
    ) -> crate::MihomoResult<RuntimeFileOutcome> {
        panic!("initial startup must not export provider caches")
    }
}

struct Fixture {
    root: PathBuf,
    profile: PathBuf,
    store: ControlledConfigStore,
    session: CoreSession,
    service: Arc<Service>,
    runtime: Arc<RuntimeSession<Service>>,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "zenclash-service-startup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let profile = root.join("profile.yaml");
        std::fs::write(
            &profile,
            "mode: rule\nproxies: []\nrules:\n  - MATCH,DIRECT\n",
        )
        .unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        let client = MihomoClient::new(crate::MihomoEndpoint::new("127.0.0.1:1", "")).unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
        let service = Arc::new(Service::default());
        let runtime = RuntimeSession::new(service.clone(), root.clone());
        Self {
            root,
            profile,
            store,
            session,
            service,
            runtime,
        }
    }

    async fn initialize(&self) -> Result<CoreInitializationOutcome, CoreSessionError> {
        let lease = self
            .store
            .acquire_write_lease_for_paths(vec![self.root.clone()])
            .await?;
        let store = self.store.with_write_lease(&lease);
        let committed = self.session.transition.clone().lock_owned().await;
        let session = self.session.clone();
        let runtime = self.runtime.clone();
        let next = CommittedConfig {
            profile: Some(self.profile.clone()),
            overrides: Vec::new(),
        };
        self.session
            .complete_service_initialization(async move {
                let _lease = lease;
                session
                    .initialize_service_admitted(&store, runtime, committed, next, async { Ok(()) })
                    .await
            })
            .await
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn startup_confirmation_failure_preserves_capture_and_network_intent() {
    let fixture = Fixture::new();
    fixture.service.lose_commit.store(true, Ordering::SeqCst);
    let outcome = fixture.initialize().await.unwrap();
    assert!(outcome.commit_pending());
    let capture = TrafficCaptureSession::new(
        fixture.session.clone(),
        fixture.store.clone(),
        None,
        Some(fixture.profile.clone()),
    );
    let previous_capture = capture.reconcile().await.unwrap().snapshot().clone();
    let previous = fixture.session.lifecycle_snapshot();
    let generation = fixture.session.generation();
    fixture.service.lose_status.store(true, Ordering::SeqCst);
    assert!(
        fixture
            .session
            .admit_network_suspension(fixture.runtime.confirm_finalizing_before_stop())
            .await
            .is_err()
    );
    assert_eq!(fixture.session.lifecycle_snapshot(), previous);
    assert_eq!(fixture.session.generation(), generation);
    assert!(!fixture.session.network_suspended.load(Ordering::SeqCst));
    let current_capture = capture.reconcile().await.unwrap().snapshot().clone();
    assert_eq!(
        current_capture.observed_plan,
        previous_capture.observed_plan
    );
    assert_eq!(
        current_capture.system_proxy.value(),
        previous_capture.system_proxy.value()
    );
    assert_eq!(current_capture.tun.value(), previous_capture.tun.value());
    assert_eq!(
        current_capture.core_available,
        previous_capture.core_available
    );
    assert_eq!(
        current_capture.system_proxy_port,
        previous_capture.system_proxy_port
    );
    assert_eq!(
        std::mem::discriminant(&current_capture.tun),
        std::mem::discriminant(&previous_capture.tun)
    );
    assert_eq!(
        std::mem::discriminant(&current_capture.system_proxy),
        std::mem::discriminant(&previous_capture.system_proxy)
    );
    assert!(!fixture.service.calls.lock().contains(&"stop"));
}

#[tokio::test]
async fn startup_commit_unknown_can_suspend_and_resume_its_saved_revision() {
    let fixture = Fixture::new();
    fixture.service.lose_commit.store(true, Ordering::SeqCst);
    let outcome = fixture.initialize().await.unwrap();
    assert!(outcome.saved().is_some() && outcome.commit_pending());
    fixture.runtime.stop_confirmed().await.unwrap();
    fixture
        .runtime
        .restart_accepted(&AtomicBool::new(false))
        .await
        .unwrap();
    assert!(fixture.service.running.load(Ordering::SeqCst));
    assert!(fixture.runtime.snapshot().is_ok());
    assert_eq!(
        fixture.session.generation(),
        outcome.saved().unwrap().generation
    );
}

#[tokio::test]
async fn startup_unconfirmed_commit_rejects_network_stop_but_can_release_on_exit() {
    let fixture = Fixture::new();
    fixture.service.lose_commit.store(true, Ordering::SeqCst);
    assert!(fixture.initialize().await.unwrap().commit_pending());
    fixture.service.lose_status.store(true, Ordering::SeqCst);
    assert!(fixture.runtime.stop_confirmed().await.is_err());
    assert!(!fixture.service.calls.lock().contains(&"stop"));
    assert!(fixture.service.running.load(Ordering::SeqCst));
    fixture.runtime.release_owned().await.unwrap();
    assert!(!fixture.service.running.load(Ordering::SeqCst));
    assert!(fixture.service.calls.lock().contains(&"stop"));
}

#[tokio::test]
async fn startup_saved_commit_unknown_retains_profile_generation_cache_and_receipt() {
    let fixture = Fixture::new();
    fixture.service.lose_commit.store(true, Ordering::SeqCst);
    let outcome = fixture.initialize().await.unwrap();
    assert!(outcome.saved().is_some());
    assert!(outcome.commit_pending());
    assert!(outcome.failure().is_some());
    let committed = fixture.session.committed_profile_snapshot();
    assert_eq!(committed.profile_path.as_ref(), Some(&fixture.profile));
    assert_eq!(committed.generation, outcome.saved().unwrap().generation);
    assert!(fixture.store.runtime_path().is_file());
    assert!(fixture.runtime.snapshot().is_err());
    fixture.runtime.reconcile().await.unwrap();
    assert!(
        fixture
            .runtime
            .snapshot()
            .unwrap()
            .yaml()
            .contains("mode: rule")
    );
    assert_eq!(
        fixture
            .service
            .calls
            .lock()
            .iter()
            .filter(|call| **call == "start")
            .count(),
        1
    );
}

#[tokio::test]
async fn startup_publishes_saved_source_before_waiting_for_commit_ack() {
    let fixture = Arc::new(Fixture::new());
    fixture.service.block_commit.store(true, Ordering::SeqCst);
    let owner = fixture.clone();
    let task = tokio::spawn(async move { owner.initialize().await });
    tokio::time::timeout(
        Duration::from_secs(3),
        fixture.service.commit_entered.notified(),
    )
    .await
    .unwrap();
    let snapshot = fixture.session.committed_profile_snapshot();
    fixture.service.commit_continue.notify_one();
    let outcome = task.await.unwrap().unwrap();
    assert_eq!(snapshot.profile_path.as_ref(), Some(&fixture.profile));
    assert_eq!(snapshot.generation, outcome.saved().unwrap().generation);
}

#[tokio::test]
async fn startup_unverified_start_keeps_owner_and_does_not_claim_saved_or_retry() {
    let fixture = Fixture::new();
    fixture.service.lose_status.store(true, Ordering::SeqCst);
    let outcome = fixture.initialize().await.unwrap();
    assert!(outcome.saved().is_none());
    assert!(!outcome.commit_pending());
    assert!(outcome.failure().is_some());
    assert!(
        fixture
            .session
            .committed_profile_snapshot()
            .profile_path
            .is_none()
    );
    assert!(!fixture.store.runtime_path().exists());
    assert!(fixture.service.running.load(Ordering::SeqCst));
    fixture.service.lose_status.store(false, Ordering::SeqCst);
    fixture.runtime.release_owned().await.unwrap();
    assert!(!fixture.service.running.load(Ordering::SeqCst));
    assert_eq!(
        fixture
            .service
            .calls
            .lock()
            .iter()
            .filter(|call| **call == "start")
            .count(),
        1
    );
}

#[tokio::test]
async fn startup_canceled_waiter_cannot_release_transition_before_completion() {
    let fixture = Arc::new(Fixture::new());
    fixture.service.block_commit.store(true, Ordering::SeqCst);
    let owner = fixture.clone();
    let waiter = tokio::spawn(async move { owner.initialize().await });
    tokio::time::timeout(
        Duration::from_secs(3),
        fixture.service.commit_entered.notified(),
    )
    .await
    .unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    let session = fixture.session.clone();
    let shutdown = tokio::spawn(async move { session.shutdown().await });
    tokio::task::yield_now().await;
    assert!(!shutdown.is_finished());
    fixture.service.commit_continue.notify_one();
    tokio::time::timeout(Duration::from_secs(3), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(fixture.service.committed.load(Ordering::SeqCst));
    assert_eq!(
        fixture
            .session
            .committed_profile_snapshot()
            .profile_path
            .as_ref(),
        Some(&fixture.profile)
    );
    assert!(fixture.store.runtime_path().is_file());
}

#[tokio::test]
async fn startup_shutdown_after_start_restores_cache_without_accepting_source() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.store.runtime_path().parent().unwrap()).unwrap();
    std::fs::write(fixture.store.runtime_path(), b"mode: direct\n").unwrap();
    let lease = fixture
        .store
        .acquire_write_lease_for_paths(vec![fixture.root.clone()])
        .await
        .unwrap();
    let store = fixture.store.with_write_lease(&lease);
    let committed = fixture.session.transition.clone().lock_owned().await;
    let outcome = fixture
        .session
        .initialize_service_admitted(
            &store,
            fixture.runtime.clone(),
            committed,
            CommittedConfig {
                profile: Some(fixture.profile.clone()),
                overrides: Vec::new(),
            },
            async {
                fixture.session.request_shutdown();
                Ok(())
            },
        )
        .await
        .unwrap();
    assert!(outcome.saved().is_none());
    assert!(!outcome.commit_pending());
    assert!(matches!(
        outcome.failure(),
        Some(CoreSessionError::ShuttingDown)
    ));
    assert!(!fixture.service.running.load(Ordering::SeqCst));
    assert_eq!(
        std::fs::read(fixture.store.runtime_path()).unwrap(),
        b"mode: direct\n"
    );
    assert!(
        fixture
            .session
            .committed_profile_snapshot()
            .profile_path
            .is_none()
    );
    assert!(!fixture.service.calls.lock().contains(&"confirm_saved"));
}

#[tokio::test]
async fn startup_public_entry_rejects_external_controller_before_service_mutation() {
    let fixture = Fixture::new();
    let error = fixture
        .session
        .initialize_service_runtime(&fixture.store, fixture.profile.clone(), Vec::new())
        .await
        .unwrap_err();
    assert!(matches!(error, CoreSessionError::ReleaseUnsupported { .. }));
    assert_eq!(fixture.session.generation(), 0);
    assert!(!fixture.store.runtime_path().exists());
}
