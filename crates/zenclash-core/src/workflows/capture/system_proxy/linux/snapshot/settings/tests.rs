use super::*;

#[link(name = "gio-2.0")]
unsafe extern "C" {
    fn g_memory_settings_backend_new() -> *mut c_void;
    fn g_settings_schema_source_new_from_directory(
        directory: *const c_char,
        parent: *mut c_void,
        trusted: i32,
        error: *mut *mut c_void,
    ) -> *mut c_void;
    fn g_settings_schema_source_unref(source: *mut c_void);
}
#[link(name = "glib-2.0")]
unsafe extern "C" {
    fn g_error_free(error: *mut c_void);
}

struct FixtureDirectory(std::path::PathBuf);
impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct SchemaSource(NonNull<c_void>);
impl Drop for SchemaSource {
    fn drop(&mut self) {
        unsafe { g_settings_schema_source_unref(self.0.as_ptr()) };
    }
}
static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn fixture() -> (FixtureDirectory, ProxySettingsCatalog) {
    let directory = loop {
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "zenclash-gnome-snapshot-{}-{sequence}",
            std::process::id()
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => break FixtureDirectory(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("cannot create schema fixture: {error}"),
        }
    };
    let mut xml = String::from("<schemalist>");
    for schema in SCHEMAS {
        xml.push_str(&format!(
            "<schema id='{schema}' path='/fixture/{}/'>",
            schema.replace('.', "/")
        ));
        if schema == ROOT {
            xml.push_str("<key name='mode' type='s'><default>'none'</default></key><key name='autoconfig-url' type='s'><default>''</default></key><key name='ignore-hosts' type='as'><default>[]</default></key><key name='use-same-proxy' type='b'><default>false</default></key><key name='native-extra' type='s'><default>'inherited fixture value'</default></key>");
        } else {
            xml.push_str("<key name='host' type='s'><default>''</default></key><key name='port' type='i'><default>0</default></key><key name='native-extra' type='s'><default>'inherited fixture value'</default></key>");
        }
        xml.push_str("</schema>");
    }
    xml.push_str("</schemalist>");
    std::fs::write(directory.0.join("fixture.gschema.xml"), xml).unwrap();
    let compiler = std::env::var("ZENCLASH_GLIB_SCHEMA_COMPILER")
        .unwrap_or_else(|_| "glib-compile-schemas".into());
    let compiled = std::process::Command::new(compiler)
        .arg(&directory.0)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let path = CString::new(directory.0.to_str().unwrap()).unwrap();
    let mut error = ptr::null_mut();
    let source = unsafe {
        g_settings_schema_source_new_from_directory(
            path.as_ptr(),
            ptr::null_mut(),
            0,
            &raw mut error,
        )
    };
    if !error.is_null() {
        unsafe { g_error_free(error) };
    }
    let source = SchemaSource(NonNull::new(source).unwrap());
    let backend = OwnedGObject(NonNull::new(unsafe { g_memory_settings_backend_new() }).unwrap());
    // Explicit memory storage isolates every setting from the user's desktop.
    let catalog =
        unsafe { ProxySettingsCatalog::from_source(source.0.as_ptr(), backend.0.as_ptr()) }
            .unwrap();
    (directory, catalog)
}

#[test]
fn native_manual_application_and_restore_preserve_default_layer_and_hidden_values() {
    let (_directory, catalog) = fixture();
    catalog
        .write(
            &SettingKey::new(FTP, "host"),
            string("foreign.ftp").unwrap().user.as_ref(),
        )
        .unwrap();
    let before = catalog.read().unwrap();
    let wrapped = NativeProxySnapshot::Platform(before.clone());
    let mut witness = NativeMutationWitness::new(&wrapped);
    let after = apply_with_catalog(
        &catalog,
        &before,
        &NativeProxyChange::Manual {
            host: "127.0.0.1",
            port: 7890,
            bypass: &["host,with,commas".into(), "quote'and\\slash".into()],
        },
        &mut witness,
    )
    .unwrap();
    assert!(witness.permits(&after).unwrap());
    let after = after.platform().unwrap();
    assert_eq!(
        after.value(FTP, "host").unwrap().string().unwrap(),
        "foreign.ftp"
    );
    assert_eq!(after.value(HTTP, "port").unwrap().kind, "i");
    assert_eq!(after.value(HTTP, "port").unwrap().int32().unwrap(), 7890);
    assert_eq!(after.value(HTTPS, "port").unwrap().kind, "i");
    assert_eq!(after.value(HTTPS, "port").unwrap().int32().unwrap(), 7890);
    assert!(after.summary.enabled);
    assert!(after.summary.secure_enabled);
    assert_eq!(
        after.summary.bypass,
        ["host,with,commas", "quote'and\\slash"]
    );
    let mut restore_witness =
        NativeMutationWitness::new(&NativeProxySnapshot::Platform(after.clone()));
    let restored = restore_with_catalog(&catalog, after, &before, &mut restore_witness).unwrap();
    assert!(restored.platform().unwrap().same_state(&before).unwrap());
    assert!(
        catalog.read().unwrap().values[&SettingKey::new(HTTP, "host")]
            .user
            .is_none()
    );
}

