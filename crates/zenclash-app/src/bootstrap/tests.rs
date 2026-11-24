use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use zenclash_core::{
    AppPreferences, ControlledConfigStore, CoreKind, CoreLaunchConfig, ManagedCoreProcess,
    MihomoEndpoint, ProfileStore, ServiceHealthKind, ServiceManager, ServiceOperation,
    TrafficCaptureSession, YamlOverrideStore,
};

use super::{
    BootstrapInputs, BootstrapOrchestrator, StartupBlocked, StartupCancellation, StartupOutcome,
    append_startup_notice, blocked_message,
    core_startup::{
        LegacyStartupContext, is_controller_listener_conflict, offline_core_state,
        prepare_disconnected_startup, prepare_offline_startup, prepare_offline_startup_with_binary,
        project_root, recover_core, recover_offline_startup, recover_safe_profile,
        wait_for_managed_core_ready,
    },
    startup_cancelled_error,
};
use crate::SafeMode;

fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn pending_gui_has_no_controller_owner_or_applied_configuration() {
    let pending = prepare_disconnected_startup(CoreKind::Mihomo, None).unwrap();
    assert_eq!(pending.client.endpoint().unwrap().controller, "127.0.0.1:0");
    assert!(!pending.session.is_managed());
    assert!(pending.profile.is_none());
    assert!(pending.initialization.is_none());
    assert!(pending.error.is_none());
}

