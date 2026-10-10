use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    net::TcpStream,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use super::*;

#[derive(Clone, Copy, Debug)]
enum Fault {
    Pass,
    PartialWrite,
    HttpOnly,
    Readback,
    Reject,
    CorruptInactiveUrl,
    ExternalHttps,
    ExternalCachedHttps,
    ExternalCachedPac,
    PartialHostWrite,
}

#[derive(Debug)]
struct FixtureState {
    active: String,
    services: HashMap<String, SystemProxyStatus>,
    faults: VecDeque<Fault>,
    fail_read: bool,
    writes: Vec<String>,
    break_preferences: Option<PathBuf>,
}

#[derive(Debug)]
struct FixtureBackend(Mutex<FixtureState>);

impl FixtureBackend {
    fn new() -> Self {
        Self(Mutex::new(FixtureState {
            active: "Wi-Fi".into(),
            services: HashMap::new(),
            faults: VecDeque::new(),
            fail_read: false,
            writes: Vec::new(),
            break_preferences: None,
        }))
    }

    fn write(
        &self,
        service: &str,
        update: impl FnOnce(&mut SystemProxyStatus),
    ) -> MihomoResult<()> {
        let mut state = self.0.lock();
        state.writes.push(service.into());
        let fault = state.faults.pop_front();
        if matches!(fault, Some(Fault::Reject)) {
            return Err(MihomoError::Process("fixture write rejected".into()));
        }
        let previous = state.services.get(service).cloned();
        update(
            state
                .services
                .entry(service.into())
                .or_insert_with(|| SystemProxyStatus {
                    service: service.into(),
                    ..Default::default()
                }),
        );
        if matches!(fault, Some(Fault::HttpOnly)) {
            state.services.get_mut(service).unwrap().secure_enabled = false;
        }
        if matches!(fault, Some(Fault::CorruptInactiveUrl)) {
            state.services.get_mut(service).unwrap().auto_url = "http://corrupted.test/pac".into();
        }
        if matches!(fault, Some(Fault::ExternalHttps)) {
            let actual = state.services.get_mut(service).unwrap();
            actual.secure_enabled = true;
            actual.secure_server = "new.external.test".into();
            actual.secure_port = 8443;
        }
        if matches!(fault, Some(Fault::ExternalCachedHttps)) {
            let actual = state.services.get_mut(service).unwrap();
            actual.secure_enabled = false;
            actual.secure_server = "new.cached.external.test".into();
            actual.secure_port = 8443;
        }
        if matches!(fault, Some(Fault::ExternalCachedPac)) {
            state.services.get_mut(service).unwrap().auto_url =
                "http://new.cached.external.test/config.pac".into();
        }
        if matches!(fault, Some(Fault::PartialHostWrite)) {
            let actual = state.services.get_mut(service).unwrap();
            let host = actual.server.clone();
            *actual = previous.unwrap();
            actual.server = host;
            actual.enabled = false;
            actual.secure_enabled = false;
            actual.auto_enabled = false;
        }
        state.fail_read = matches!(fault, Some(Fault::Readback));
        if let Some(path) = state.break_preferences.take() {
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
        }
        if matches!(
            fault,
            Some(
                Fault::PartialWrite
                    | Fault::HttpOnly
                    | Fault::ExternalHttps
                    | Fault::ExternalCachedHttps
                    | Fault::ExternalCachedPac
                    | Fault::PartialHostWrite
            )
        ) {
            return Err(MihomoError::Process(
                "fixture response failed after writing".into(),
            ));
        }
        Ok(())
    }
}

impl NativeProxyBackend for FixtureBackend {
    fn restore_snapshot(&self, previous: &SystemProxyStatus) -> MihomoResult<()> {
        self.write(&previous.service, |status| *status = previous.clone())
    }

    fn detect_service(&self) -> MihomoResult<String> {
        Ok(self.0.lock().active.clone())
    }
    fn status(&self, service: &str) -> MihomoResult<SystemProxyStatus> {
        let mut state = self.0.lock();
        if std::mem::take(&mut state.fail_read) {
            return Err(MihomoError::Process("fixture readback failed".into()));
        }
        Ok(state
            .services
            .get(service)
            .cloned()
            .unwrap_or_else(|| SystemProxyStatus {
                service: service.into(),
                ..Default::default()
            }))
    }
    fn set_manual(
        &self,
        service: &str,
        enabled: bool,
        host: &str,
        port: u16,
        bypass: &[String],
    ) -> MihomoResult<()> {
        self.write(service, |status| {
            status.auto_enabled = false;
            status.enabled = enabled;
            status.secure_enabled = enabled;
            if enabled {
                status.server = host.into();
                status.secure_server = host.into();
                status.port = port;
                status.secure_port = port;
                status.bypass = bypass.into();
            }
        })
    }
    fn set_pac(&self, service: &str, url: &str) -> MihomoResult<()> {
        self.write(service, |status| {
            status.enabled = false;
            status.secure_enabled = false;
            status.auto_enabled = true;
            status.auto_url = url.into();
        })
    }
}

