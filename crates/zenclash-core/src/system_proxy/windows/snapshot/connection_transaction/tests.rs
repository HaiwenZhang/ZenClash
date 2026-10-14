use std::cell::{Cell, RefCell};

use windows_sys::Win32::Networking::WinInet::{
    PROXY_TYPE_AUTO_DETECT, PROXY_TYPE_AUTO_PROXY_URL, PROXY_TYPE_DIRECT, PROXY_TYPE_PROXY,
};

use super::*;

struct MemoryDefaultConnection {
    values: RefCell<InternetSettingsRegistryValues>,
    connection: RefCell<DefaultConnectionOptions>,
    events: RefCell<Vec<&'static str>>,
    write_count: Cell<usize>,
    fail_write_at: Cell<Option<usize>>,
    fail_after_write: Cell<bool>,
    fail_refresh: Cell<bool>,
    foreign_after_write: Cell<bool>,
    api_server_override: RefCell<Option<String>>,
}

impl MemoryDefaultConnection {
    fn new() -> Self {
        let mut server =
            registry_string_value("pRoXySeRvEr", "http=old:80;ftp=ftp:21;socks=socks:1080");
        server.kind = REG_EXPAND_SZ_KIND;
        let mut enabled = proxy_enable_value(true);
        enabled.bytes = 2_u32.to_le_bytes().to_vec().into();
        let values = [
            server,
            enabled,
            registry_string_value("Unmanaged", "retained"),
        ]
        .into_iter()
        .map(|value| (ascii_folded_value_name(&value.name), value))
        .collect();
        Self {
            values: RefCell::new(values),
            connection: RefCell::new(DefaultConnectionOptions {
                flags: PROXY_TYPE_DIRECT | PROXY_TYPE_PROXY | PROXY_TYPE_AUTO_DETECT | 0x8000_0000,
                proxy_server: Some("http=old:80;ftp=ftp:21;socks=socks:1080".into()),
                proxy_bypass: None,
                auto_config_url: Some("http://cached.test/proxy.pac".into()),
            }),
            events: RefCell::default(),
            write_count: Cell::new(0),
            fail_write_at: Cell::new(None),
            fail_after_write: Cell::new(false),
            fail_refresh: Cell::new(false),
            foreign_after_write: Cell::new(false),
            api_server_override: RefCell::new(None),
        }
    }

    fn failure() -> crate::MihomoError {
        crate::SystemProxyError::Platform {
            backend: crate::SystemProxyBackendKind::WinInet,
            operation: "isolated default option",
            code: 5,
        }
        .into()
    }

    fn begin_write(&self, event: &'static str) -> MihomoResult<bool> {
        self.events.borrow_mut().push(event);
        let count = self.write_count.get() + 1;
        self.write_count.set(count);
        let failed = self.fail_write_at.get() == Some(count);
        if failed && !self.fail_after_write.get() {
            return Err(Self::failure());
        }
        Ok(failed)
    }

    fn store(&self, name: &str, mut value: Option<RawRegistryValue>, preserve_name: bool) {
        let key = ascii_folded_value_name(&utf16_units(name));
        let mut values = self.values.borrow_mut();
        if preserve_name && let (Some(value), Some(old)) = (&mut value, values.get(&key)) {
            value.name.clone_from(&old.name);
        }
        if let Some(value) = value {
            values.insert(key, value);
        } else {
            values.remove(&key);
        }
    }
}

