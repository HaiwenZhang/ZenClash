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
    PartialWrite,
    HttpOnly,
    Readback,
    Reject,
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
        state.fail_read = matches!(fault, Some(Fault::Readback));
        if let Some(path) = state.break_preferences.take() {
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
        }
        if matches!(fault, Some(Fault::PartialWrite | Fault::HttpOnly)) {
            return Err(MihomoError::Process(
                "fixture response failed after writing".into(),
            ));
        }
        Ok(())
    }
}

impl NativeProxyBackend for FixtureBackend {
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
            fixture
                .backend
                .0
                .lock()
                .services
                .get_mut("Wi-Fi")
                .unwrap()
                .secure_enabled = false;
            fixture.session.release_owned().unwrap();
        } else {
            result.unwrap();
        }
        assert!(!fixture.backend.status("Wi-Fi").unwrap().active());
    }
}

#[test]
fn failed_rollback_with_successful_disable_closes_both_listeners() {
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
