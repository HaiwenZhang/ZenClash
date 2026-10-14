//! GSettings user values and effective values form one guarded proxy snapshot.

#[cfg(target_os = "linux")]
mod settings {
    use crate::{
        MihomoResult,
        system_proxy::{
            SystemProxyStatus,
            native::{
                NativeMutationWitness, NativeProxyChange, NativeProxySnapshot, NativeProxyTarget,
                NativeWriteAttempt, invalid_snapshot, ownership_changed, unstable_observation,
                verification_failure,
            },
        },
    };
    use std::{
        collections::BTreeMap,
        ffi::{CStr, CString, c_char, c_void},
        ptr::{self, NonNull},
        sync::Arc,
    };
    const ROOT: &str = "org.gnome.system.proxy";
    const HTTP: &str = "org.gnome.system.proxy.http";
    const HTTPS: &str = "org.gnome.system.proxy.https";
    const FTP: &str = "org.gnome.system.proxy.ftp";
    const SOCKS: &str = "org.gnome.system.proxy.socks";
    const SCHEMAS: [&str; 5] = [ROOT, HTTP, HTTPS, FTP, SOCKS];
    const MAX_SNAPSHOT_BYTES: usize = 1024 * 1024;

    #[link(name = "gio-2.0")]
    unsafe extern "C" {
        fn g_settings_backend_get_default() -> *mut c_void;
        fn g_settings_schema_source_get_default() -> *mut c_void;
        fn g_settings_schema_source_lookup(
            source: *mut c_void,
            id: *const c_char,
            recursive: i32,
        ) -> *mut c_void;
        fn g_settings_schema_unref(schema: *mut c_void);
        fn g_settings_schema_list_keys(schema: *mut c_void) -> *mut *mut c_char;
        fn g_settings_new_full(
            schema: *mut c_void,
            backend: *mut c_void,
            path: *const c_char,
        ) -> *mut c_void;
        fn g_settings_get_value(settings: *mut c_void, key: *const c_char) -> *mut c_void;
        fn g_settings_get_user_value(settings: *mut c_void, key: *const c_char) -> *mut c_void;
        fn g_settings_is_writable(settings: *mut c_void, key: *const c_char) -> i32;
        fn g_settings_set_value(
            settings: *mut c_void,
            key: *const c_char,
            value: *mut c_void,
        ) -> i32;
        fn g_settings_reset(settings: *mut c_void, key: *const c_char);
        fn g_settings_sync();
    }
    #[link(name = "gobject-2.0")]
    unsafe extern "C" {
        fn g_object_unref(object: *mut c_void);
        fn g_type_name_from_instance(instance: *mut c_void) -> *const c_char;
    }
    #[link(name = "glib-2.0")]
    unsafe extern "C" {
        fn g_strfreev(value: *mut *mut c_char);
        fn g_variant_unref(value: *mut c_void);
        fn g_variant_ref_sink(value: *mut c_void) -> *mut c_void;
        fn g_variant_get_type_string(value: *mut c_void) -> *const c_char;
        fn g_variant_get_size(value: *mut c_void) -> usize;
        fn g_variant_get_data(value: *mut c_void) -> *const u8;
        fn g_variant_is_normal_form(value: *mut c_void) -> i32;
        fn g_variant_type_string_is_valid(value: *const c_char) -> i32;
        fn g_variant_type_new(value: *const c_char) -> *mut c_void;
        fn g_variant_type_free(value: *mut c_void);
        fn g_bytes_new(data: *const u8, size: usize) -> *mut c_void;
        fn g_bytes_unref(bytes: *mut c_void);
        fn g_variant_new_from_bytes(
            kind: *mut c_void,
            bytes: *mut c_void,
            trusted: i32,
        ) -> *mut c_void;
        fn g_variant_new_string(value: *const c_char) -> *mut c_void;
        fn g_variant_new_uint32(value: u32) -> *mut c_void;
        fn g_variant_new_strv(values: *const *const c_char, length: isize) -> *mut c_void;
        fn g_variant_get_string(value: *mut c_void, length: *mut usize) -> *const c_char;
        fn g_variant_get_uint32(value: *mut c_void) -> u32;
        fn g_variant_get_boolean(value: *mut c_void) -> i32;
        fn g_variant_n_children(value: *mut c_void) -> usize;
        fn g_variant_get_child_value(value: *mut c_void, index: usize) -> *mut c_void;
    }
    struct OwnedGObject(NonNull<c_void>);
    impl Drop for OwnedGObject {
        fn drop(&mut self) {
            unsafe { g_object_unref(self.0.as_ptr()) };
        }
    }
    struct OwnedSchema(NonNull<c_void>);
    impl Drop for OwnedSchema {
        fn drop(&mut self) {
            unsafe { g_settings_schema_unref(self.0.as_ptr()) };
        }
    }
    struct OwnedStringVector(*mut *mut c_char);
    impl Drop for OwnedStringVector {
        fn drop(&mut self) {
            unsafe { g_strfreev(self.0) };
        }
    }
    struct OwnedVariant(NonNull<c_void>);
    impl Drop for OwnedVariant {
        fn drop(&mut self) {
            unsafe { g_variant_unref(self.0.as_ptr()) };
        }
    }
    impl OwnedVariant {
        // SAFETY: Consume one full reference, or sink one newly created floating ref.
        unsafe fn owned(value: *mut c_void, floating: bool) -> MihomoResult<Self> {
            let value = NonNull::new(value).ok_or_else(invalid_snapshot)?;
            if floating {
                unsafe { g_variant_ref_sink(value.as_ptr()) };
            }
            Ok(Self(value))
        }
        fn capture(&self, budget: &mut usize) -> MihomoResult<SerializedVariant> {
            let size = unsafe { g_variant_get_size(self.0.as_ptr()) };
            if size > *budget || unsafe { g_variant_is_normal_form(self.0.as_ptr()) } == 0 {
                return Err(invalid_snapshot());
            }
            let kind = unsafe { CStr::from_ptr(g_variant_get_type_string(self.0.as_ptr())) }
                .to_str()
                .map_err(|_| invalid_snapshot())?
                .to_owned();
            let bytes = if size == 0 {
                Arc::from([])
            } else {
                let data = unsafe { g_variant_get_data(self.0.as_ptr()) };
                if data.is_null() {
                    return Err(invalid_snapshot());
                }
                Arc::from(unsafe { std::slice::from_raw_parts(data, size) })
            };
            *budget -= size;
            Ok(SerializedVariant { kind, bytes })
        }
        fn new_string(value: &str) -> MihomoResult<Self> {
            let value = CString::new(value).map_err(|_| invalid_snapshot())?;
            unsafe { Self::owned(g_variant_new_string(value.as_ptr()), true) }
        }
    }
    #[derive(Clone, PartialEq, Eq)]
    struct SerializedVariant {
        kind: String,
        bytes: Arc<[u8]>,
    }
    impl SerializedVariant {
        fn native(&self) -> MihomoResult<OwnedVariant> {
            let kind = CString::new(self.kind.as_str()).map_err(|_| invalid_snapshot())?;
            if unsafe { g_variant_type_string_is_valid(kind.as_ptr()) } == 0 {
                return Err(invalid_snapshot());
            }
            let kind = unsafe { g_variant_type_new(kind.as_ptr()) };
            if kind.is_null() {
                return Err(invalid_snapshot());
            }
            // GBytes copies and aligns the serialized buffer; the variant retains it.
            let bytes = unsafe { g_bytes_new(self.bytes.as_ptr(), self.bytes.len()) };
            if bytes.is_null() {
                unsafe { g_variant_type_free(kind) };
                return Err(invalid_snapshot());
            }
            let value = unsafe { g_variant_new_from_bytes(kind, bytes, 0) };
            unsafe {
                g_bytes_unref(bytes);
                g_variant_type_free(kind);
            }
            let value = unsafe { OwnedVariant::owned(value, true) }?;
            if unsafe { g_variant_is_normal_form(value.0.as_ptr()) } == 0 {
                return Err(invalid_snapshot());
            }
            Ok(value)
        }
        fn string(&self) -> MihomoResult<String> {
            if self.kind != "s" {
                return Err(invalid_snapshot());
            }
            let value = self.native()?;
            let mut length = 0;
            let bytes = unsafe { g_variant_get_string(value.0.as_ptr(), &raw mut length) };
            let bytes = unsafe { std::slice::from_raw_parts(bytes.cast(), length) };
            std::str::from_utf8(bytes)
                .map(str::to_owned)
                .map_err(|_| invalid_snapshot())
        }
        fn uint32(&self) -> MihomoResult<u32> {
            if self.kind != "u" {
                return Err(invalid_snapshot());
            }
            let value = self.native()?;
            Ok(unsafe { g_variant_get_uint32(value.0.as_ptr()) })
        }
        fn boolean(&self) -> MihomoResult<bool> {
            if self.kind != "b" {
                return Err(invalid_snapshot());
            }
            let value = self.native()?;
            Ok(unsafe { g_variant_get_boolean(value.0.as_ptr()) } != 0)
        }
        fn strings(&self) -> MihomoResult<Vec<String>> {
            if self.kind != "as" {
                return Err(invalid_snapshot());
            }
            let value = self.native()?;
            let count = unsafe { g_variant_n_children(value.0.as_ptr()) };
            (0..count)
                .map(|index| {
                    let child = unsafe {
                        OwnedVariant::owned(
                            g_variant_get_child_value(value.0.as_ptr(), index),
                            false,
                        )
                    }?;
                    let mut budget = MAX_SNAPSHOT_BYTES;
                    child.capture(&mut budget)?.string()
                })
                .collect()
        }
    }
    #[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
    struct SettingKey {
        schema: String,
        key: String,
    }
    impl SettingKey {
        fn new(schema: &str, key: &str) -> Self {
            Self {
                schema: schema.into(),
                key: key.into(),
            }
        }
    }
    #[derive(Clone, PartialEq, Eq)]
    struct SettingValue {
        effective: SerializedVariant,
        user: Option<SerializedVariant>,
    }
    struct ProxySettingsCatalog {
        schemas: BTreeMap<String, (OwnedGObject, Vec<String>)>,
    }
    impl ProxySettingsCatalog {
        fn open() -> MihomoResult<Self> {
            let backend = OwnedGObject(
                NonNull::new(unsafe { g_settings_backend_get_default() })
                    .ok_or_else(invalid_snapshot)?,
            );
            let name = unsafe { g_type_name_from_instance(backend.0.as_ptr()) };
            if name.is_null()
                || matches!(
                    unsafe { CStr::from_ptr(name) }.to_bytes(),
                    b"GMemorySettingsBackend" | b"GNullSettingsBackend"
                )
            {
                return Err(crate::SystemProxyError::BackendUnavailable {
                    backend: crate::SystemProxyBackendKind::Gnome,
                    reason: crate::SystemProxyBackendUnavailable::NoPersistentSettings,
                }
                .into());
            }
            let source = unsafe { g_settings_schema_source_get_default() };
            // SAFETY: The default schema source is borrowed for this call, and
            // the owned backend remains live. GSettings retains both dependencies.
            unsafe { Self::from_source(source, backend.0.as_ptr()) }
        }

