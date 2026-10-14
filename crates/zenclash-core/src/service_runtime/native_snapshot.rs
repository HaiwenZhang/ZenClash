//! Private files retained for a native Start/Stage operation and its recovery.

use std::{fs, io::Write as _, path::Path, sync::Arc};

use super::*;

#[derive(Clone)]
pub(crate) struct FrozenRuntime {
    pub(crate) native: zenclash_service::RuntimeBundle,
    pub(crate) config: PathBuf,
    directory: Arc<FrozenDirectory>,
}

struct FrozenDirectory(PathBuf);

impl Drop for FrozenDirectory {
    fn drop(&mut self) {
        let path = self.0.clone();
        let cleanup = move || {
            if let Err(error) = local_runtime::remove_owned_tree(&path) {
                tracing::warn!(%error, "could not remove an owned service snapshot");
            }
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(cleanup);
        } else {
            let _ = std::thread::Builder::new()
                .name("zenclash-snapshot-cleanup".into())
                .spawn(cleanup);
        }
    }
}

impl FrozenRuntime {
    pub(crate) fn root(&self) -> &Path {
        &self.directory.0
    }

    pub(crate) async fn prepare(
        bundle: Arc<ServiceRuntimeBundle>,
        home: PathBuf,
        core: Option<PathBuf>,
        validate: bool,
    ) -> MihomoResult<Self> {
        tokio::task::spawn_blocking(move || {
            let core = core.map_or_else(
                || crate::managed_core_process::service_core_source(&home),
                Ok,
            )?;
            let prepared = materialize(&bundle, &home, &core)?;
            if validate {
                crate::verify_ordinary_local_executable(&core)?;
                let validator =
                    crate::CoreConfigValidator::new(crate::CoreKind::Mihomo, core, prepared.root());
                // This newly allocated directory is owned by the retained snapshot.
                // Validation must not reacquire an outer application-home write lease.
                validator
                    .validate_file_while_leased(&prepared.config)
                    .map_err(|error| MihomoError::InvalidInput(error.to_string()))?;
            }
            Ok(prepared)
        })
        .await
        .map_err(|error| MihomoError::Process(error.to_string()))?
    }
}

fn materialize(
    bundle: &ServiceRuntimeBundle,
    home: &Path,
    core: &Path,
) -> MihomoResult<FrozenRuntime> {
    if !home.is_absolute() || !core.is_absolute() {
        return Err(invalid("Service source paths must be absolute"));
    }
    local_runtime::check_ancestors(home)?;
    let parent = home.join("service-snapshots");
    fs::create_dir_all(&parent).map_err(io_error)?;
    local_runtime::check_ancestors(&parent)?;
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|error| MihomoError::Process(error.to_string()))?;
    let name: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let root = parent.join(name);
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let mut builder = builder;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(&root).map_err(io_error)?;
    let directory = Arc::new(FrozenDirectory(root.clone()));
    let mut native_assets = Vec::with_capacity(bundle.assets.len());
    let remote = bundle.cache_providers()?;
    for asset in &bundle.assets {
        local_runtime::validate_asset_path(&asset.path)?;
        let destination = asset
            .path
            .strip_prefix("assets/geodata/")
            .unwrap_or(&asset.path);
        let source = root.join(destination);
        private_write(&source, &asset.bytes)?;
        // Native upstream caches are core-owned; do not declare the same path as a
        // copied asset. Retain their bytes locally for a possible sidecar recovery.
        if !remote.iter().any(|provider| provider.path == asset.path) {
            // The service requires canonical sources; on Windows this also adds
            // the verbatim prefix and normalizes nested asset separators.
            #[cfg(windows)]
            let source = fs::canonicalize(&source).map_err(io_error)?;
            native_assets.push(zenclash_service::RuntimeAsset {
                source: source.to_string_lossy().into_owned(),
                destination: destination.to_owned(),
            });
        }
    }
    let config = root.join("runtime.yaml");
    private_write(&config, bundle.yaml.as_bytes())?;
    let mapping = serde_yaml::from_str::<Value>(&bundle.yaml)
        .map_err(|_| invalid("Invalid prepared service YAML"))?;
    let mapping = mapping
        .as_mapping()
        .ok_or_else(|| invalid("Service YAML must be a mapping"))?;
    let remote_providers = crate::service::remote_providers_of(mapping, &root)
        .map_err(|error| MihomoError::InvalidInput(error.to_string()))?
        .into_iter()
        .map(|provider| provider.provider)
        .collect();
    Ok(FrozenRuntime {
        native: zenclash_service::RuntimeBundle {
            yaml: bundle.yaml.clone(),
            assets: native_assets,
            remote_providers,
            core_path: core.to_string_lossy().into_owned(),
        },
        config,
        directory,
    })
}

fn private_write(path: &Path, bytes: &[u8]) -> MihomoResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Missing snapshot parent"))?;
    fs::create_dir_all(parent).map_err(io_error)?;
    #[cfg(windows)]
    let mut file = crate::service::create_private_current_user_file(path)
        .map_err(|error| MihomoError::Process(error.to_string()))?;
    #[cfg(not(windows))]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(io_error)?
    };
    file.write_all(bytes).map_err(io_error)?;
    file.sync_all().map_err(io_error)
}

fn io_error(error: std::io::Error) -> MihomoError {
    MihomoError::Process(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service_runtime::test_support::TestHome;

    fn bundle() -> ServiceRuntimeBundle {
        ServiceRuntimeBundle {
            yaml: "mode: rule\nproxy-providers:\n  local: {type: file, path: assets/providers/0.yaml}\n".into(),
            assets: vec![
                RuntimeAsset {
                    path: "assets/providers/0.yaml".into(),
                    bytes: Arc::from(&b"proxies: []\n"[..]),
                },
                RuntimeAsset {
                    path: "assets/tls/1.pem".into(),
                    bytes: Arc::from(&b"certificate"[..]),
                },
                RuntimeAsset {
                    path: "assets/geodata/GeoIP.dat".into(),
                    bytes: Arc::from(&b"geodata"[..]),
                },
            ],
        }
    }

    #[cfg(windows)]
    #[test]
    fn ordinary_home_emits_canonical_native_asset_sources() {
        let home = TestHome::new();
        let ordinary_home = std::env::temp_dir().join(home.0.file_name().unwrap());
        let frozen = materialize(&bundle(), &ordinary_home, &ordinary_home.join("mihomo")).unwrap();

        let expected: Vec<_> = ["assets/providers/0.yaml", "assets/tls/1.pem", "GeoIP.dat"]
            .into_iter()
            .map(|destination| {
                (
                    destination.to_owned(),
                    fs::canonicalize(frozen.root().join(destination))
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                )
            })
            .collect();
        let actual: Vec<_> = frozen
            .native
            .assets
            .iter()
            .map(|asset| (asset.destination.clone(), asset.source.clone()))
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn canonical_home_emits_canonical_nested_native_asset_sources() {
        let home = TestHome::new();
        let frozen = materialize(&bundle(), &home.0, &home.0.join("mihomo")).unwrap();

        let expected: Vec<_> = ["assets/providers/0.yaml", "assets/tls/1.pem", "GeoIP.dat"]
            .into_iter()
            .map(|destination| {
                (
                    destination.to_owned(),
                    fs::canonicalize(frozen.root().join(destination))
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                )
            })
            .collect();
        let actual: Vec<_> = frozen
            .native
            .assets
            .iter()
            .map(|asset| (asset.destination.clone(), asset.source.clone()))
            .collect();
        assert_eq!(actual, expected);
    }
}