impl DefaultConnectionAccess for MemoryDefaultConnection {
    fn read_values(&self) -> MihomoResult<InternetSettingsRegistryValues> {
        Ok(self.values.borrow().clone())
    }
    fn query_connection(&self) -> MihomoResult<DefaultConnectionOptions> {
        let mut connection = self.connection.borrow().clone();
        let values = self.values.borrow();
        // This fixture reads the same stored strings that raw replay changes.
        connection.proxy_server = Some(registry_string(&values, "ProxyServer")?);
        connection.proxy_bypass = Some(registry_string(&values, "ProxyOverride")?);
        connection.auto_config_url = Some(registry_string(&values, "AutoConfigURL")?);
        if let Some(value) = self.api_server_override.borrow().as_ref() {
            connection.proxy_server = Some(value.clone());
        }
        Ok(connection)
    }
    fn set_option(&self, option: &DefaultConnectionOption) -> MihomoResult<()> {
        let failed = self.begin_write("native option")?;
        {
            let mut connection = self.connection.borrow_mut();
            match option {
                DefaultConnectionOption::Flags(flags) => {
                    connection.flags = *flags;
                    self.store(
                        "ProxyEnable",
                        Some(proxy_enable_value(flags & PROXY_TYPE_PROXY != 0)),
                        true,
                    );
                }
                DefaultConnectionOption::ProxyServer(value) => {
                    connection.proxy_server = Some(value.clone());
                    self.store(
                        "ProxyServer",
                        Some(registry_string_value("ProxyServer", value)),
                        true,
                    );
                }
                DefaultConnectionOption::ProxyBypass(value) => {
                    connection.proxy_bypass = Some(value.clone());
                    self.store(
                        "ProxyOverride",
                        Some(registry_string_value("ProxyOverride", value)),
                        true,
                    );
                }
                DefaultConnectionOption::AutoConfigUrl(value) => {
                    connection.auto_config_url = Some(value.clone());
                    self.store(
                        "AutoConfigURL",
                        Some(registry_string_value("AutoConfigURL", value)),
                        true,
                    );
                }
            }
        }
        if self.foreign_after_write.get() {
            self.store(
                "Unmanaged",
                Some(registry_string_value("Unmanaged", "external")),
                false,
            );
        }
        if failed { Err(Self::failure()) } else { Ok(()) }
    }
    fn restore_registry_value(
        &self,
        name: &str,
        value: Option<&RawRegistryValue>,
    ) -> MihomoResult<()> {
        let failed = self.begin_write("raw representation")?;
        self.store(name, value.cloned(), false);
        if failed { Err(Self::failure()) } else { Ok(()) }
    }
    fn refresh(&self) -> MihomoResult<()> {
        self.events.borrow_mut().push("refresh");
        if self.fail_refresh.get() {
            Err(Self::failure())
        } else {
            Ok(())
        }
    }
}

#[test]
fn compound_capture_uses_flags_instead_of_cached_pac_url() {
    let access = MemoryDefaultConnection::new();
    access.store(
        "AutoConfigURL",
        Some(registry_string_value(
            "AutoConfigURL",
            "http://cached.test/proxy.pac",
        )),
        false,
    );
    let snapshot = capture(&access).unwrap();
    assert!(!snapshot.summary().pac().is_enabled());
    assert_eq!(
        snapshot.summary().pac().url(),
        "http://cached.test/proxy.pac"
    );
    assert!(snapshot.summary().http().is_enabled());
    assert!(!snapshot.summary().https().is_enabled());
    assert!(access.events.borrow().is_empty());
}

#[test]
fn inconsistent_api_and_raw_storage_is_rejected_before_any_native_write() {
    let access = MemoryDefaultConnection::new();
    *access.api_server_override.borrow_mut() = Some("other:8080".into());
    let before = capture(&access).unwrap();
    let mut attempts = Vec::new();
    let result = apply(&access, &before, &NativeProxyChange::Disable, |state| {
        attempts.push(state)
    });
    assert!(
        matches!(result, Err(crate::MihomoError::SystemProxy(source))
        if matches!(source.as_ref(), crate::SystemProxyError::BackendUnavailable {
            reason: crate::SystemProxyBackendUnavailable::UnverifiedStorageContract, ..
        }))
    );
    assert!(attempts.is_empty() && access.events.borrow().is_empty());
    assert!(before.same_state(&capture(&access).unwrap()).unwrap());
}