        // SAFETY: source and backend are valid native references for this call.
        unsafe fn from_source(source: *mut c_void, backend: *mut c_void) -> MihomoResult<Self> {
            if source.is_null() {
                return Err(crate::SystemProxyError::BackendUnavailable {
                    backend: crate::SystemProxyBackendKind::Gnome,
                    reason: crate::SystemProxyBackendUnavailable::NoSchemas,
                }
                .into());
            }
            let mut schemas = BTreeMap::new();
            for id in SCHEMAS {
                let name = CString::new(id).map_err(|_| invalid_snapshot())?;
                let schema = unsafe { g_settings_schema_source_lookup(source, name.as_ptr(), 1) };
                let schema = OwnedSchema(NonNull::new(schema).ok_or_else(|| {
                    crate::MihomoError::from(crate::SystemProxyError::BackendUnavailable {
                        backend: crate::SystemProxyBackendKind::Gnome,
                        reason: crate::SystemProxyBackendUnavailable::MissingSchema,
                    })
                })?);
                let object = OwnedGObject(
                    NonNull::new(unsafe {
                        g_settings_new_full(schema.0.as_ptr(), backend, ptr::null())
                    })
                    .ok_or_else(invalid_snapshot)?,
                );
                let keys =
                    OwnedStringVector(unsafe { g_settings_schema_list_keys(schema.0.as_ptr()) });
                if keys.0.is_null() {
                    return Err(invalid_snapshot());
                }
                let mut names = Vec::new();
                for index in 0..4096 {
                    let key = unsafe { *keys.0.add(index) };
                    if key.is_null() {
                        break;
                    }
                    names.push(
                        unsafe { CStr::from_ptr(key) }
                            .to_str()
                            .map_err(|_| invalid_snapshot())?
                            .to_owned(),
                    );
                }
                if names.len() == 4096 {
                    return Err(invalid_snapshot());
                }
                schemas.insert(id.into(), (object, names));
            }
            Ok(Self { schemas })
        }
        fn read(&self) -> MihomoResult<ProxySnapshot> {
            let mut values = BTreeMap::new();
            let mut budget = MAX_SNAPSHOT_BYTES;
            for (schema, (settings, keys)) in &self.schemas {
                for name in keys {
                    let key = CString::new(name.as_str()).map_err(|_| invalid_snapshot())?;
                    let effective = unsafe {
                        OwnedVariant::owned(
                            g_settings_get_value(settings.0.as_ptr(), key.as_ptr()),
                            false,
                        )
                    }?
                    .capture(&mut budget)?;
                    let user =
                        unsafe { g_settings_get_user_value(settings.0.as_ptr(), key.as_ptr()) };
                    let user = if user.is_null() {
                        None
                    } else {
                        Some(unsafe { OwnedVariant::owned(user, false) }?.capture(&mut budget)?)
                    };
                    values.insert(
                        SettingKey::new(schema, name),
                        SettingValue { effective, user },
                    );
                }
            }
            ProxySnapshot::from_values(values)
        }
        fn write(&self, key: &SettingKey, user: Option<&SerializedVariant>) -> MihomoResult<()> {
            let (settings, keys) = self.schemas.get(&key.schema).ok_or_else(invalid_snapshot)?;
            if !keys.contains(&key.key) {
                return Err(invalid_snapshot());
            }
            let key = CString::new(key.key.as_str()).map_err(|_| invalid_snapshot())?;
            if unsafe { g_settings_is_writable(settings.0.as_ptr(), key.as_ptr()) } == 0 {
                return Err(crate::SystemProxyError::BackendUnavailable {
                    backend: crate::SystemProxyBackendKind::Gnome,
                    reason: crate::SystemProxyBackendUnavailable::LockedSetting,
                }
                .into());
            }
            if let Some(user) = user {
                let user = user.native()?;
                if unsafe {
                    g_settings_set_value(settings.0.as_ptr(), key.as_ptr(), user.0.as_ptr())
                } == 0
                {
                    return Err(crate::SystemProxyError::NativeOperationRejected {
                        backend: crate::SystemProxyBackendKind::Gnome,
                        operation: "g_settings_set_value",
                    }
                    .into());
                }
            } else {
                unsafe { g_settings_reset(settings.0.as_ptr(), key.as_ptr()) };
            }
            unsafe { g_settings_sync() };
            Ok(())
        }
    }
    #[derive(Clone)]
    pub(in crate::system_proxy) struct ProxySnapshot {
        values: BTreeMap<SettingKey, SettingValue>,
        summary: SystemProxyStatus,
    }
    impl ProxySnapshot {
        pub(in crate::system_proxy) fn summary(&self) -> &SystemProxyStatus {
            &self.summary
        }
        pub(in crate::system_proxy) fn same_target(&self, _other: &Self) -> bool {
            true
        }
        pub(in crate::system_proxy) fn same_state(&self, other: &Self) -> MihomoResult<bool> {
            Ok(self.values == other.values)
        }
        pub(in crate::system_proxy) fn legacy_release_is_safe(&self) -> bool {
            let same = self
                .value(ROOT, "use-same-proxy")
                .and_then(SerializedVariant::boolean)
                .unwrap_or(false);
            same || [FTP, SOCKS].iter().all(|schema| {
                self.value(schema, "host")
                    .and_then(SerializedVariant::string)
                    .is_ok_and(|host| host.is_empty())
            })
        }
        fn value(&self, schema: &str, key: &str) -> MihomoResult<&SerializedVariant> {
            self.values
                .get(&SettingKey::new(schema, key))
                .map(|value| &value.effective)
                .ok_or_else(invalid_snapshot)
        }
        fn from_values(values: BTreeMap<SettingKey, SettingValue>) -> MihomoResult<Self> {
            let mut snapshot = Self {
                values,
                summary: SystemProxyStatus {
                    service: "GNOME".into(),
                    ..Default::default()
                },
            };
            let mode = snapshot.value(ROOT, "mode")?.string()?;
            snapshot.summary = SystemProxyStatus {
                service: "GNOME".into(),
                enabled: mode == "manual",
                secure_enabled: mode == "manual",
                server: snapshot.value(HTTP, "host")?.string()?,
                port: u16::try_from(snapshot.value(HTTP, "port")?.uint32()?)
                    .map_err(|_| invalid_snapshot())?,
                secure_server: snapshot.value(HTTPS, "host")?.string()?,
                secure_port: u16::try_from(snapshot.value(HTTPS, "port")?.uint32()?)
                    .map_err(|_| invalid_snapshot())?,
                bypass: snapshot.value(ROOT, "ignore-hosts")?.strings()?,
                auto_enabled: mode == "auto",
                auto_url: snapshot.value(ROOT, "autoconfig-url")?.string()?,
            };
            Ok(snapshot)
        }
    }
    pub(in crate::system_proxy) fn capture(
        target: NativeProxyTarget<'_>,
    ) -> MihomoResult<NativeProxySnapshot> {
        match target {
            NativeProxyTarget::Service("GNOME") => {}
            NativeProxyTarget::Captured(snapshot) => {
                snapshot.platform()?;
            }
            _ => return Err(ownership_changed()),
        }
        let catalog = ProxySettingsCatalog::open()?;
        let first = catalog.read()?;
        let second = catalog.read()?;
        if !first.same_state(&second)? {
            return Err(unstable_observation());
        }
        Ok(NativeProxySnapshot::Platform(second))
    }
    fn captured(value: OwnedVariant) -> MihomoResult<SettingValue> {
        let mut budget = MAX_SNAPSHOT_BYTES;
        let effective = value.capture(&mut budget)?;
        Ok(SettingValue {
            user: Some(effective.clone()),
            effective,
        })
    }
    fn string(value: &str) -> MihomoResult<SettingValue> {
        captured(OwnedVariant::new_string(value)?)
    }
    pub(in crate::system_proxy) fn apply(
        expected: &NativeProxySnapshot,
        change: &NativeProxyChange<'_>,
    ) -> NativeWriteAttempt {
        NativeWriteAttempt::run(expected, |witness| {
            let catalog = ProxySettingsCatalog::open()?;
            apply_with_catalog(&catalog, expected.platform()?, change, witness)
        })
    }

