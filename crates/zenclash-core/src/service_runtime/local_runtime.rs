//! Ordinary-user recovery files; privileged services never write this directory.

use std::{fs, path::Component};

use super::*;

impl ServiceRuntimeBundle {
    // Caller holds the runtime/store gates and has confirmed the Local child stopped.
    pub(crate) async fn export_local_caches(
        self: &Arc<Self>,
        store: &crate::ControlledConfigStore,
        active_config: PathBuf,
    ) -> MihomoResult<Arc<Self>> {
        let root = store.root().join("local-runtime");
        let bundle = self.clone();
        tokio::task::spawn_blocking(move || export_caches(&bundle, &root, &active_config))
            .await
            .map_err(|_| invalid("Local cache export worker failed"))?
            .map(Arc::new)
    }

    /// Materializes a TUN-disabled recovery configuration and its held resources.
    /// The alternate slot is replaced only after preparation succeeds. Source
    /// files and the original home are untouched, including GeoData activation.
    ///
    /// # Errors
    /// Rejects invalid active paths, linked directories, invalid resources, or I/O failures.
    pub async fn materialize_local_runtime(
        self: &Arc<Self>,
        store: &crate::ControlledConfigStore,
        active_config: Option<PathBuf>,
    ) -> MihomoResult<PathBuf> {
        self.materialize_local_runtime_prepared(store, active_config)
            .await
            .map(|(path, _)| path)
    }

    pub(crate) async fn materialize_local_runtime_prepared(
        self: &Arc<Self>,
        store: &crate::ControlledConfigStore,
        active_config: Option<PathBuf>,
    ) -> MihomoResult<(PathBuf, String)> {
        let lease = store
            .acquire_write_lease()
            .await
            .map_err(|error| invalid(&error.to_string()))?;
        let mutation = store.lock_service_tun_mutation().await;
        self.materialize_local_runtime_admitted(store, active_config, lease, mutation)
            .await
            .map(|(prepared, _lease, _mutation)| prepared)
    }

    pub(crate) async fn materialize_local_runtime_admitted(
        self: &Arc<Self>,
        store: &crate::ControlledConfigStore,
        active_config: Option<PathBuf>,
        lease: crate::data_coordinator::DataWriteLease,
        mutation: tokio::sync::OwnedMutexGuard<()>,
    ) -> MihomoResult<(
        (PathBuf, String),
        crate::data_coordinator::DataWriteLease,
        tokio::sync::OwnedMutexGuard<()>,
    )> {
        if !store.owns_service_tun_mutation(&mutation) {
            return Err(invalid("Local recovery has another store mutation gate"));
        }
        if !lease.covers(store.root()) {
            return Err(invalid("Local recovery lease does not cover its store"));
        }
        let bundle = self.clone();
        let root = store.root().join("local-runtime");
        tokio::spawn(async move {
            let prepared = tokio::task::spawn_blocking(move || {
                materialize(&bundle, &root, active_config.as_deref())
            })
            .await
            .map_err(|_| invalid("Local recovery worker failed"))??;
            Ok((prepared, lease, mutation))
        })
        .await
        .map_err(|_| invalid("Local recovery completion failed"))?
    }
}