#[test]
fn pending_gui_preserves_ready_configuration_libraries() {
    let root = unique_temp_dir("zenclash-pending-libraries");
    let inputs = BootstrapInputs {
        preferences_store: None,
        preferences: AppPreferences::default(),
        requested_core: CoreKind::Mihomo,
        environment_core: None,
        controlled_config_store: ControlledConfigStore::new(root.join("controlled")),
        profile_store: ProfileStore::new(root.join("profiles")).unwrap(),
        override_store: YamlOverrideStore::new(root.join("overrides")).unwrap(),
        traffic_history_store: None,
        recovery_notices: Vec::new(),
        safe_mode: SafeMode::disabled(),
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let services = BootstrapOrchestrator::prepare_pending_services(
        &inputs,
        runtime.handle(),
        Arc::new(parking_lot::Mutex::new(None)),
    )
    .unwrap();
    let profile_root = services
        .profile_store
        .as_ref()
        .map(|store| store.root().to_path_buf());
    let override_root = services
        .override_store
        .as_ref()
        .map(|store| store.root().to_path_buf());
    drop(services);
    runtime.shutdown_timeout(Duration::from_secs(1));
    std::fs::remove_dir_all(&root).unwrap();
    assert_eq!(profile_root, Some(root.join("profiles")), "配置仓库不可用");
    assert_eq!(
        override_root,
        Some(root.join("overrides")),
        "YAML 覆写仓库不可用"
    );
}

#[test]
fn cancelled_bootstrap_returns_before_loading_layers_or_starting_a_core() {
    let root = unique_temp_dir("zenclash-cancelled-startup");
    let controlled = ControlledConfigStore::new(root.join("controlled"));
    std::fs::create_dir_all(root.join("controlled")).unwrap();
    let damaged = root.join("controlled/override.yaml");
    std::fs::write(&damaged, "tun: [invalid").unwrap();
    let inputs = BootstrapInputs {
        preferences_store: None,
        preferences: AppPreferences::default(),
        requested_core: CoreKind::Mihomo,
        environment_core: None,
        controlled_config_store: controlled,
        profile_store: ProfileStore::new(root.join("profiles")).unwrap(),
        override_store: YamlOverrideStore::new(root.join("overrides")).unwrap(),
        traffic_history_store: None,
        recovery_notices: Vec::new(),
        safe_mode: SafeMode::disabled(),
    };
    let cleanup = Arc::new(parking_lot::Mutex::new(None));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let cancellation = StartupCancellation::new();
    cancellation.cancel();
    let outcome = BootstrapOrchestrator::prepare_application(
        &runtime,
        inputs,
        cancellation,
        cleanup.clone(),
        Arc::new(parking_lot::Mutex::new(None)),
    );
    assert!(
        matches!(outcome, StartupOutcome::Cancelled),
        "cancelled bootstrap must short-circuit as StartupOutcome::Cancelled"
    );
    assert!(cleanup.lock().is_none());
    assert_eq!(std::fs::read_to_string(damaged).unwrap(), "tun: [invalid");
    assert!(root.starts_with(std::env::temp_dir()));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cancelled_startup_error_is_not_swallowed_into_offline_startup() {
    let active = StartupCancellation::new();
    for managed_mihomo in [true, false] {
        let error = recover_offline_startup(
            managed_mihomo,
            CoreKind::Mihomo,
            None,
            &active,
            Box::new(startup_cancelled_error()),
        )
        .err()
        .expect("Interrupted startup error must not degrade to offline startup");
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
    }

    let cancelled = StartupCancellation::new();
    cancelled.cancel();
    let error = recover_offline_startup(
        true,
        CoreKind::Mihomo,
        None,
        &cancelled,
        Box::new(std::io::Error::other("controller failed after quit")),
    )
    .err()
    .expect("cancelled flag must suppress offline startup fallback");
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );

    let offline = recover_offline_startup(
        true,
        CoreKind::Mihomo,
        None,
        &active,
        Box::new(std::io::Error::other("service unavailable")),
    )
    .unwrap();
    assert!(offline.session.is_offline_recovery());
    assert_eq!(offline.error.as_deref(), Some("service unavailable"));
}

#[tokio::test]
async fn wait_for_managed_core_ready_exits_promptly_when_cancelled() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let home = unique_temp_dir("zenclash-cancel-ready");
    let process = ManagedCoreProcess::prepare_stopped(CoreLaunchConfig {
        kind: CoreKind::Mihomo,
        binary: home.join("mihomo"),
        config_file: home.join("config.yaml"),
        home_dir: home,
        endpoint: MihomoEndpoint::new(format!("http://{address}"), ""),
        controller_override: None,
    });
    let cancellation = StartupCancellation::new();
    let trigger = cancellation.clone();
    let started = Instant::now();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        trigger.cancel();
    });

    let result = tokio::time::timeout(
        Duration::from_secs(1),
        wait_for_managed_core_ready(&process, Duration::from_secs(20), &cancellation),
    )
    .await
    .expect("cancelled readiness wait must not block on the 20s startup timeout");
    server.abort();
    let error = result.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn recover_core_and_safe_profile_stop_immediately_when_cancelled() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let store = ControlledConfigStore::new(std::env::temp_dir().join("zenclash-cancel-recover"));
    let cancellation = StartupCancellation::new();
    cancellation.cancel();
    let cause = std::io::Error::other("initial core start failed");
    let preferences = AppPreferences::default();
    let context = LegacyStartupContext {
        runtime: &runtime,
        preferences: &preferences,
        requested_core: CoreKind::Mihomo,
        preferred_binary: None,
        allow_core_recovery: true,
        profile_store: None,
        controlled_config_store: &store,
        override_paths: &[],
        cancellation: &cancellation,
    };

    let core_error = recover_core(&context, &cause)
        .err()
        .expect("cancelled recover_core must stop immediately");
    assert_eq!(core_error.kind(), std::io::ErrorKind::Interrupted);

    let profile_error = recover_safe_profile(&context, true, &cause)
        .err()
        .expect("cancelled recover_safe_profile must stop immediately");
    assert_eq!(profile_error.kind(), std::io::ErrorKind::Interrupted);
}

#[test]
fn blocked_service_startup_keeps_the_gui_available_without_a_kernel() {
    for reason in [
        StartupBlocked::Health(ServiceHealthKind::Unavailable("service stopped".into())),
        StartupBlocked::Health(ServiceHealthKind::Unknown),
        StartupBlocked::ConfigurationUnknown,
    ] {
        let prepared =
            prepare_offline_startup(CoreKind::Mihomo, blocked_message(reason.clone())).unwrap();
        assert_eq!(
            prepared.client.endpoint().unwrap().controller,
            "127.0.0.1:0"
        );
        assert!(!prepared.session.is_managed());
        assert!(prepared.session.is_offline_recovery());
        assert!(prepared.session.runtime_descriptor().binary().is_none());
        assert!(prepared.profile.is_none());
        assert!(prepared.initialization.is_none());
        assert_eq!(prepared.error, Some(blocked_message(reason)));
    }
}

