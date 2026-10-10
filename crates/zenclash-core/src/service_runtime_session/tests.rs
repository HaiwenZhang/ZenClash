//! Regressions use the real native operation schema and immutable application snapshots.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use zenclash_service::{ServiceLifecycleState, StageRejection};

#[derive(Default)]
struct MockNative {
    generation: AtomicU64,
    running: AtomicBool,
    start_refused: AtomicBool,
    http_refused: AtomicBool,
    fail_get: AtomicUsize,
    gets: AtomicUsize,
    lose_status: AtomicBool,
    restart_required: AtomicBool,
    cache_changes: AtomicBool,
    secret: parking_lot::Mutex<String>,
    staged: parking_lot::Mutex<Option<zenclash_service::RuntimeBundle>>,
    starts: parking_lot::Mutex<Vec<zenclash_service::RuntimeBundle>>,
    calls: parking_lot::Mutex<Vec<String>>,
    cache: parking_lot::Mutex<Option<Vec<u8>>>,
    core: PathBuf,
    root: PathBuf,
}

impl RuntimeTransport for MockNative {
    async fn prepare_snapshot(
        &self,
        bundle: Arc<ServiceRuntimeBundle>,
        home: PathBuf,
        core: Option<PathBuf>,
    ) -> MihomoResult<FrozenRuntime> {
        self.calls.lock().push("prepare".into());
        FrozenRuntime::prepare(
            bundle,
            home,
            Some(core.unwrap_or_else(|| self.core.clone())),
            false,
        )
        .await
    }
    async fn start(&self, runtime: zenclash_service::RuntimeBundle) -> MihomoResult<()> {
        self.calls.lock().push("start".into());
        if self.start_refused.load(Ordering::SeqCst) {
            return Err(MihomoError::Service(ServiceCallError::Rejected {
                code: zenclash_service::ServiceErrorCode::InvalidRuntimeAsset as u16,
                message: "source rejected".into(),
            }));
        }
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.running.store(true, Ordering::SeqCst);
        *self.secret.lock() = secret_of(&runtime.yaml);
        self.starts.lock().push(runtime);
        Ok(())
    }
    async fn stage_runtime(
        &self,
        runtime: &zenclash_service::RuntimeBundle,
    ) -> MihomoResult<StageRuntimeOutcome> {
        self.calls.lock().push("stage".into());
        if self.restart_required.load(Ordering::SeqCst) {
            return Ok(StageRuntimeOutcome::RestartRequired {
                reason: StageRejection::CoreRestarted,
            });
        }
        *self.staged.lock() = Some(runtime.clone());
        Ok(StageRuntimeOutcome::Staged {
            config_path: self.root.join("staged.yaml").to_string_lossy().into_owned(),
        })
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        _body: Option<&serde_json::Value>,
        secret: &str,
    ) -> MihomoResult<NativeHttpResponse> {
        self.calls.lock().push(format!("{method} {path}"));
        assert_eq!(
            secret,
            *self.secret.lock(),
            "authenticate against the currently running secret"
        );
        if method == "GET" {
            let call = self.gets.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_get.load(Ordering::SeqCst) == call {
                return Err(unknown());
            }
        } else if self.http_refused.load(Ordering::SeqCst) {
            return Ok(NativeHttpResponse {
                status: 400,
                body: br#"{"message":"invalid config"}"#.to_vec(),
            });
        } else if let Some(staged) = self.staged.lock().as_ref() {
            *self.secret.lock() = secret_of(&staged.yaml);
        }
        Ok(NativeHttpResponse {
            status: if method == "GET" { 200 } else { 204 },
            body: b"{}".to_vec(),
        })
    }
    async fn stop(&self) -> MihomoResult<()> {
        self.calls.lock().push("stop".into());
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }
    async fn status(&self) -> MihomoResult<ServiceStatusSnapshot> {
        self.calls.lock().push("status".into());
        if self.lose_status.load(Ordering::SeqCst) {
            return Err(unknown());
        }
        let running = self.running.load(Ordering::SeqCst);
        Ok(ServiceStatusSnapshot {
            is_active: running,
            active_generation: running.then(|| self.generation.load(Ordering::SeqCst)),
            service_state: if running {
                ServiceLifecycleState::Running
            } else {
                ServiceLifecycleState::Starting
            },
            core_pid: running.then_some(123),
            core_started_at: None,
            last_core_exit_reason: None,
            restart_count: 0,
            last_recovery_at: None,
            desired_core_should_be_running: running,
            desired_generation: self.generation.load(Ordering::SeqCst),
            desired_updated_at: 0,
        })
    }
    fn owns_status(&self, status: &ServiceStatusSnapshot) -> bool {
        status.is_active && status.active_generation == self.active_generation()
    }
    fn active_generation(&self) -> Option<u64> {
        self.running
            .load(Ordering::SeqCst)
            .then(|| self.generation.load(Ordering::SeqCst))
    }
    async fn read_runtime_file(
        &self,
        request: &RuntimeFileRequest,
    ) -> MihomoResult<RuntimeFileOutcome> {
        self.calls.lock().push(format!("read {}", request.offset));
        assert!(
            self.running.load(Ordering::SeqCst),
            "read before Stop retires the proof"
        );
        let cache = self.cache.lock();
        let Some(bytes) = cache.as_ref() else {
            return Ok(RuntimeFileOutcome::Absent);
        };
        let offset = request.offset as usize;
        let chunk = &bytes[offset..(offset + 2).min(bytes.len())];
        let hex = chunk.iter().map(|byte| format!("{byte:02x}")).collect();
        let len =
            bytes.len() as u64 + u64::from(offset > 0 && self.cache_changes.load(Ordering::SeqCst));
        Ok(RuntimeFileOutcome::Chunk {
            hex,
            len,
            mtime_ns: None,
        })
    }
}