struct Fixture {
    root: PathBuf,
    backend: Arc<FixtureBackend>,
    controller: SystemProxyController,
    session: SystemProxySession,
    store: AppPreferencesStore,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "zenclash-native-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let backend = Arc::new(FixtureBackend::new());
        let controller = SystemProxyController {
            native: backend.clone(),
            ..SystemProxyController::default()
        };
        let store = AppPreferencesStore::new(root.join("preferences.json"));
        let session = SystemProxySession::new(store.clone(), controller.clone());
        Self {
            root,
            backend,
            controller,
            session,
            store,
        }
    }
    fn start_pac(&self) -> PacServerStatus {
        let preferences = AppPreferences {
            system_proxy_mode: SystemProxyMode::Pac,
            ..AppPreferences::default()
        };
        self.store.save(&preferences).unwrap();
        self.session.set_enabled(true, 7890).unwrap();
        self.controller.pac_status().unwrap()
    }
    fn change_pac(&self) -> SystemProxySessionResult<AppPreferences> {
        self.session.save_settings(
            SystemProxySettings {
                mode: SystemProxyMode::Pac,
                host: "127.0.0.1".into(),
                bypass: Vec::new(),
                pac_script: "function FindProxyForURL(url, host) { return 'DIRECT'; }".into(),
            },
            7890,
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.controller.pac_server.stop();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn read_pac(status: &PacServerStatus) -> String {
    let mut stream = TcpStream::connect_timeout(&status.address, Duration::from_secs(1)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    stream
        .write_all(b"GET /pac HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

impl Fixture {
    fn capture(&self) -> crate::TrafficCaptureSession {
        let core = crate::CoreSession::open(
            crate::CoreKind::Mihomo,
            crate::MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        crate::TrafficCaptureSession::new(
            core,
            crate::ControlledConfigStore::new(self.root.join("controlled")),
            Some(self.session.clone()),
            None,
        )
    }

    fn observed_proxy(&self) -> crate::Observation<SystemProxySessionSnapshot> {
        crate::Observation::Fresh {
            value: self.session.snapshot().unwrap(),
            observed_at_ms: 1,
        }
    }
}

#[tokio::test]
async fn offline_startup_releases_owned_proxy_without_querying_the_placeholder_controller() {
    let fixture = Fixture::new();
    fixture.session.set_enabled(true, 7890).unwrap();
    let core =
        crate::CoreSession::open_offline(crate::CoreKind::Mihomo, fixture.root.clone(), None)
            .unwrap();
    let capture = crate::TrafficCaptureSession::new(
        core,
        crate::ControlledConfigStore::new(fixture.root.join("controlled")),
        Some(fixture.session.clone()),
        None,
    );
    let outcome = capture.reconcile().await.unwrap();
    assert!(
        matches!(outcome, crate::CaptureOutcome::Unchanged { .. }),
        "offline startup must not report a request to 127.0.0.1:0: {outcome:?}"
    );
    assert!(!outcome.snapshot().core_available);
    assert!(!fixture.session.snapshot().unwrap().actual.active());
    assert!(
        fixture.store.load().unwrap().system_proxy_enabled,
        "offline recovery must preserve the user's enabled intent"
    );
}

#[tokio::test]
async fn maintenance_proxy_suspension_persists_off_across_reconciliation_and_reopen() {
    let fixture = Fixture::new();
    fixture.session.set_enabled(true, 7890).unwrap();
    fixture
        .capture()
        .suspend_maintenance_proxy_admitted(&fixture.observed_proxy())
        .await
        .unwrap();
    let preferences = fixture.store.load().unwrap();
    assert!(!preferences.system_proxy_enabled);
    assert!(preferences.system_proxy_ownership.is_none());
    assert!(!fixture.backend.status("Wi-Fi").unwrap().active());
    let writes = fixture.backend.0.lock().writes.clone();
    let reopened = SystemProxySession::new(fixture.store.clone(), fixture.controller.clone());
    assert_eq!(
        reopened.reconcile(true, Some(7890)).unwrap(),
        SystemProxyReconcileOutcome::Unchanged
    );
    assert_eq!(fixture.backend.0.lock().writes, writes);
}

#[tokio::test]
async fn maintenance_proxy_suspension_releases_pac_and_preserves_a_native_replacement() {
    let fixture = Fixture::new();
    let listener = fixture.start_pac();
    let observed = fixture.observed_proxy();
    fixture
        .capture()
        .suspend_maintenance_proxy_admitted(&observed)
        .await
        .unwrap();
    assert!(!fixture.store.load().unwrap().system_proxy_enabled);
    assert!(fixture.controller.pac_status().is_none());
    assert!(TcpStream::connect(listener.address).is_err());

    fixture.session.set_enabled(true, 7890).unwrap();
    let external = SystemProxyStatus {
        service: "Wi-Fi".into(),
        enabled: true,
        server: "external.test".into(),
        port: 8080,
        ..Default::default()
    };
    fixture
        .backend
        .0
        .lock()
        .services
        .insert("Wi-Fi".into(), external.clone());
    let writes = fixture.backend.0.lock().writes.clone();
    fixture
        .capture()
        .suspend_maintenance_proxy_admitted(&fixture.observed_proxy())
        .await
        .unwrap();
    let saved = fixture.store.load().unwrap();
    assert!(!saved.system_proxy_enabled);
    assert!(saved.system_proxy_ownership.is_none());
    assert_eq!(fixture.backend.status("Wi-Fi").unwrap(), external);
    assert_eq!(fixture.backend.0.lock().writes, writes);
}

#[tokio::test]
async fn maintenance_proxy_suspension_native_failure_preserves_intent_and_ownership() {
    let fixture = Fixture::new();
    let expected = fixture.session.set_enabled(true, 7890).unwrap();
    let before = fixture.backend.status("Wi-Fi").unwrap();
    fixture.backend.0.lock().faults.push_back(Fault::Reject);
    assert!(
        fixture
            .capture()
            .suspend_maintenance_proxy_admitted(&fixture.observed_proxy())
            .await
            .is_err()
    );
    assert_eq!(fixture.store.load().unwrap(), expected);
    assert_eq!(fixture.backend.status("Wi-Fi").unwrap(), before);
}

#[tokio::test]
async fn maintenance_proxy_suspension_save_failure_restores_the_previous_live_pac() {
    let fixture = Fixture::new();
    let listener = fixture.start_pac();
    let observed = fixture.observed_proxy();
    let before = fixture.backend.status("Wi-Fi").unwrap();
    fixture.backend.0.lock().break_preferences = Some(fixture.root.join("preferences.json"));
    assert!(
        fixture
            .capture()
            .suspend_maintenance_proxy_admitted(&observed)
            .await
            .is_err()
    );
    assert_eq!(fixture.backend.status("Wi-Fi").unwrap(), before);
    assert_eq!(fixture.controller.pac_status().unwrap(), listener);
    assert!(read_pac(&listener).contains("127.0.0.1:7890"));
}

#[tokio::test]
async fn maintenance_proxy_suspension_off_or_unknown_does_not_write_preferences_or_native_state() {
    let fixture = Fixture::new();
    fixture.store.save(&AppPreferences::default()).unwrap();
    let before = std::fs::read(fixture.root.join("preferences.json")).unwrap();
    fixture
        .capture()
        .suspend_maintenance_proxy_admitted(&fixture.observed_proxy())
        .await
        .unwrap();
    assert!(
        fixture
            .capture()
            .suspend_maintenance_proxy_admitted(&crate::Observation::Loading)
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.root.join("preferences.json")).unwrap(),
        before
    );
    assert!(fixture.backend.0.lock().writes.is_empty());
}

#[test]
fn exit_releases_the_recorded_service_after_the_active_service_changes() {
    let fixture = Fixture::new();
    fixture.session.set_enabled(true, 7890).unwrap();
    let external = SystemProxyStatus {
        service: "Ethernet".into(),
        enabled: true,
        server: "external.test".into(),
        port: 8080,
        ..Default::default()
    };
    {
        let mut state = fixture.backend.0.lock();
        state.active = "Ethernet".into();
        state.services.insert("Ethernet".into(), external.clone());
    }
    assert!(fixture.session.release_owned().unwrap());
    assert!(!fixture.backend.status("Wi-Fi").unwrap().active());
    assert_eq!(fixture.backend.status("Ethernet").unwrap(), external);
    let preferences = fixture.store.load().unwrap();
    assert!(preferences.system_proxy_enabled);
    assert!(preferences.system_proxy_ownership.is_none());
}

#[test]
fn exit_preserves_a_replacement_on_the_recorded_service() {
    let fixture = Fixture::new();
    fixture.session.set_enabled(true, 7890).unwrap();
    let external = SystemProxyStatus {
        service: "Wi-Fi".into(),
        enabled: true,
        server: "external.test".into(),
        port: 8080,
        ..Default::default()
    };
    fixture
        .backend
        .0
        .lock()
        .services
        .insert("Wi-Fi".into(), external.clone());
    assert!(!fixture.session.release_owned().unwrap());
    assert_eq!(fixture.backend.status("Wi-Fi").unwrap(), external);
}

#[test]
fn exit_keeps_owned_services_alive_when_an_external_proxy_creates_mixed_state() {
    for pac in [false, true] {
        let fixture = Fixture::new();
        let listener = if pac {
            Some(fixture.start_pac())
        } else {
            fixture.session.set_enabled(true, 7890).unwrap();
            None
        };
        {
            let mut state = fixture.backend.0.lock();
            let actual = state.services.get_mut("Wi-Fi").unwrap();
            actual.secure_enabled = true;
            actual.secure_server = "external.test".into();
            actual.secure_port = 8080;
        }
        assert!(fixture.session.release_owned().is_err());
        assert!(
            fixture
                .store
                .load()
                .unwrap()
                .system_proxy_ownership
                .is_some()
        );
        if let Some(listener) = &listener {
            assert!(read_pac(listener).contains("127.0.0.1:7890"));
        }
        let ownership = fixture
            .store
            .load()
            .unwrap()
            .system_proxy_ownership
            .unwrap();
        assert!(
            fixture
                .controller
                .begin_operation()
                .release_if_owned(&ownership)
                .is_err()
        );
        assert_eq!(
            fixture.backend.status("Wi-Fi").unwrap().secure_server,
            "external.test"
        );
        fixture.backend.0.lock().active = "Ethernet".into();
        assert!(fixture.session.reconcile(true, Some(7890)).is_err());
        assert!(!fixture.backend.status("Ethernet").unwrap().active());
        if let Some(listener) = &listener {
            assert!(read_pac(listener).contains("127.0.0.1:7890"));
        }
    }
}

#[test]
fn enabling_again_and_reconciling_migrate_without_leaving_the_old_service_enabled() {
    for reconcile in [false, true] {
        let fixture = Fixture::new();
        fixture.session.set_enabled(true, 7890).unwrap();
        fixture.backend.0.lock().active = "Ethernet".into();
        if reconcile {
            assert_eq!(
                fixture.session.reconcile(true, Some(7890)).unwrap(),
                SystemProxyReconcileOutcome::Restored
            );
        } else {
            fixture.session.set_enabled(true, 7890).unwrap();
        }
        assert!(!fixture.backend.status("Wi-Fi").unwrap().active());
        assert!(fixture.backend.status("Ethernet").unwrap().active());
        assert!(fixture.session.release_owned().unwrap());
        assert!(!fixture.backend.status("Ethernet").unwrap().active());
    }
}

#[test]
fn a_failed_service_release_does_not_write_the_new_service_or_forget_ownership() {
    let fixture = Fixture::new();
    let expected = fixture.session.set_enabled(true, 7890).unwrap();
    {
        let mut state = fixture.backend.0.lock();
        state.active = "Ethernet".into();
        state.faults.push_back(Fault::Reject);
    }
    assert!(fixture.session.set_enabled(true, 7890).is_err());
    assert!(!fixture.backend.status("Ethernet").unwrap().active());
    assert!(fixture.backend.status("Wi-Fi").unwrap().active());
    assert_eq!(fixture.store.load().unwrap(), expected);
    assert!(
        !fixture
            .backend
            .0
            .lock()
            .writes
            .iter()
            .any(|service| service == "Ethernet")
    );
}

#[test]
fn partial_pac_write_and_failed_readback_restore_the_live_previous_listener() {
    for fault in [Fault::PartialWrite, Fault::Readback] {
        let fixture = Fixture::new();
        let old = fixture.start_pac();
        let expected = fixture.store.load().unwrap();
        fixture.backend.0.lock().faults.push_back(fault);
        assert!(fixture.change_pac().is_err());
        assert_eq!(fixture.backend.status("Wi-Fi").unwrap().auto_url, old.url);
        assert!(read_pac(&old).contains("127.0.0.1:7890"));
        assert_eq!(fixture.store.load().unwrap(), expected);
        assert_eq!(fixture.controller.pac_status().unwrap(), old);
    }
}

#[test]
fn successful_pac_commit_closes_the_old_listener_and_serves_the_new_script() {
    let fixture = Fixture::new();
    let old = fixture.start_pac();
    fixture.change_pac().unwrap();
    let new = fixture.controller.pac_status().unwrap();
    assert!(TcpStream::connect(old.address).is_err());
    assert!(read_pac(&new).contains("return 'DIRECT'"));
    assert_eq!(fixture.backend.status("Wi-Fi").unwrap().auto_url, new.url);
}

#[test]
fn failed_rollback_and_disable_keep_the_referenced_candidate_until_release_can_recover() {
    let fixture = Fixture::new();
    let old = fixture.start_pac();
    fixture
        .backend
        .0
        .lock()
        .faults
        .extend([Fault::PartialWrite, Fault::Reject, Fault::Reject]);
    assert!(fixture.change_pac().is_err());
    let actual = fixture.backend.status("Wi-Fi").unwrap();
    let address = reqwest::Url::parse(&actual.auto_url)
        .unwrap()
        .socket_addrs(|| None)
        .unwrap()[0];
    assert!(
        read_pac(&PacServerStatus {
            address,
            url: actual.auto_url
        })
        .contains("return 'DIRECT'")
    );
    assert!(read_pac(&old).contains("127.0.0.1:7890"));
    assert!(fixture.change_pac().is_err());
    fixture.session.release_owned().unwrap();
    assert!(!fixture.backend.status("Wi-Fi").unwrap().active());
    assert!(TcpStream::connect(address).is_err());
    assert!(TcpStream::connect(old.address).is_err());
}

#[test]
fn failed_first_enable_is_recovered_on_exit_without_persisted_ownership() {
    for mode in [SystemProxyMode::Pac, SystemProxyMode::Manual] {
        let fixture = Fixture::new();
        fixture
            .store
            .save(&AppPreferences {
                system_proxy_mode: mode,
                ..AppPreferences::default()
            })
            .unwrap();
        fixture
            .backend
            .0
            .lock()
            .faults
            .extend([Fault::PartialWrite, Fault::Reject, Fault::Reject]);
        assert!(fixture.session.set_enabled(true, 7890).is_err());
        assert!(fixture.backend.status("Wi-Fi").unwrap().active());
        assert!(!fixture.store.load().unwrap().system_proxy_enabled);
        assert!(
            fixture
                .store
                .load()
                .unwrap()
                .system_proxy_ownership
                .is_none()
        );
        fixture.session.release_owned().unwrap();
        assert!(!fixture.backend.status("Wi-Fi").unwrap().active());
        assert!(fixture.controller.pac_status().is_none());
    }
}

#[test]
fn partial_http_enable_is_recovered_without_clearing_an_external_https_proxy() {
    for external_https in [false, true] {
        let fixture = Fixture::new();
        fixture
            .backend
            .0
            .lock()
            .faults
            .extend([Fault::HttpOnly, Fault::Reject, Fault::Reject]);
        assert!(fixture.session.set_enabled(true, 7890).is_err());
        let known_observed = fixture.backend.status("Wi-Fi").unwrap();
        let writes = fixture.backend.0.lock().writes.len();
        if external_https {
            let mut state = fixture.backend.0.lock();
            let actual = state.services.get_mut("Wi-Fi").unwrap();
            actual.secure_enabled = true;
            actual.secure_server = "external.test".into();
            actual.secure_port = 8080;
        }
        let result = fixture.session.release_owned();
        if external_https {
            assert!(result.is_err());
            let actual = fixture.backend.status("Wi-Fi").unwrap();
            assert_eq!(actual.secure_server, "external.test");
            assert!(actual.enabled);
            assert!(fixture.controller.recovery.lock().is_some());
            assert_eq!(fixture.backend.0.lock().writes.len(), writes);
            fixture
                .backend
                .0
                .lock()
                .services
                .get_mut("Wi-Fi")
                .unwrap()
                .secure_enabled = false;
            assert!(fixture.session.release_owned().is_err());
            let actual = fixture.backend.status("Wi-Fi").unwrap();
            assert!(!actual.secure_enabled);
            assert_eq!(actual.secure_server, "external.test");
            assert_eq!(actual.secure_port, 8080);
            assert!(fixture.controller.recovery.lock().is_some());
            assert_eq!(fixture.backend.0.lock().writes.len(), writes);
            fixture
                .backend
                .0
                .lock()
                .services
                .insert("Wi-Fi".into(), known_observed);
            fixture.session.release_owned().unwrap();
        } else {
            result.unwrap();
        }
        assert!(!fixture.backend.status("Wi-Fi").unwrap().active());
        assert!(fixture.controller.recovery.lock().is_none());
    }
}

#[test]
fn failed_rollback_with_successful_disable_retains_the_previous_listener_until_retry() {
    let fixture = Fixture::new();
    let old = fixture.start_pac();
    fixture
        .backend
        .0
        .lock()
        .faults
        .extend([Fault::PartialWrite, Fault::Reject]);
    assert!(fixture.change_pac().is_err());
    assert!(!fixture.backend.status("Wi-Fi").unwrap().active());
    assert!(fixture.controller.recovery.lock().is_some());
    assert!(read_pac(&old).contains("127.0.0.1:7890"));
    fixture.session.release_owned().unwrap();
    assert!(fixture.controller.recovery.lock().is_none());
    assert!(fixture.controller.pac_status().is_none());
    assert!(TcpStream::connect(old.address).is_err());
}

#[test]
fn preference_failure_restores_previous_native_state_before_discarding_the_candidate() {
    let fixture = Fixture::new();
    let old = fixture.start_pac();
    fixture.backend.0.lock().break_preferences = Some(fixture.root.join("preferences.json"));
    assert!(fixture.change_pac().is_err());
    assert_eq!(fixture.backend.status("Wi-Fi").unwrap().auto_url, old.url);
    assert!(read_pac(&old).contains("127.0.0.1:7890"));
}

#[test]
fn release_does_not_reenable_a_proxy_when_ownership_persistence_fails() {
    for reconcile in [false, true] {
        let fixture = Fixture::new();
        let old = fixture.start_pac();
        fixture.backend.0.lock().break_preferences = Some(fixture.root.join("preferences.json"));
        let result = if reconcile {
            fixture.session.reconcile(false, None).map(|_| ())
        } else {
            fixture.session.release_owned().map(|_| ())
        };
        assert!(result.is_err());
        assert!(!fixture.backend.status("Wi-Fi").unwrap().active());
        assert!(TcpStream::connect(old.address).is_err());
    }
}

fn external_snapshot(enabled: bool, secure_enabled: bool, auto_enabled: bool) -> SystemProxyStatus {
    SystemProxyStatus {
        service: "Wi-Fi".into(),
        enabled,
        server: "http.original.test".into(),
        port: 8080,
        secure_enabled,
        secure_server: "https.original.test".into(),
        secure_port: 8443,
        bypass: vec!["localhost".into(), "*.original.test".into()],
        auto_enabled,
        auto_url: "http://pac.original.test/config.pac".into(),
    }
}

#[test]
fn failed_write_restores_every_captured_protocol_flag_endpoint_and_bypass() {
    for (http, https, pac) in [
        (true, true, false),
        (true, false, false),
        (false, true, false),
        (false, false, false),
        (true, true, true),
        (true, false, true),
        (false, true, true),
        (false, false, true),
    ] {
        let fixture = Fixture::new();
        let before = external_snapshot(http, https, pac);
        {
            let mut state = fixture.backend.0.lock();
            state.services.insert("Wi-Fi".into(), before.clone());
            state.faults.push_back(Fault::PartialWrite);
        }

        assert!(fixture.session.set_enabled(true, 7890).is_err());

        assert_eq!(fixture.backend.status("Wi-Fi").unwrap(), before);
        assert!(fixture.controller.recovery.lock().is_none());
        assert!(!fixture.store.load().unwrap().system_proxy_enabled);
    }
}

#[test]
fn failed_write_initial_rollback_preserves_a_new_external_active_protocol() {
    for fault in [
        Fault::ExternalHttps,
        Fault::ExternalCachedHttps,
        Fault::ExternalCachedPac,
    ] {
        let fixture = Fixture::new();
        fixture.backend.0.lock().faults.push_back(fault);

        assert!(fixture.session.set_enabled(true, 7890).is_err());

        let actual = fixture.backend.status("Wi-Fi").unwrap();
        assert!(actual.enabled);
        assert_eq!((actual.server.as_str(), actual.port), ("127.0.0.1", 7890));
        match fault {
            Fault::ExternalHttps => {
                assert!(actual.secure_enabled);
                assert_eq!(
                    (actual.secure_server.as_str(), actual.secure_port),
                    ("new.external.test", 8443)
                );
            }
            Fault::ExternalCachedHttps => {
                assert!(!actual.secure_enabled);
                assert_eq!(
                    (actual.secure_server.as_str(), actual.secure_port),
                    ("new.cached.external.test", 8443)
                );
            }
            Fault::ExternalCachedPac => {
                assert!(!actual.auto_enabled);
                assert_eq!(
                    actual.auto_url,
                    "http://new.cached.external.test/config.pac"
                );
            }
            _ => unreachable!(),
        }
        assert_eq!(fixture.backend.0.lock().writes, ["Wi-Fi"]);
        assert!(fixture.controller.recovery.lock().is_some());
        assert!(fixture.session.release_owned().is_err());
        assert_eq!(fixture.backend.status("Wi-Fi").unwrap(), actual);
        assert_eq!(fixture.backend.0.lock().writes, ["Wi-Fi"]);
    }
}

#[test]
fn failed_inactive_host_only_write_restores_the_captured_snapshot() {
    let fixture = Fixture::new();
    let before = external_snapshot(true, true, false);
    {
        let mut state = fixture.backend.0.lock();
        state.services.insert("Wi-Fi".into(), before.clone());
        state.faults.push_back(Fault::PartialHostWrite);
    }

    assert!(fixture.session.set_enabled(true, 7890).is_err());

    assert_eq!(fixture.backend.status("Wi-Fi").unwrap(), before);
    assert!(fixture.controller.recovery.lock().is_none());
}

#[test]
fn failed_restore_retains_the_original_snapshot_after_a_successful_safety_disable() {
    let fixture = Fixture::new();
    let before = external_snapshot(true, false, true);
    {
        let mut state = fixture.backend.0.lock();
        state.services.insert("Wi-Fi".into(), before.clone());
        state.faults.extend([Fault::PartialWrite, Fault::Reject]);
    }
    assert!(fixture.session.set_enabled(true, 7890).is_err());
    assert!(!fixture.backend.status("Wi-Fi").unwrap().active());
    assert!(fixture.controller.recovery.lock().is_some());

    // No persisted ZenClash claim exists: retry restores the captured external
    // proxy instead of treating safety-disable as the final recovered state.
    assert!(!fixture.session.release_owned().unwrap());

    assert_eq!(fixture.backend.status("Wi-Fi").unwrap(), before);
    assert!(fixture.controller.recovery.lock().is_none());
}

#[test]
fn failed_restore_retry_preserves_a_new_external_proxy_and_remains_recoverable() {
    let fixture = Fixture::new();
    let before = external_snapshot(true, true, false);
    {
        let mut state = fixture.backend.0.lock();
        state.services.insert("Wi-Fi".into(), before.clone());
        state
            .faults
            .extend([Fault::PartialWrite, Fault::Reject, Fault::Reject]);
    }
    assert!(fixture.session.set_enabled(true, 7890).is_err());
    let external = SystemProxyStatus {
        server: "new.external.test".into(),
        secure_server: "new.external.test".into(),
        ..before
    };
    fixture
        .backend
        .0
        .lock()
        .services
        .insert("Wi-Fi".into(), external.clone());

    assert!(fixture.session.release_owned().is_err());

    assert_eq!(fixture.backend.status("Wi-Fi").unwrap(), external);
    assert!(fixture.controller.recovery.lock().is_some());
}

#[test]
fn failed_restore_retry_preserves_a_later_external_disable_or_bypass_change() {
    for disable in [false, true] {
        let fixture = Fixture::new();
        {
            let mut state = fixture.backend.0.lock();
            state
                .services
                .insert("Wi-Fi".into(), external_snapshot(true, true, false));
            state
                .faults
                .extend([Fault::PartialWrite, Fault::Reject, Fault::Reject]);
        }
        assert!(fixture.session.set_enabled(true, 7890).is_err());
        let external = {
            let mut state = fixture.backend.0.lock();
            let actual = state.services.get_mut("Wi-Fi").unwrap();
            if disable {
                actual.enabled = false;
                actual.secure_enabled = false;
            } else {
                actual.bypass = vec!["changed.external.test".into()];
            }
            actual.clone()
        };
        let writes = fixture.backend.0.lock().writes.len();

        assert!(fixture.session.release_owned().is_err());

        assert_eq!(fixture.backend.status("Wi-Fi").unwrap(), external);
        assert_eq!(fixture.backend.0.lock().writes.len(), writes);
        assert!(fixture.controller.recovery.lock().is_some());
    }
}

#[test]
fn failed_restore_retry_preserves_external_changes_to_inactive_cached_fields() {
    for change_https in [true, false] {
        let fixture = Fixture::new();
        {
            let mut state = fixture.backend.0.lock();
            state
                .services
                .insert("Wi-Fi".into(), external_snapshot(true, true, false));
            state
                .faults
                .extend([Fault::PartialWrite, Fault::Reject, Fault::Reject]);
        }
        assert!(fixture.session.set_enabled(true, 7890).is_err());
        let external = {
            let mut state = fixture.backend.0.lock();
            let actual = state.services.get_mut("Wi-Fi").unwrap();
            assert!(actual.enabled);
            if change_https {
                actual.secure_enabled = false;
                actual.secure_server = "new.cached.external.test".into();
                actual.secure_port = 8443;
            } else {
                assert!(!actual.auto_enabled);
                actual.auto_url = "http://new.cached.external.test/config.pac".into();
            }
            actual.clone()
        };
        let writes = fixture.backend.0.lock().writes.len();

        assert!(fixture.session.release_owned().is_err());

        assert_eq!(fixture.backend.status("Wi-Fi").unwrap(), external);
        assert_eq!(fixture.backend.0.lock().writes.len(), writes);
        assert!(fixture.controller.recovery.lock().is_some());
    }
}

#[test]
fn a_successful_restore_call_with_incorrect_disabled_values_remains_retryable() {
    let fixture = Fixture::new();
    let before = external_snapshot(false, false, false);
    {
        let mut state = fixture.backend.0.lock();
        state.services.insert("Wi-Fi".into(), before.clone());
        state
            .faults
            .extend([Fault::PartialWrite, Fault::CorruptInactiveUrl]);
    }
    assert!(fixture.session.set_enabled(true, 7890).is_err());
    assert_ne!(fixture.backend.status("Wi-Fi").unwrap(), before);
    assert!(fixture.controller.recovery.lock().is_some());

    assert!(!fixture.session.release_owned().unwrap());

    assert_eq!(fixture.backend.status("Wi-Fi").unwrap(), before);
    assert!(fixture.controller.recovery.lock().is_none());
}

#[test]
fn a_failed_restore_readback_reapplies_even_equal_stored_values_before_clearing_recovery() {
    let fixture = Fixture::new();
    let before = external_snapshot(true, false, false);
    {
        let mut state = fixture.backend.0.lock();
        state.services.insert("Wi-Fi".into(), before.clone());
        state.faults.extend([Fault::PartialWrite, Fault::Readback]);
    }
    assert!(fixture.session.set_enabled(true, 7890).is_err());
    assert_eq!(fixture.backend.status("Wi-Fi").unwrap(), before);
    assert!(fixture.controller.recovery.lock().is_some());
    let writes = fixture.backend.0.lock().writes.len();

    assert!(!fixture.session.release_owned().unwrap());

    assert!(fixture.backend.0.lock().writes.len() > writes);
    assert_eq!(fixture.backend.status("Wi-Fi").unwrap(), before);
    assert!(fixture.controller.recovery.lock().is_none());
}

#[test]
fn a_failed_migration_restores_both_services_and_keeps_the_old_owned_pac_alive() {
    let fixture = Fixture::new();
    let old = fixture.start_pac();
    let preferences = fixture.store.load().unwrap();
    let old_service = fixture.backend.status("Wi-Fi").unwrap();
    let external = SystemProxyStatus {
        service: "Ethernet".into(),
        ..external_snapshot(true, false, true)
    };
    {
        let mut state = fixture.backend.0.lock();
        state.active = "Ethernet".into();
        state.services.insert("Ethernet".into(), external.clone());
        state.faults.extend([Fault::Pass, Fault::PartialWrite]);
    }

    assert!(fixture.session.set_enabled(true, 7890).is_err());

    assert_eq!(fixture.backend.status("Wi-Fi").unwrap(), old_service);
    assert_eq!(fixture.backend.status("Ethernet").unwrap(), external);
    assert_eq!(fixture.store.load().unwrap(), preferences);
    assert!(read_pac(&old).contains("127.0.0.1:7890"));
    assert!(fixture.session.release_owned().unwrap());
    assert!(!fixture.backend.status("Wi-Fi").unwrap().active());
    assert_eq!(fixture.backend.status("Ethernet").unwrap(), external);
}