fn export_caches(
    bundle: &ServiceRuntimeBundle,
    root: &Path,
    active: &Path,
) -> MihomoResult<ServiceRuntimeBundle> {
    if active != root.join("slot0/runtime.yaml") && active != root.join("slot1/runtime.yaml") {
        return Err(invalid("Local cache export is outside its recovery slots"));
    }
    let slot = active
        .parent()
        .ok_or_else(|| invalid("Missing recovery slot"))?;
    check_ancestors(slot)?;
    let providers = bundle.cache_providers()?;
    let mut exported = bundle.cache_export_base(&providers);
    let mut references = BTreeMap::new();
    for asset in &bundle.assets {
        validate_asset_path(&asset.path)?;
        references.insert(
            slot.join(&asset.path)
                .to_str()
                .ok_or_else(|| invalid("Invalid cache path"))?
                .to_owned(),
            PathBuf::from(&asset.path),
        );
    }
    for provider in providers {
        validate_asset_path(&provider.path)?;
        let path = slot.join(&provider.path);
        check_ancestors(
            path.parent()
                .ok_or_else(|| invalid("Missing cache parent"))?,
        )?;
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !linked(&metadata) => metadata,
            Ok(_) => return Err(invalid("Local provider cache is linked or not a file")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                exported.replace_provider_cache(&provider, None)?;
                continue;
            }
            Err(error) => return Err(io_error(error)),
        };
        let used = exported
            .assets
            .iter()
            .map(|asset| asset.bytes.len())
            .sum::<usize>();
        let remaining = MAX_TOTAL_BYTES.saturating_sub(used) as u64;
        if metadata.len() > remaining.min(MAX_ASSET_BYTES) {
            return Err(invalid("Local provider cache exceeds its byte budget"));
        }
        let mut bytes = read_asset_with_limit(&path, remaining).map_err(io_error)?;
        if provider.kind == zenclash_service::ProviderKind::Proxy {
            let mut value: Value = serde_yaml::from_slice(&bytes)
                .map_err(|_| invalid("Invalid local proxy provider YAML"))?;
            rewrite_references(&mut value, &references, 0)?;
            bytes = serde_yaml::to_string(&value)
                .map_err(|_| invalid("Cannot encode local proxy provider YAML"))?
                .into_bytes();
            if bytes.len() as u64 > MAX_ASSET_BYTES {
                return Err(invalid("Rewritten local cache exceeds its byte budget"));
            }
        }
        exported.replace_provider_cache(&provider, Some(bytes))?;
    }
    Ok(exported)
}

fn materialize(
    bundle: &ServiceRuntimeBundle,
    root: &Path,
    active: Option<&Path>,
) -> MihomoResult<(PathBuf, String)> {
    if !root.is_absolute()
        || root
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(invalid("Local recovery root must be absolute"));
    }
    let slot = match active {
        None => "slot0",
        Some(path) if path == root.join("slot0/runtime.yaml") => "slot1",
        Some(path) if path == root.join("slot1/runtime.yaml") => "slot0",
        Some(_) => {
            return Err(invalid(
                "Active recovery configuration is outside its slots",
            ));
        }
    };
    check_ancestors(root)?;
    fs::create_dir_all(root).map_err(io_error)?;
    check_ancestors(root)?;
    let target = root.join(slot);
    let pending = root.join(format!(".{slot}.pending"));
    let previous = root.join(format!(".{slot}.previous"));
    remove_owned_tree(&pending)?;
    remove_owned_tree(&previous)?;
    check_tree(&target, 0, &mut 0)?;
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder.create(&pending).map_err(io_error)?;
    let yaml = match write_bundle(bundle, &pending, &target) {
        Ok(yaml) => yaml,
        Err(error) => {
            remove_owned_tree(&pending)?;
            return Err(error);
        }
    };
    let had_previous = target.exists();
    if had_previous {
        fs::rename(&target, &previous).map_err(io_error)?;
    }
    if let Err(error) = fs::rename(&pending, &target) {
        if had_previous {
            fs::rename(&previous, &target).map_err(io_error)?;
        }
        remove_owned_tree(&pending)?;
        return Err(io_error(error));
    }
    remove_owned_tree(&previous)?;
    Ok((target.join("runtime.yaml"), yaml))
}

