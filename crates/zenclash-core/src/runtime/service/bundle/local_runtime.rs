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
        lease: crate::platform::data_coordinator::DataWriteLease,
        mutation: tokio::sync::OwnedMutexGuard<()>,
    ) -> MihomoResult<(
        (PathBuf, String),
        crate::platform::data_coordinator::DataWriteLease,
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
        if provider.kind == ProviderKind::Proxy {
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
        crate::platform::fs_io::atomic_write(&pending.join(&asset.path), &bytes)
            .map_err(io_error)?;
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
    crate::platform::fs_io::atomic_write(&pending.join("runtime.yaml"), yaml.as_bytes())
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

pub(super) fn validate_asset_path(path: &str) -> MihomoResult<()> {
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

pub(super) fn remove_owned_tree(path: &Path) -> MihomoResult<()> {
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
mod tests;