#[test]
fn failed_core_preparation_preserves_the_error_for_the_gui() {
    let prepared = prepare_offline_startup(CoreKind::Mihomo, "invalid profile".into()).unwrap();
    assert_eq!(prepared.error.as_deref(), Some("invalid profile"));
    assert_eq!(
        prepared.client.endpoint().unwrap().controller,
        "127.0.0.1:0"
    );
    assert!(prepared.session.runtime_descriptor().binary().is_none());
}

#[test]
fn offline_startup_keeps_the_selected_core_for_service_maintenance() {
    let selected = std::env::temp_dir().join("missing-selected-mihomo");
    let prepared = prepare_offline_startup_with_binary(
        CoreKind::Mihomo,
        "invalid profile".into(),
        Some(selected),
    )
    .unwrap();
    let capture = TrafficCaptureSession::new(
        prepared.session.clone(),
        ControlledConfigStore::new(std::env::temp_dir().join("offline-unused-controlled")),
        None,
        None,
    );
    let manager = ServiceManager::new(prepared.session, capture);
    assert!(
        manager
            .request_maintenance(ServiceOperation::Repair, None)
            .is_ok()
    );
    assert!(manager.request_enable_tun().is_err());
    assert_eq!(prepared.error.as_deref(), Some("invalid profile"));
}

#[test]
fn pending_disconnected_startup_has_no_maintenance_capability() {
    let prepared = prepare_disconnected_startup(CoreKind::Mihomo, None).unwrap();
    assert!(!prepared.session.is_offline_recovery());
    assert!(prepared.error.is_none());
}

#[test]
fn failed_core_recovery_uses_an_impossible_controller_and_keeps_ui_state() {
    let state = offline_core_state(
        CoreKind::Mihomo,
        None,
        &std::io::Error::other("no eligible binary"),
    );

    assert_eq!(state.endpoint.controller, "127.0.0.1:0");
    assert!(state.process.is_none());
    assert!(state.notice.is_none());
    assert!(
        state.error.is_some_and(
            |message| message.contains("9090") && message.contains("no eligible binary")
        )
    );
}

#[test]
fn packaged_recovery_profile_is_direct_and_exposes_no_proxy_listener() {
    let payload = std::fs::read_to_string(
        project_root()
            .unwrap()
            .join("platforms/common/recovery.yaml"),
    )
    .unwrap();
    let config: serde_yaml::Value = serde_yaml::from_str(&payload).unwrap();

    assert_eq!(
        config.get("mixed-port").and_then(serde_yaml::Value::as_u64),
        Some(0)
    );
    assert_eq!(
        config.get("mode").and_then(serde_yaml::Value::as_str),
        Some("direct")
    );
    assert_eq!(
        config
            .get("rules")
            .and_then(serde_yaml::Value::as_sequence)
            .and_then(|rules| rules.first())
            .and_then(serde_yaml::Value::as_str),
        Some("MATCH,DIRECT")
    );
}

#[test]
fn recovery_notices_are_combined_without_losing_the_primary_reason() {
    let mut notice = Some("活动配置无效".to_owned());
    append_startup_notice(&mut notice, "受控层已隔离".to_owned());

    assert_eq!(
        notice,
        Some(format!(
            "活动配置无效{}受控层已隔离",
            zenclash_i18n::text("startup.separator")
        ))
    );
}

#[test]
fn only_external_controller_bind_failures_are_retryable() {
    assert!(is_controller_listener_conflict(
        "External controller listen error: listen tcp 127.0.0.1:19191: bind: address already in use"
    ));
    assert!(!is_controller_listener_conflict(
        "Start Mixed proxy error: listen tcp 127.0.0.1:7890: address already in use"
    ));
}