#[test]
fn typed_empty_array_and_auto_mode_with_empty_url_are_preserved() {
    let (_directory, catalog) = fixture();
    catalog
        .write(
            &SettingKey::new(ROOT, "mode"),
            string("auto").unwrap().user.as_ref(),
        )
        .unwrap();
    let before = catalog.read().unwrap();
    assert!(before.summary.auto_enabled && before.summary.auto_url.is_empty());
    assert!(before.summary.bypass.is_empty());
    let mut witness = NativeMutationWitness::new(&NativeProxySnapshot::Platform(before.clone()));
    let disabled =
        apply_with_catalog(&catalog, &before, &NativeProxyChange::Disable, &mut witness).unwrap();
    let mut restore_witness = NativeMutationWitness::new(&disabled);
    let restored = restore_with_catalog(
        &catalog,
        disabled.platform().unwrap(),
        &before,
        &mut restore_witness,
    )
    .unwrap();
    assert!(restored.platform().unwrap().same_state(&before).unwrap());
}

#[test]
fn hidden_value_replacement_rejects_application_before_any_managed_write() {
    let (_directory, catalog) = fixture();
    let before = catalog.read().unwrap();
    catalog
        .write(
            &SettingKey::new(ROOT, "native-extra"),
            string("external value").unwrap().user.as_ref(),
        )
        .unwrap();
    let external = catalog.read().unwrap();
    let mut witness = NativeMutationWitness::new(&NativeProxySnapshot::Platform(before.clone()));
    assert!(
        apply_with_catalog(
            &catalog,
            &before,
            &NativeProxyChange::Pac {
                url: "http://127.0.0.1:9999/pac"
            },
            &mut witness
        )
        .is_err()
    );
    assert!(catalog.read().unwrap().same_state(&external).unwrap());
    assert!(!witness.has_steps());
}

#[test]
fn int32_ports_validate_range_and_manual_enable_predicate() {
    let (_directory, catalog) = fixture();
    let int32_value = |port: i32| {
        captured(unsafe { OwnedVariant::owned(g_variant_new_int32(port), true) }.unwrap()).unwrap()
    };

    catalog
        .write(
            &SettingKey::new(ROOT, "mode"),
            string("manual").unwrap().user.as_ref(),
        )
        .unwrap();
    let empty_manual = catalog.read().unwrap();
    assert!(!empty_manual.summary.enabled);
    assert!(!empty_manual.summary.secure_enabled);

    catalog
        .write(
            &SettingKey::new(HTTP, "host"),
            string("127.0.0.1").unwrap().user.as_ref(),
        )
        .unwrap();
    let zero_port = catalog.read().unwrap();
    assert!(!zero_port.summary.enabled);
    assert!(!zero_port.summary.secure_enabled);

    catalog
        .write(
            &SettingKey::new(HTTP, "port"),
            int32_value(7890).user.as_ref(),
        )
        .unwrap();
    let http_only = catalog.read().unwrap();
    assert!(http_only.summary.enabled);
    assert_eq!(http_only.summary.port, 7890);
    assert!(!http_only.summary.secure_enabled);

    for invalid_port in [-1, 65536] {
        catalog
            .write(
                &SettingKey::new(HTTP, "port"),
                int32_value(invalid_port).user.as_ref(),
            )
            .unwrap();
        assert!(matches!(
            catalog.read(),
            Err(crate::MihomoError::SystemProxy(error))
                if matches!(
                    error.as_ref(),
                    crate::SystemProxyError::InvalidNativeSnapshot { .. }
                )
        ));
    }
}
