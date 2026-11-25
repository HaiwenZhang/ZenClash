use super::*;
use std::cell::{Cell, RefCell};

struct MemoryRegistry {
    values: RefCell<InternetSettingsRegistryValues>,
    events: RefCell<Vec<String>>,
    write_attempts: Cell<usize>,
    fail_write_at: Cell<Option<usize>>,
    fail_refresh: Cell<bool>,
    external_after_write: Cell<Option<usize>>,
}

impl MemoryRegistry {
    fn new(values: InternetSettingsRegistryValues) -> Self {
        Self {
            values: RefCell::new(values),
            events: RefCell::default(),
            write_attempts: Cell::new(0),
            fail_write_at: Cell::new(None),
            fail_refresh: Cell::new(false),
            external_after_write: Cell::new(None),
        }
    }
    fn failure(operation: &'static str) -> crate::MihomoError {
        crate::SystemProxyError::Platform {
            backend: crate::SystemProxyBackendKind::WinInet,
            operation,
            code: 5,
        }
        .into()
    }
}

impl RegistryAccess for MemoryRegistry {
    fn read(&self) -> MihomoResult<InternetSettingsRegistryValues> {
        Ok(self.values.borrow().clone())
    }
    fn write(&self, name: &str, value: Option<&RawRegistryValue>) -> MihomoResult<()> {
        self.events.borrow_mut().push(name.into());
        let attempt = self.write_attempts.get() + 1;
        self.write_attempts.set(attempt);
        if self.fail_write_at.get() == Some(attempt) {
            return Err(Self::failure("RegSetValueExW fixture"));
        }
        let mut values = self.values.borrow_mut();
        let key = ascii_folded_value_name(&utf16_units(name));
        if let Some(value) = value {
            values.insert(key, value.clone());
        } else {
            values.remove(&key);
        }
        if self.external_after_write.get() == Some(attempt) {
            values.insert(
                ascii_folded_value_name(&utf16_units("Unmanaged")),
                registry_string_value("Unmanaged", "external replacement"),
            );
        }
        Ok(())
    }
    fn refresh(&self) -> MihomoResult<()> {
        self.events.borrow_mut().push("refresh".into());
        if self.fail_refresh.get() {
            Err(Self::failure("InternetSetOptionW fixture"))
        } else {
            Ok(())
        }
    }
}

fn original_values() -> InternetSettingsRegistryValues {
    let mut values = InternetSettingsRegistryValues::new();
    let mut server = registry_string_value(
        "pRoXySeRvEr",
        "http=old:80;https=old:80;ftp=ftp:21;socks=socks:1080",
    );
    server.kind = REG_EXPAND_SZ_KIND;
    values.insert(ascii_folded_value_name(&server.name), server);
    values.insert(
        ascii_folded_value_name(&utf16_units("Unmanaged")),
        RawRegistryValue {
            name: utf16_units("Unmanaged"),
            kind: 3,
            bytes: vec![1, 2, 0, 255].into(),
        },
    );
    values
}

#[test]
fn manual_mutation_guards_complete_state_and_preserves_unmanaged_values() {
    let access = MemoryRegistry::new(original_values());
    let before = capture(&access).unwrap();
    let mut attempted = Vec::new();
    let after = apply(
        &access,
        &before,
        &NativeProxyChange::Manual {
            host: "new",
            port: 7890,
            bypass: &["localhost".into()],
        },
        |state| attempted.push(state),
    )
    .unwrap();
    assert_eq!(
        access.events.into_inner(),
        [
            "ProxyEnable",
            "AutoConfigURL",
            "ProxyServer",
            "ProxyOverride",
            "ProxyEnable",
            "refresh"
        ]
    );
    assert_eq!(attempted.len(), 5);
    assert!(after.same_target(&before));
    assert!(after.summary().enabled && after.summary().secure_enabled);
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
    assert!(!after.legacy_release_is_safe());
}

#[test]
fn restore_replays_original_types_names_and_absence() {
    let access = MemoryRegistry::new(original_values());
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
    let restored = restore(&access, &after, &before, |_| {}).unwrap();
    assert!(restored.same_state(&before).unwrap());
    for name in ["ProxyEnable", "ProxyOverride", "AutoConfigURL"] {
        assert!(
            !restored
                .values
                .contains_key(&ascii_folded_value_name(&utf16_units(name)))
        );
    }
    let server = restored
        .values
        .get(&ascii_folded_value_name(&utf16_units("ProxyServer")))
        .unwrap();
    assert_eq!(server.kind, REG_EXPAND_SZ_KIND);
    assert_eq!(server.name, utf16_units("pRoXySeRvEr"));
}

