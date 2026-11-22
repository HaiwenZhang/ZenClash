use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use zenclash_app::{
    EdgeStableObservation, OutboundMode, OutboundModeCoordinator, QuitState, SafeMode,
    ShutdownCoordinator, ShutdownOutcome, SourceRevisionKey, SourceScanner, StableObservation,
    StartupCancellation,
    bootstrap::{is_startup_cancelled, startup_cancelled_error},
    safe_mode::SAFE_MODE_CLI_FLAG,
};
use zenclash_core::{
    CaptureOutcome, ControlledConfigStore, CoreKind, CoreSession, CoreSessionError, MihomoClient,
    MihomoEndpoint, MihomoError, NetworkReachability, ProfileStore, TrafficCaptureError,
    TrafficCaptureSession, YamlOverrideStore,
};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "zenclash-app-integration-{label}-{}-{nanos}-{seq}",
            std::process::id(),
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn safe_mode_policy_blocks_host_side_effects_and_isolates_data_root() {
    let base = Path::new("/var/lib/zenclash");

    let disabled = SafeMode::disabled();
    assert!(!disabled.is_enabled());
    assert!(disabled.allows_privileged_service());
    assert!(disabled.allows_autostart());
    assert!(disabled.allows_system_proxy());
    assert!(disabled.allows_tun());
    assert!(disabled.allows_system_dns());
    assert!(disabled.allows_default_ports());
    assert_eq!(disabled.data_root(base), base);
    assert_eq!(
        disabled.instance_lock_path(base),
        base.join("instance.lock")
    );
    assert_eq!(
        disabled
            .guard_host_side_effect("apply_proxy", || Ok::<_, String>("applied"))
            .unwrap(),
        Some("applied")
    );

    let enabled = SafeMode::enabled();
    assert!(enabled.is_enabled());
    assert!(!enabled.allows_privileged_service());
    assert!(!enabled.allows_autostart());
    assert!(!enabled.allows_system_proxy());
    assert!(!enabled.allows_tun());
    assert!(!enabled.allows_system_dns());
    assert!(!enabled.allows_default_ports());
    assert_eq!(enabled.data_root(base), base.join("safe-mode"));
    assert_eq!(
        enabled.instance_lock_path(base),
        base.join("safe-mode").join("instance.lock")
    );
    assert_eq!(
        enabled
            .guard_host_side_effect("apply_proxy", || Ok::<_, String>("should-skip"))
            .unwrap(),
        None
    );

    assert!(SafeMode::from_inputs([SAFE_MODE_CLI_FLAG], None).is_enabled());
    assert!(
        SafeMode::from_inputs(std::iter::empty::<&str>(), Some(OsStr::new("true"))).is_enabled()
    );
    assert!(!SafeMode::from_inputs(std::iter::empty::<&str>(), Some(OsStr::new("0"))).is_enabled());
}