fn write_bundle(
    bundle: &ServiceRuntimeBundle,
    pending: &Path,
    target: &Path,
) -> MihomoResult<String> {
    if bundle.assets.len() > MAX_ASSETS {
        return Err(invalid("Local recovery resource count exceeds its budget"));
    }
    let mut value: Value =
        serde_yaml::from_str(&bundle.yaml).map_err(|_| invalid("Invalid accepted YAML"))?;
    let proxy_paths: Vec<_> = value
        .get("proxy-providers")
        .and_then(Value::as_mapping)
        .into_iter()
        .flat_map(|mapping| mapping.values())
        .filter_map(|provider| {
            provider
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    let mut destinations = BTreeMap::new();
    let mut total = 0usize;
    for asset in &bundle.assets {
        validate_asset_path(&asset.path)?;
        total = total
            .checked_add(asset.bytes.len())
            .filter(|total| *total <= MAX_TOTAL_BYTES)
            .ok_or_else(|| invalid("Local recovery resources exceed their budget"))?;
        if asset.bytes.len() as u64 > MAX_ASSET_BYTES {
            return Err(invalid("Local recovery resource exceeds its byte budget"));
        }
        destinations.insert(asset.path.clone(), target.join(&asset.path));
    }
    // Missing HTTP caches still need an absolute writable destination in this slot.
    for provider in bundle.cache_providers()? {
        validate_asset_path(&provider.path)?;
        destinations.insert(provider.path.clone(), target.join(provider.path));
    }
    let mut written_total = 0usize;
    for asset in &bundle.assets {
        let bytes = if proxy_paths.contains(&asset.path) {
            let mut provider: Value = serde_yaml::from_slice(&asset.bytes)
                .map_err(|_| invalid("Invalid proxy provider YAML"))?;
            rewrite_references(&mut provider, &destinations, 0)?;
            serde_yaml::to_string(&provider)
                .map_err(|_| invalid("Invalid provider YAML"))?
                .into_bytes()
        } else {
            asset.bytes.to_vec()
        };
        if bytes.len() as u64 > MAX_ASSET_BYTES {
            return Err(invalid("Rewritten local provider exceeds its byte budget"));
        }
        written_total = written_total
            .checked_add(bytes.len())
            .filter(|total| *total <= MAX_TOTAL_BYTES)
            .ok_or_else(|| invalid("Rewritten local resources exceed their budget"))?;
        crate::profiles::atomic_write(&pending.join(&asset.path), &bytes).map_err(io_error)?;
    }
    let mapping = value
        .as_mapping_mut()
        .ok_or_else(|| invalid("Invalid accepted YAML mapping"))?;
    let tun = mapping
        .entry(Value::from("tun"))
        .or_insert_with(|| Value::Mapping(Default::default()));
    tun.as_mapping_mut()
        .ok_or_else(|| invalid("Invalid TUN definition"))?
        .insert(Value::from("enable"), Value::from(false));
    rewrite_references(&mut value, &destinations, 0)?;
    let yaml =
        serde_yaml::to_string(&value).map_err(|_| invalid("Cannot encode local recovery YAML"))?;
    if yaml.len() > MAX_CONFIG_BYTES {
        return Err(invalid("Local recovery YAML exceeds its byte budget"));
    }
    crate::profiles::atomic_write(&pending.join("runtime.yaml"), yaml.as_bytes())
        .map_err(io_error)?;
    Ok(yaml)
}

fn rewrite_references(
    value: &mut Value,
    paths: &BTreeMap<String, PathBuf>,
    depth: usize,
) -> MihomoResult<()> {
    if depth > 64 {
        return Err(invalid("Local recovery nesting exceeds its budget"));
    }
    match value {
        Value::Mapping(mapping) => {
            for (key, value) in mapping {
                if matches!(
                    key.as_str(),
                    Some(
                        "path"
                            | "certificate"
                            | "private-key"
                            | "client-auth-cert"
                            | "ech-key"
                            | "planet"
                    )
                ) && let Some(path) = value.as_str().and_then(|path| paths.get(path))
                {
                    *value = Value::from(
                        path.to_str()
                            .ok_or_else(|| invalid("Local resource path is not UTF-8"))?,
                    );
                }
                rewrite_references(value, paths, depth + 1)?;
            }
        }
        Value::Sequence(sequence) => {
            for value in sequence {
                rewrite_references(value, paths, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_asset_path(path: &str) -> MihomoResult<()> {
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 3
        || parts[0] != "assets"
        || !matches!(parts[1], "providers" | "tls" | "planet" | "geodata")
        || parts[2].is_empty()
        || parts[2].contains(['\\', ':'])
        || !matches!(
            Path::new(parts[2]).components().next(),
            Some(Component::Normal(_))
        )
    {
        return Err(invalid("Invalid prepared local resource path"));
    }
    Ok(())
}

pub(super) fn linked(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

pub(super) fn check_ancestors(path: &Path) -> MihomoResult<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.is_dir() && !linked(&metadata) => {}
            Ok(_) => {
                return Err(invalid(
                    "Local recovery parent is linked or not a directory",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(error)),
        }
    }
    Ok(())
}

fn check_tree(path: &Path, depth: usize, count: &mut usize) -> MihomoResult<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(error)),
    };
    *count += 1;
    // Local Mihomo can create caches that were absent from the uploaded resources.
    if depth > 4
        || *count > MAX_ASSETS * 2 + 8
        || linked(&metadata)
        || (depth == 0 && !metadata.is_dir())
    {
        return Err(invalid(
            "Local recovery slot is linked or exceeds its tree budget",
        ));
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(io_error)? {
            check_tree(&entry.map_err(io_error)?.path(), depth + 1, count)?;
        }
    } else if !metadata.is_file() {
        return Err(invalid("Local recovery slot contains a non-regular file"));
    }
    Ok(())
}

fn remove_owned_tree(path: &Path) -> MihomoResult<()> {
    check_ancestors(
        path.parent()
            .ok_or_else(|| invalid("Local recovery parent missing"))?,
    )?;
    check_tree(path, 0, &mut 0)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path).map_err(io_error),
        Ok(_) => Err(invalid("Local recovery slot is not a directory")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

fn io_error(_: std::io::Error) -> MihomoError {
    invalid("Local recovery filesystem operation failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> super::super::tests::TestHome {
        super::super::tests::TestHome::new()
    }

    #[tokio::test]
    async fn local_slots_reject_uncovered_lease_before_creating_recovery_files() {
        let home = home();
        let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
        let unrelated = crate::ControlledConfigStore::new(home.0.join("unrelated"));
        let lease = unrelated.acquire_write_lease().await.unwrap();
        let mutation = store.lock_service_tun_mutation().await;
        let bundle = Arc::new(ServiceRuntimeBundle {
            yaml: "tun:\n  enable: true\n".into(),
            assets: vec![],
        });
        let result = bundle
            .materialize_local_runtime_admitted(&store, None, lease, mutation)
            .await;
        assert!(
            result.is_err(),
            "An unrelated lease must not authorize recovery"
        );
        assert!(!store.root().join("local-runtime").exists());
    }

    #[tokio::test]
    async fn local_cache_export_keeps_new_caches_and_held_tls_without_reading_sources() {
        let home = home();
        let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
        let bundle = Arc::new(ServiceRuntimeBundle {
            yaml: "proxy-providers:\n  nodes:\n    type: http\n    path: assets/providers/nodes.yaml\nrule-providers:\n  rules:\n    type: http\n    path: assets/providers/rules.mrs\n".into(),
            assets: vec![RuntimeAsset { path: "assets/tls/cert.pem".into(), bytes: Arc::from(&b"held certificate"[..]) }],
        });
        let config = bundle
            .materialize_local_runtime(&store, None)
            .await
            .unwrap();
        let slot = config.parent().unwrap();
        fs::create_dir_all(slot.join("assets/providers")).unwrap();
        let mut provider: Value =
            serde_yaml::from_str("proxies:\n- name: downloaded\n  type: http\n").unwrap();
        provider["proxies"][0]["certificate"] =
            Value::from(slot.join("assets/tls/cert.pem").to_str().unwrap());
        fs::write(
            slot.join("assets/providers/nodes.yaml"),
            serde_yaml::to_string(&provider).unwrap(),
        )
        .unwrap();
        fs::write(slot.join("assets/providers/rules.mrs"), b"new rules").unwrap();
        fs::remove_file(slot.join("assets/tls/cert.pem")).unwrap();
        let exported = bundle.export_local_caches(&store, config).await.unwrap();
        assert_eq!(exported.yaml(), bundle.yaml());
        assert_eq!(
            &*exported
                .assets
                .iter()
                .find(|asset| asset.path == "assets/tls/cert.pem")
                .unwrap()
                .bytes,
            b"held certificate"
        );
        assert_eq!(
            &*exported
                .assets
                .iter()
                .find(|asset| asset.path == "assets/providers/rules.mrs")
                .unwrap()
                .bytes,
            b"new rules"
        );
        let nodes: Value = serde_yaml::from_slice(
            &exported
                .assets
                .iter()
                .find(|asset| asset.path == "assets/providers/nodes.yaml")
                .unwrap()
                .bytes,
        )
        .unwrap();
        assert_eq!(
            nodes["proxies"][0]["certificate"].as_str(),
            Some("assets/tls/cert.pem")
        );
        assert_eq!(bundle.assets.len(), 1);
    }

    #[tokio::test]
    async fn local_cache_export_missing_removes_old_cache_and_invalid_cache_preserves_bundle() {
        let home = home();
        let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
        let bundle = Arc::new(ServiceRuntimeBundle {
            yaml:
                "rule-providers:\n  rules:\n    type: http\n    path: assets/providers/rules.mrs\n"
                    .into(),
            assets: vec![RuntimeAsset {
                path: "assets/providers/rules.mrs".into(),
                bytes: Arc::from(&b"old"[..]),
            }],
        });
        let config = bundle
            .materialize_local_runtime(&store, None)
            .await
            .unwrap();
        let cache = config.parent().unwrap().join("assets/providers/rules.mrs");
        fs::remove_file(&cache).unwrap();
        assert!(
            bundle
                .export_local_caches(&store, config.clone())
                .await
                .unwrap()
                .assets
                .is_empty()
        );
        fs::create_dir(&cache).unwrap();
        assert!(bundle.export_local_caches(&store, config).await.is_err());
        assert_eq!(&*bundle.assets[0].bytes, b"old");
        assert!(
            bundle
                .export_local_caches(&store, home.0.join("outside/runtime.yaml"))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn local_slots_rotate_after_mihomo_populates_previously_missing_caches() {
        let home = home();
        let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
        let mut yaml = String::from("rule-providers:\n");
        for index in 0..MAX_ASSETS {
            yaml.push_str(&format!(
                "  cache-{index}:\n    type: http\n    path: assets/providers/cache-{index}.mrs\n"
            ));
        }
        let bundle = Arc::new(ServiceRuntimeBundle {
            yaml,
            assets: (0..4)
                .map(|index| RuntimeAsset {
                    path: format!("assets/tls/{index}.pem"),
                    bytes: Arc::from([1u8]),
                })
                .collect(),
        });
        let first = bundle
            .materialize_local_runtime(&store, None)
            .await
            .unwrap();
        let caches = first.parent().unwrap().join("assets/providers");
        fs::create_dir(&caches).unwrap();
        for index in 0..MAX_ASSETS {
            fs::write(caches.join(format!("cache-{index}.mrs")), [1]).unwrap();
        }
        let second = bundle
            .materialize_local_runtime(&store, Some(first.clone()))
            .await
            .unwrap();
        let third = bundle
            .materialize_local_runtime(&store, Some(second))
            .await
            .expect("a populated HTTP cache slot must remain eligible for rotation");
        assert_eq!(third, first);
        assert!(!caches.exists());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn local_slots_reject_junctioned_destination_without_writing_outside() {
        let home = home();
        let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
        fs::create_dir_all(store.root()).unwrap();
        let outside = home.0.join("outside");
        fs::create_dir(&outside).unwrap();
        let junction = store.root().join("local-runtime");
        let output = std::process::Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(output.status.success());
        let bundle = Arc::new(
            ServiceRuntimeBundle::prepare("mode: rule\n", home.0.clone())
                .await
                .unwrap(),
        );
        assert!(
            bundle
                .materialize_local_runtime(&store, None)
                .await
                .is_err()
        );
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        fs::remove_dir(junction).unwrap();
    }

    #[tokio::test]
    async fn local_slots_use_held_tls_and_provider_bytes_after_sources_are_deleted() {
        let home = home();
        fs::write(home.0.join("key.pem"), b"held private key").unwrap();
        fs::write(
            home.0.join("provider.yaml"),
            "proxies:\n- name: tls-node\n  type: trojan\n  private-key: key.pem\n",
        )
        .unwrap();
        let source = "tun:\n  enable: true\nproxy-providers:\n  file:\n    type: file\n    path: provider.yaml\n  missing:\n    type: http\n    url: https://example.com/cache\nrule-providers:\n  rules:\n    type: file\n    path: rules.mrs\n";
        fs::write(home.0.join("rules.mrs"), [0, 255, 1, 2]).unwrap();
        let bundle = Arc::new(
            ServiceRuntimeBundle::prepare(source, home.0.clone())
                .await
                .unwrap(),
        );
        fs::remove_file(home.0.join("key.pem")).unwrap();
        fs::remove_file(home.0.join("provider.yaml")).unwrap();
        fs::remove_file(home.0.join("rules.mrs")).unwrap();
        let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
        let path = bundle
            .materialize_local_runtime(&store, None)
            .await
            .unwrap();
        let value: Value = serde_yaml::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["tun"]["enable"].as_bool(), Some(false));
        let file = PathBuf::from(value["proxy-providers"]["file"]["path"].as_str().unwrap());
        let proxy: Value = serde_yaml::from_slice(&fs::read(file).unwrap()).unwrap();
        assert_eq!(
            fs::read(proxy["proxies"][0]["private-key"].as_str().unwrap()).unwrap(),
            b"held private key"
        );
        assert_eq!(
            fs::read(value["rule-providers"]["rules"]["path"].as_str().unwrap()).unwrap(),
            [0, 255, 1, 2]
        );
        let missing = Path::new(
            value["proxy-providers"]["missing"]["path"]
                .as_str()
                .unwrap(),
        );
        assert!(missing.is_absolute() && missing.starts_with(path.parent().unwrap()));
        assert!(!missing.exists());
        assert!(bundle.yaml().contains("enable: true"));
    }

    #[tokio::test]
    async fn local_slot_failure_preserves_active_configuration_and_resources() {
        let home = home();
        let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
        let bundle = Arc::new(
            ServiceRuntimeBundle::prepare("mode: rule\n", home.0.clone())
                .await
                .unwrap(),
        );
        let first = bundle
            .materialize_local_runtime(&store, None)
            .await
            .unwrap();
        let first_bytes = fs::read(&first).unwrap();
        let invalid_bundle = Arc::new(ServiceRuntimeBundle {
            yaml: "tun: broken\n".into(),
            assets: vec![],
        });
        assert!(
            invalid_bundle
                .materialize_local_runtime(&store, Some(first.clone()))
                .await
                .is_err()
        );
        assert_eq!(fs::read(&first).unwrap(), first_bytes);
        assert!(!store.root().join("local-runtime/.slot1.pending").exists());
        let second = bundle
            .materialize_local_runtime(&store, Some(first.clone()))
            .await
            .unwrap();
        assert!(second.ends_with("slot1/runtime.yaml"));
        assert_eq!(fs::read(&first).unwrap(), first_bytes);
        let third = bundle
            .materialize_local_runtime(&store, Some(second.clone()))
            .await
            .unwrap();
        assert_eq!(third, first);
        assert!(second.exists());
    }

    #[tokio::test]
    async fn local_slots_reject_an_unrelated_active_path_before_creating_directories() {
        let home = home();
        let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
        let bundle = Arc::new(
            ServiceRuntimeBundle::prepare("mode: rule\n", home.0.clone())
                .await
                .unwrap(),
        );
        assert!(
            bundle
                .materialize_local_runtime(&store, Some(home.0.join("unrelated.yaml")))
                .await
                .is_err()
        );
        assert!(!store.root().join("local-runtime").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_slots_reject_symlinked_destination_without_writing_outside() {
        let home = home();
        let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
        fs::create_dir_all(store.root()).unwrap();
        let outside = home.0.join("outside");
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, store.root().join("local-runtime")).unwrap();
        let bundle = Arc::new(
            ServiceRuntimeBundle::prepare("mode: rule\n", home.0.clone())
                .await
                .unwrap(),
        );
        assert!(
            bundle
                .materialize_local_runtime(&store, None)
                .await
                .is_err()
        );
        assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
    }
}