#[test]
fn every_write_failure_records_the_attempted_prefix_and_allows_verified_restore() {
    for failed_step in 1..=5 {
        let access = MemoryRegistry::new(original_values());
        let before = capture(&access).unwrap();
        access.fail_write_at.set(Some(failed_step));
        let mut attempted = Vec::new();
        let result = apply(
            &access,
            &before,
            &NativeProxyChange::Manual {
                host: "new",
                port: 7890,
                bypass: &["localhost".into()],
            },
            |state| attempted.push(state),
        );
        assert!(result.is_err());
        assert_eq!(attempted.len(), failed_step);
        let actual = capture(&access).unwrap();
        assert!(
            before.same_state(&actual).unwrap()
                || attempted
                    .iter()
                    .any(|state| state.same_state(&actual).unwrap())
        );
        access.fail_write_at.set(None);
        let restored = restore(&access, &actual, &before, |_| {}).unwrap();
        assert!(restored.same_state(&before).unwrap());
        assert_eq!(
            access.events.borrow().last().map(String::as_str),
            Some("refresh")
        );
    }
}

#[test]
fn every_restore_failure_retains_a_known_prefix_for_a_guarded_retry() {
    for failed_step in 1..=5 {
        let access = MemoryRegistry::new(original_values());
        let previous = capture(&access).unwrap();
        let current = apply(
            &access,
            &previous,
            &NativeProxyChange::Pac {
                url: "http://127.0.0.1:1234/pac",
            },
            |_| {},
        )
        .unwrap();
        access.write_attempts.set(0);
        access.fail_write_at.set(Some(failed_step));
        let mut attempted = Vec::new();
        assert!(restore(&access, &current, &previous, |state| attempted.push(state)).is_err());
        assert_eq!(attempted.len(), failed_step);
        let actual = capture(&access).unwrap();
        assert!(
            actual.same_state(&current).unwrap()
                || attempted
                    .iter()
                    .any(|state| state.same_state(&actual).unwrap())
        );
        access.fail_write_at.set(None);
        assert!(
            restore(&access, &actual, &previous, |_| {})
                .unwrap()
                .same_state(&previous)
                .unwrap()
        );
    }
}

#[test]
fn hidden_field_replacement_before_write_rejects_without_recording_or_mutating() {
    let access = MemoryRegistry::new(original_values());
    let before = capture(&access).unwrap();
    access.values.borrow_mut().insert(
        ascii_folded_value_name(&utf16_units("Unmanaged")),
        registry_string_value("Unmanaged", "external replacement"),
    );
    let mut attempted = Vec::new();
    let result = apply(&access, &before, &NativeProxyChange::Disable, |state| {
        attempted.push(state)
    });
    assert!(matches!(result, Err(crate::MihomoError::SystemProxy(error))
        if matches!(error.as_ref(), crate::SystemProxyError::OwnershipChanged)));
    assert!(attempted.is_empty() && access.events.borrow().is_empty());
}

#[test]
fn concurrent_hidden_edit_after_write_is_not_an_authorized_snapshot() {
    let access = MemoryRegistry::new(original_values());
    let before = capture(&access).unwrap();
    access.external_after_write.set(Some(1));
    let mut attempted = Vec::new();
    let result = apply(&access, &before, &NativeProxyChange::Disable, |state| {
        attempted.push(state)
    });
    assert!(matches!(result, Err(crate::MihomoError::SystemProxy(error))
        if matches!(error.as_ref(), crate::SystemProxyError::VerificationFailed)));
    let actual = capture(&access).unwrap();
    assert!(!before.same_state(&actual).unwrap());
    assert!(
        attempted
            .iter()
            .all(|state| !state.same_state(&actual).unwrap())
    );
}

#[test]
fn refresh_failure_is_not_success_even_when_stored_values_match() {
    let access = MemoryRegistry::new(original_values());
    let before = capture(&access).unwrap();
    access.fail_refresh.set(true);
    let mut attempted = Vec::new();
    assert!(
        apply(
            &access,
            &before,
            &NativeProxyChange::Pac {
                url: "http://127.0.0.1:1234/pac"
            },
            |state| attempted.push(state)
        )
        .is_err()
    );
    let actual = capture(&access).unwrap();
    assert!(attempted.last().unwrap().same_state(&actual).unwrap());
    assert_eq!(
        access.events.borrow().last().map(String::as_str),
        Some("refresh")
    );
    access.fail_refresh.set(false);
    assert!(
        restore(&access, &actual, &before, |_| {})
            .unwrap()
            .same_state(&before)
            .unwrap()
    );
}