#[tokio::test]
async fn outbound_mode_parsing_serialization_and_coordinator_deduplication() {
    assert_eq!(OutboundMode::default(), OutboundMode::Rule);
    for (mode, api, code) in [
        (OutboundMode::Rule, "rule", "RULE"),
        (OutboundMode::Global, "global", "GLOBAL"),
        (OutboundMode::Direct, "direct", "DIRECT"),
    ] {
        assert_eq!(mode.api_value(), api);
        assert_eq!(mode.code(), code);
        assert_eq!(OutboundMode::from_api(api), mode);
        assert_eq!(OutboundMode::from_api(code), mode);
        assert!(!mode.label().is_empty());
    }
    assert_eq!(OutboundMode::from_api("unknown-mode"), OutboundMode::Rule);

    let temp = TempDir::new("outbound-mode");
    let controlled = ControlledConfigStore::new(temp.path().join("controlled"));
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::new("http://127.0.0.1:1", "")).unwrap(),
    )
    .unwrap();

    let coordinator = OutboundModeCoordinator::new_unsynchronized(OutboundMode::Rule);
    assert_eq!(coordinator.displayed(), OutboundMode::Rule);
    assert_eq!(coordinator.intent_generation(), 0);
    assert!(!coordinator.is_pending());
    assert_eq!(coordinator.error(), None);

    let mut updates = coordinator.subscribe();
    coordinator.synchronize(OutboundMode::Global, 0);
    assert!(updates.has_changed().unwrap());
    let _ = *updates.borrow_and_update();
    assert_eq!(coordinator.displayed(), OutboundMode::Global);

    // Requesting the already-confirmed mode is deduplicated and does not spawn a task.
    assert!(!coordinator.request(
        OutboundMode::Global,
        &session,
        &controlled,
        &tokio::runtime::Handle::current(),
    ));
    assert!(!coordinator.is_pending());
    assert_eq!(coordinator.intent_generation(), 0);

    // Submitting a new mode advances intent generation and rejects stale runtime snapshots.
    assert!(coordinator.request(
        OutboundMode::Direct,
        &session,
        &controlled,
        &tokio::runtime::Handle::current(),
    ));
    assert_eq!(coordinator.intent_generation(), 1);
    coordinator.synchronize(OutboundMode::Rule, 0);
    assert_ne!(coordinator.displayed(), OutboundMode::Rule);

    tokio::time::timeout(Duration::from_secs(2), async {
        while coordinator.is_pending() {
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    // Unreachable controller rolls back displayed mode to last confirmed state.
    assert_eq!(coordinator.displayed(), OutboundMode::Global);
    assert!(coordinator.error().is_some());
}

#[test]
fn edge_stable_observation_enforces_commit_dedup_and_exponential_backoff() {
    let start = Instant::now();
    let mut tracker = EdgeStableObservation::new();

    // First sample only primes candidate; second identical sample triggers once.
    assert!(!tracker.observe(NetworkReachability::Online, start));
    assert!(tracker.observe(NetworkReachability::Online, start));
    tracker.mark_committed(NetworkReachability::Online);

    // Repeated unchanged observations do not re-fire.
    assert!(!tracker.observe(NetworkReachability::Online, start + Duration::from_secs(1)));
    assert!(!tracker.observe(NetworkReachability::Online, start + Duration::from_secs(10)));

    // Transition to Offline triggers, then backs off on failure (2s -> 4s).
    let t1 = start + Duration::from_secs(11);
    assert!(!tracker.observe(NetworkReachability::Offline, t1));
    assert!(tracker.observe(NetworkReachability::Offline, t1));
    tracker.record_failure(t1);
    assert!(!tracker.observe(NetworkReachability::Offline, t1 + Duration::from_secs(1)));
    assert!(tracker.observe(NetworkReachability::Offline, t1 + Duration::from_secs(2)));

    let t2 = t1 + Duration::from_secs(2);
    tracker.record_failure(t2);
    assert!(!tracker.observe(NetworkReachability::Offline, t2 + Duration::from_secs(3)));
    assert!(tracker.observe(NetworkReachability::Offline, t2 + Duration::from_secs(4)));

    // Changing observed state immediately resets failure backoff.
    let t3 = t2 + Duration::from_secs(4);
    assert!(!tracker.observe(NetworkReachability::Online, t3));
    assert!(!tracker.observe(NetworkReachability::Online, t3 + Duration::from_secs(1)));

    // StableObservation requires two consecutive matching revisions.
    let rev = SourceRevisionKey {
        source_profile: PathBuf::from("active.yaml"),
        digest: [7; 32],
    };
    let mut stable = StableObservation::new();
    assert!(!stable.observe(rev.clone()));
    assert!(stable.observe(rev));
}

#[test]
fn source_scanner_tracks_injected_profile_and_override_stores_without_discovery() {
    let temp = TempDir::new("source-scanner");
    let profiles = ProfileStore::new(temp.path().join("profiles")).unwrap();
    let overrides = YamlOverrideStore::new(temp.path().join("overrides")).unwrap();
    let controlled = ControlledConfigStore::new(temp.path().join("controlled"));

    let raw_profile = temp.path().join("import-profile.yaml");
    fs::write(&raw_profile, "mode: rule\nrules:\n  - MATCH,DIRECT\n").unwrap();
    let record = profiles.import_local(&raw_profile).unwrap();
    let active_profile = profiles.activate(&record.id).unwrap();

    let raw_override = temp.path().join("import-override.yaml");
    fs::write(&raw_override, "mode: direct\n").unwrap();
    let imported = overrides.import_paths([raw_override]).unwrap();
    assert_eq!(imported.len(), 1);
    let enabled_overrides = overrides.load_enabled_paths().unwrap();

    controlled
        .materialize_with_overrides_for_core(&active_profile, &enabled_overrides, CoreKind::Mihomo)
        .unwrap();

    let mut scanner = SourceScanner::new(Some(overrides));
    assert!(
        scanner
            .scan(&controlled, CoreKind::Mihomo, Some(active_profile.clone()))
            .unwrap()
            .is_none()
    );

    // Editing the active profile invalidates the cache and reports a pending revision.
    fs::write(
        &active_profile,
        "mode: rule\nlog-level: debug\nrules:\n  - MATCH,DIRECT\n",
    )
    .unwrap();
    let pending = scanner
        .scan(&controlled, CoreKind::Mihomo, Some(active_profile.clone()))
        .unwrap()
        .expect("edited active profile should produce a pending source revision");
    assert_eq!(pending.source_profile, active_profile);
    assert_eq!(pending.override_paths, enabled_overrides);

    // Materializing the new source clears the pending revision.
    controlled
        .materialize_with_overrides_for_core(&active_profile, &enabled_overrides, CoreKind::Mihomo)
        .unwrap();
    assert!(
        scanner
            .scan(&controlled, CoreKind::Mihomo, Some(active_profile))
            .unwrap()
            .is_none()
    );
    assert!(
        scanner
            .scan(&controlled, CoreKind::Mihomo, None)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn shutdown_coordinator_enforces_three_stage_interactive_quit_transitions() {
    let mut quit_state = QuitState::default();
    assert_eq!(quit_state, QuitState::Idle);

    let temp = TempDir::new("shutdown");
    let controlled = ControlledConfigStore::new(temp.path().join("controlled"));
    let core = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::new("http://127.0.0.1:1", "")).unwrap(),
    )
    .unwrap();
    let capture = TrafficCaptureSession::new(core.clone(), controlled.clone(), None, None);
    let released = capture.release_owned().await.unwrap();
    let coordinator = ShutdownCoordinator::new(core.clone(), Some(capture), None);

    // Stage 1 failure -> QuitState::Blocked without poisoning CoreSession.
    quit_state = QuitState::InProgress;
    assert_eq!(quit_state, QuitState::InProgress);
    let blocked = coordinator
        .execute_interactive_with_release(Err(TrafficCaptureError::Controller(MihomoError::Api {
            status: 500,
            message: "capture release refused".into(),
        })))
        .await;
    assert!(matches!(blocked, ShutdownOutcome::Blocked(_)));
    quit_state = QuitState::Blocked;
    assert_eq!(quit_state, QuitState::Blocked);
    assert!(!core.is_shutting_down());

    // Partial reconcile outcome also blocks before Stage 2.
    let reconcile = coordinator
        .execute_interactive_with_release(Ok(CaptureOutcome::ReconcileNeeded {
            plan: None,
            snapshot: released.snapshot().clone(),
            failure: "pending system proxy restore".into(),
        }))
        .await;
    assert!(matches!(reconcile, ShutdownOutcome::Blocked(_)));
    assert!(!core.is_shutting_down());

    // Retry with clean Stage 1 release -> Stage 2 shuts down core -> ReadyToExit.
    quit_state = QuitState::InProgress;
    let outcome = coordinator
        .execute_interactive_with_release(Ok(released))
        .await;
    assert_eq!(outcome, ShutdownOutcome::ReadyToExit);
    assert_eq!(quit_state, QuitState::InProgress);
    assert!(core.is_shutting_down());
    assert!(matches!(
        core.set_mode(&controlled, "rule").await,
        Err(CoreSessionError::ShuttingDown)
    ));
}

#[tokio::test]
async fn startup_cancellation_propagates_across_clones_and_races_futures() {
    let token = StartupCancellation::new();
    let worker_token = token.clone();
    assert!(!token.is_cancelled());
    assert!(!worker_token.is_cancelled());
    assert!(token.ensure_active().is_ok());

    let completed = token.run_until_cancelled(async { 42_u32 }).await.unwrap();
    assert_eq!(completed, 42);

    let waiter = tokio::spawn({
        let worker_token = worker_token.clone();
        async move {
            worker_token
                .run_until_cancelled(std::future::pending::<()>())
                .await
        }
    });

    token.cancel();
    assert!(token.is_cancelled());
    assert!(worker_token.is_cancelled());
    let err = worker_token.ensure_active().unwrap_err();
    assert!(is_startup_cancelled(&err));
    assert!(is_startup_cancelled(&startup_cancelled_error()));

    let race_err = tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(is_startup_cancelled(&race_err));
    tokio::time::timeout(Duration::from_secs(2), worker_token.cancelled())
        .await
        .unwrap();
}