    fn apply_with_catalog(
        catalog: &ProxySettingsCatalog,
        before: &ProxySnapshot,
        change: &NativeProxyChange<'_>,
        witness: &mut NativeMutationWitness,
    ) -> MihomoResult<NativeProxySnapshot> {
        let mut steps = vec![(SettingKey::new(ROOT, "mode"), string("none")?)];
        match change {
            NativeProxyChange::Disable => {}
            NativeProxyChange::Pac { url } => {
                steps.push((SettingKey::new(ROOT, "autoconfig-url"), string(url)?));
                steps.push((SettingKey::new(ROOT, "mode"), string("auto")?));
            }
            NativeProxyChange::Manual { host, port, bypass } => {
                for schema in [HTTP, HTTPS] {
                    steps.push((SettingKey::new(schema, "host"), string(host)?));
                    steps.push((
                        SettingKey::new(schema, "port"),
                        captured(unsafe {
                            OwnedVariant::owned(g_variant_new_uint32(u32::from(*port)), true)
                        }?)?,
                    ));
                }
                let strings = bypass
                    .iter()
                    .map(|value| CString::new(value.as_str()).map_err(|_| invalid_snapshot()))
                    .collect::<MihomoResult<Vec<_>>>()?;
                let pointers = strings
                    .iter()
                    .map(|value| value.as_ptr())
                    .collect::<Vec<_>>();
                let bypass = captured(unsafe {
                    OwnedVariant::owned(
                        g_variant_new_strv(pointers.as_ptr(), pointers.len() as isize),
                        true,
                    )
                }?)?;
                steps.push((SettingKey::new(ROOT, "ignore-hosts"), bypass));
                steps.push((SettingKey::new(ROOT, "mode"), string("manual")?));
            }
        }
        write_steps(catalog, before, steps, witness)
    }
    pub(in crate::system_proxy) fn restore(
        expected: &NativeProxySnapshot,
        previous: &NativeProxySnapshot,
    ) -> NativeWriteAttempt {
        NativeWriteAttempt::run(expected, |witness| {
            let catalog = ProxySettingsCatalog::open()?;
            restore_with_catalog(
                &catalog,
                expected.platform()?,
                previous.platform()?,
                witness,
            )
        })
    }