fn secret_of(yaml: &str) -> String {
    serde_yaml::from_str::<serde_yaml::Value>(yaml)
        .unwrap()
        .get("secret")
        .and_then(serde_yaml::Value::as_str)
        .unwrap_or("")
        .to_owned()
}

struct Fixture {
    root: PathBuf,
    native: Arc<MockNative>,
    runtime: Arc<RuntimeSession<MockNative>>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "zenclash-native-runtime-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let core = root.join(format!("mihomo{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&core, b"fixture core").unwrap();
        let native = Arc::new(MockNative {
            root: root.clone(),
            core,
            ..MockNative::default()
        });
        let runtime = RuntimeSession::new(native.clone(), root.clone());
        Self {
            root,
            native,
            runtime,
        }
    }
    async fn accept(&self, yaml: &str) {
        self.runtime
            .prepare(yaml)
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn preparation_freezes_provider_bytes_without_native_effects_or_later_source_reads() {
    let fixture = Fixture::new();
    let source = fixture.root.join("provider.yaml");
    std::fs::write(&source, b"proxies: []\n").unwrap();
    let prepared = fixture
        .runtime
        .prepare("proxy-providers:\n  p:\n    type: file\n    path: provider.yaml\n")
        .await
        .unwrap();
    assert_eq!(*fixture.native.calls.lock(), ["prepare"]);
    std::fs::remove_file(source).unwrap();
    let applied = prepared.apply(true).await.unwrap();
    {
        let starts = fixture.native.starts.lock();
        assert_eq!(starts.len(), 1);
        assert_eq!(
            std::fs::read(&starts[0].assets[0].source).unwrap(),
            b"proxies: []\n"
        );
    }
    applied.commit().await.unwrap();
}

#[tokio::test]
async fn a_definite_http_refusal_after_staging_is_unknown_and_rolls_back_immutable_accepted_config()
{
    let fixture = Fixture::new();
    fixture.accept("mode: rule\n").await;
    fixture.native.http_refused.store(true, Ordering::SeqCst);
    let failure = fixture
        .runtime
        .prepare("mode: direct\n")
        .await
        .unwrap()
        .apply(true)
        .await
        .err()
        .unwrap();
    assert!(failure.mutation_result_unknown());
    assert!(fixture.runtime.snapshot().is_err());
    assert!(fixture.runtime.reconcile().await.is_err());
    fixture.native.http_refused.store(false, Ordering::SeqCst);
    fixture.runtime.restore_active().await.unwrap();
    assert!(
        fixture
            .runtime
            .snapshot()
            .unwrap()
            .yaml()
            .contains("mode: rule")
    );
    let starts = fixture.native.starts.lock();
    assert_eq!(starts.len(), 2);
    assert!(starts.last().unwrap().yaml.contains("mode: rule"));
}

#[tokio::test]
async fn secret_reload_authenticates_with_old_secret_then_confirms_with_new_secret() {
    let fixture = Fixture::new();
    fixture.accept("secret: old\n").await;
    fixture.accept("secret: new\n").await;
    assert_eq!(fixture.runtime.controller_secret(), "new");
    assert_eq!(fixture.native.starts.lock().len(), 1);
}

#[tokio::test]
async fn an_abandoned_prepared_candidate_has_no_native_effect_and_can_be_discarded() {
    let fixture = Fixture::new();
    fixture.accept("mode: rule\n").await;
    drop(fixture.runtime.prepare("mode: direct\n").await.unwrap());
    fixture.runtime.reconcile().await.unwrap();
    assert!(
        fixture
            .runtime
            .snapshot()
            .unwrap()
            .yaml()
            .contains("mode: rule")
    );
    assert_eq!(fixture.native.starts.lock().len(), 1);
}

#[tokio::test]
async fn an_abandoned_applied_candidate_requires_explicit_rollback_before_new_preparation() {
    let fixture = Fixture::new();
    fixture.accept("mode: rule\n").await;
    drop(
        fixture
            .runtime
            .prepare("mode: direct\n")
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap(),
    );
    assert!(fixture.runtime.prepare("mode: global\n").await.is_err());
    fixture.runtime.restore_active().await.unwrap();
    assert!(
        fixture
            .runtime
            .snapshot()
            .unwrap()
            .yaml()
            .contains("mode: rule")
    );
}

#[tokio::test]
async fn saved_candidate_survives_lost_confirmation_and_cannot_be_rolled_back() {
    let fixture = Fixture::new();
    fixture.accept("mode: rule\n").await;
    let applied = fixture
        .runtime
        .prepare("mode: direct\n")
        .await
        .unwrap()
        .apply(true)
        .await
        .unwrap();
    fixture.native.fail_get.store(
        fixture.native.gets.load(Ordering::SeqCst) + 1,
        Ordering::SeqCst,
    );
    assert!(applied.commit().await.is_err());
    assert!(fixture.runtime.restore_active().await.is_err());
    assert!(fixture.runtime.snapshot().is_err());
    fixture.runtime.reconcile().await.unwrap();
    assert!(
        fixture
            .runtime
            .snapshot()
            .unwrap()
            .yaml()
            .contains("mode: direct")
    );
    assert_eq!(fixture.native.starts.lock().len(), 1);
}

#[tokio::test]
async fn restart_fallback_that_stopped_the_old_core_cannot_report_a_definite_no_effect_refusal() {
    let fixture = Fixture::new();
    fixture.accept("mode: rule\n").await;
    fixture
        .native
        .restart_required
        .store(true, Ordering::SeqCst);
    fixture.native.start_refused.store(true, Ordering::SeqCst);
    let error = fixture
        .runtime
        .prepare("mode: direct\n")
        .await
        .unwrap()
        .apply(true)
        .await
        .err()
        .unwrap();
    assert!(error.mutation_result_unknown());
    assert!(!fixture.native.running.load(Ordering::SeqCst));
}

#[tokio::test]
async fn export_reads_complete_provider_cache_before_native_stop_retires_proof() {
    let fixture = Fixture::new();
    fixture.accept("proxy-providers:\n  p:\n    type: http\n    url: https://example.invalid/sub\n    path: cache.yaml\n").await;
    *fixture.native.cache.lock() = Some(b"cache-bytes".to_vec());
    let bundle = fixture.runtime.stop_and_export().await.unwrap();
    let providers = bundle.cache_providers().unwrap();
    let frozen = FrozenRuntime::prepare(
        bundle,
        fixture.root.clone(),
        Some(fixture.native.core.clone()),
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read(frozen.root().join(&providers[0].path)).unwrap(),
        b"cache-bytes"
    );
    let calls = fixture.native.calls.lock();
    let stopped = calls.iter().position(|call| call == "stop").unwrap();
    assert!(
        calls[..stopped]
            .iter()
            .any(|call| call.starts_with("read "))
    );
    assert!(
        !calls[stopped..]
            .iter()
            .any(|call| call.starts_with("read "))
    );
}

#[tokio::test]
async fn changing_cache_metadata_rejects_export_without_stopping_the_native_owner() {
    let fixture = Fixture::new();
    fixture.accept("proxy-providers:\n  p:\n    type: http\n    url: https://example.invalid/sub\n    path: cache.yaml\n").await;
    *fixture.native.cache.lock() = Some(b"cache-bytes".to_vec());
    fixture.native.cache_changes.store(true, Ordering::SeqCst);
    assert!(fixture.runtime.stop_and_export().await.is_err());
    assert!(
        !fixture
            .native
            .calls
            .lock()
            .iter()
            .any(|call| call == "stop")
    );
    assert!(fixture.native.running.load(Ordering::SeqCst));
}