#[test]
fn manual_mode_uses_native_options_and_retains_unknown_flags_and_extra_protocols() {
    let access = MemoryDefaultConnection::new();
    let before = capture(&access).unwrap();
    let after = apply(
        &access,
        &before,
        &NativeProxyChange::Manual {
            host: "new",
            port: 7890,
            bypass: &["localhost".into()],
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(
        access.events.borrow().as_slice(),
        [
            "native option",
            "native option",
            "native option",
            "native option",
            "native option",
            "refresh"
        ]
    );
    assert_eq!(
        after.connection().unwrap().flags,
        PROXY_TYPE_DIRECT | PROXY_TYPE_PROXY | 0x8000_0000
    );
    assert!(after.summary().http().is_enabled() && after.summary().https().is_enabled());
    assert_eq!(
        registry_string(&after.values, "ProxyServer").unwrap(),
        "http=new:7890;https=new:7890;ftp=ftp:21;socks=socks:1080"
    );
    assert!(
        after
            .values
            .get(&ascii_folded_value_name(&utf16_units("Unmanaged")))
            == before
                .values
                .get(&ascii_folded_value_name(&utf16_units("Unmanaged")))
    );
}

#[test]
fn pac_and_disable_modes_update_actual_connection_flags() {
    let access = MemoryDefaultConnection::new();
    let before = capture(&access).unwrap();
    let pac = apply(
        &access,
        &before,
        &NativeProxyChange::Pac {
            url: "http://127.0.0.1:1234/pac",
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(
        pac.connection().unwrap().flags,
        PROXY_TYPE_DIRECT | PROXY_TYPE_AUTO_PROXY_URL | 0x8000_0000
    );
    assert!(pac.summary().pac().is_enabled());
    assert!(!pac.summary().http().is_enabled());
    let disabled = apply(&access, &pac, &NativeProxyChange::Disable, |_| {}).unwrap();
    assert!(!disabled.summary().active());
    assert_eq!(
        disabled.connection().unwrap().flags,
        PROXY_TYPE_DIRECT | 0x8000_0000
    );
    assert_eq!(disabled.summary().http().host(), "old");
}

#[test]
fn restoration_replays_native_flags_and_original_raw_types_names_and_absence() {
    let access = MemoryDefaultConnection::new();
    let before = capture(&access).unwrap();
    let after = apply(
        &access,
        &before,
        &NativeProxyChange::Pac {
            url: "http://127.0.0.1:1234/pac",
        },
        |_| {},
    )
    .unwrap();
    access.events.borrow_mut().clear();
    let restored = restore(&access, &after, &before, |_| {}).unwrap();
    assert!(restored.same_state(&before).unwrap());
    assert_eq!(
        access.events.borrow().as_slice(),
        [
            "native option",
            "native option",
            "native option",
            "native option",
            "native option",
            "raw representation",
            "raw representation",
            "raw representation",
            "raw representation",
            "refresh"
        ]
    );
    assert!(
        !restored
            .values
            .contains_key(&ascii_folded_value_name(&utf16_units("AutoConfigURL")))
    );
    assert!(
        !restored
            .values
            .contains_key(&ascii_folded_value_name(&utf16_units("ProxyOverride")))
    );
    let server = restored
        .values
        .get(&ascii_folded_value_name(&utf16_units("ProxyServer")))
        .unwrap();
    assert_eq!(server.kind, REG_EXPAND_SZ_KIND);
    assert_eq!(server.name, utf16_units("pRoXySeRvEr"));
    assert_eq!(
        restored
            .values
            .get(&ascii_folded_value_name(&utf16_units("ProxyEnable")))
            .unwrap()
            .bytes
            .as_ref(),
        2_u32.to_le_bytes()
    );
}

#[test]
fn unprojected_flags_change_revokes_write_permission_even_with_identical_summary() {
    let access = MemoryDefaultConnection::new();
    let before = capture(&access).unwrap();
    access.connection.borrow_mut().flags ^= PROXY_TYPE_AUTO_DETECT;
    let changed = capture(&access).unwrap();
    assert_eq!(before.summary(), changed.summary());
    assert!(!before.same_state(&changed).unwrap());
    let mut attempts = Vec::new();
    let error = apply(&access, &before, &NativeProxyChange::Disable, |state| {
        attempts.push(state)
    })
    .err()
    .expect("changed flags must reject the original write");
    assert!(matches!(error, crate::MihomoError::SystemProxy(source)
        if matches!(source.as_ref(), crate::SystemProxyError::OwnershipChanged)));
    assert!(attempts.is_empty() && access.events.borrow().is_empty());
}

#[test]
fn every_native_apply_failure_retains_a_known_prefix_for_original_scope_restore() {
    for failed_step in 1..=5 {
        for after_write in [false, true] {
            let access = MemoryDefaultConnection::new();
            let before = capture(&access).unwrap();
            access.fail_write_at.set(Some(failed_step));
            access.fail_after_write.set(after_write);
            let mut attempts = vec![before.clone()];
            assert!(
                apply(
                    &access,
                    &before,
                    &NativeProxyChange::Manual {
                        host: "new",
                        port: 7890,
                        bypass: &[]
                    },
                    |state| attempts.push(state)
                )
                .is_err()
            );
            let actual = capture(&access).unwrap();
            assert_eq!(attempts.len(), failed_step + 1);
            assert!(
                attempts
                    .iter()
                    .any(|known| known.same_state(&actual).unwrap())
            );
            access.fail_write_at.set(None);
            let restored = restore(&access, &actual, &before, |_| {}).unwrap();
            assert!(restored.same_state(&before).unwrap());
        }
    }
}

#[test]
fn every_native_or_raw_restore_failure_retains_a_known_prefix_for_retry() {
    for failed_step in 1..=9 {
        for after_write in [false, true] {
            let access = MemoryDefaultConnection::new();
            let before = capture(&access).unwrap();
            let after = apply(
                &access,
                &before,
                &NativeProxyChange::Pac {
                    url: "http://127.0.0.1:1234/pac",
                },
                |_| {},
            )
            .unwrap();
            access.write_count.set(0);
            access.fail_write_at.set(Some(failed_step));
            access.fail_after_write.set(after_write);
            let mut attempts = vec![after.clone()];
            assert!(restore(&access, &after, &before, |state| attempts.push(state)).is_err());
            let actual = capture(&access).unwrap();
            assert_eq!(attempts.len(), failed_step + 1);
            assert!(
                attempts
                    .iter()
                    .any(|known| known.same_state(&actual).unwrap())
            );
            access.fail_write_at.set(None);
            assert!(
                restore(&access, &actual, &before, |_| {})
                    .unwrap()
                    .same_state(&before)
                    .unwrap()
            );
        }
    }
}

#[test]
fn unanticipated_storage_changes_are_not_adopted_as_an_attempted_prefix() {
    let access = MemoryDefaultConnection::new();
    let before = capture(&access).unwrap();
    access.foreign_after_write.set(true);
    let mut attempts = vec![before.clone()];
    assert!(
        apply(&access, &before, &NativeProxyChange::Disable, |state| {
            attempts.push(state)
        })
        .is_err()
    );
    let actual = capture(&access).unwrap();
    assert!(
        attempts
            .iter()
            .all(|known| !known.same_state(&actual).unwrap())
    );
}

#[test]
fn refresh_failure_retains_owned_values_and_requires_a_successful_restore_refresh() {
    let access = MemoryDefaultConnection::new();
    let before = capture(&access).unwrap();
    access.fail_refresh.set(true);
    let mut attempts = Vec::new();
    assert!(
        apply(
            &access,
            &before,
            &NativeProxyChange::Pac {
                url: "http://127.0.0.1:1234/pac"
            },
            |state| attempts.push(state)
        )
        .is_err()
    );
    let actual = capture(&access).unwrap();
    assert!(attempts.last().unwrap().same_state(&actual).unwrap());
    access.fail_refresh.set(false);
    assert!(
        restore(&access, &actual, &before, |_| {})
            .unwrap()
            .same_state(&before)
            .unwrap()
    );
}