    fn restore_with_catalog(
        catalog: &ProxySettingsCatalog,
        before: &ProxySnapshot,
        previous: &ProxySnapshot,
        witness: &mut NativeMutationWitness,
    ) -> MihomoResult<NativeProxySnapshot> {
        let mut steps = vec![(SettingKey::new(ROOT, "mode"), string("none")?)];
        for (schema, key) in [
            (HTTP, "host"),
            (HTTP, "port"),
            (HTTPS, "host"),
            (HTTPS, "port"),
            (ROOT, "ignore-hosts"),
            (ROOT, "autoconfig-url"),
            (ROOT, "mode"),
        ] {
            let key = SettingKey::new(schema, key);
            let value = previous
                .values
                .get(&key)
                .ok_or_else(invalid_snapshot)?
                .clone();
            steps.push((key, value));
        }
        write_steps(catalog, before, steps, witness)
    }
    fn write_steps(
        catalog: &ProxySettingsCatalog,
        before: &ProxySnapshot,
        steps: Vec<(SettingKey, SettingValue)>,
        witness: &mut NativeMutationWitness,
    ) -> MihomoResult<NativeProxySnapshot> {
        let mut current = before.clone();
        for (key, value) in steps {
            if !catalog.read()?.same_state(&current)? {
                return Err(ownership_changed());
            }
            if current.values.get(&key) == Some(&value) {
                continue;
            }
            let mut next = current.values.clone();
            next.insert(key.clone(), value.clone());
            let next = ProxySnapshot::from_values(next)?;
            witness.record(NativeProxySnapshot::Platform(next.clone()));
            catalog.write(&key, value.user.as_ref())?;
            if !catalog.read()?.same_state(&next)? {
                return Err(verification_failure());
            }
            current = next;
        }
        unsafe { g_settings_sync() };
        let verified = catalog.read()?;
        if !verified.same_state(&current)? {
            return Err(verification_failure());
        }
        Ok(NativeProxySnapshot::Platform(verified))
    }
    #[cfg(test)]
    mod tests {
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
                    xml.push_str("<key name='host' type='s'><default>''</default></key><key name='port' type='u'><default>0</default></key><key name='native-extra' type='s'><default>'inherited fixture value'</default></key>");
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
            let backend =
                OwnedGObject(NonNull::new(unsafe { g_memory_settings_backend_new() }).unwrap());
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
            assert_eq!(
                after.summary.bypass,
                ["host,with,commas", "quote'and\\slash"]
            );
            let mut restore_witness =
                NativeMutationWitness::new(&NativeProxySnapshot::Platform(after.clone()));
            let restored =
                restore_with_catalog(&catalog, after, &before, &mut restore_witness).unwrap();
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
            let mut witness =
                NativeMutationWitness::new(&NativeProxySnapshot::Platform(before.clone()));
            let disabled =
                apply_with_catalog(&catalog, &before, &NativeProxyChange::Disable, &mut witness)
                    .unwrap();
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
            let mut witness =
                NativeMutationWitness::new(&NativeProxySnapshot::Platform(before.clone()));
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
    }
}
#[cfg(target_os = "linux")]
pub(in crate::system_proxy) use settings::{ProxySnapshot, apply, capture, restore};
