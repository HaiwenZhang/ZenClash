use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde_yaml::{Mapping, Value};
use sha2::{Digest, Sha256};

use crate::protocol::ServiceErrorCode;
use crate::runtime_manifest::{
    CacheDisposition, RuntimeManifest, SourceIdentity, declared_remote_providers, plan_stage,
};

pub(crate) const MAX_CONFIG_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_ASSET_BYTES: u64 = 128 * 1024 * 1024;
pub(crate) const MAX_RUNTIME_BYTES: u64 = 256 * 1024 * 1024;
pub(crate) const MAX_ASSETS: usize = 256;
pub(crate) const ASSET_CHUNK_BYTES: usize = 256 * 1024;

#[derive(Debug, thiserror::Error)]
pub(crate) enum RuntimeError {
    #[error("runtime configuration was rejected")]
    Configuration,
    #[error("runtime resource path was rejected")]
    Asset,
    #[error("runtime resource budget exceeded")]
    Budget,
    #[error("runtime resource upload is incomplete")]
    Incomplete,
    #[error("runtime storage failed")]
    Storage(#[from] io::Error),
}

impl RuntimeError {
    pub(crate) fn code(&self) -> ServiceErrorCode {
        match self {
            Self::Configuration => ServiceErrorCode::InvalidConfiguration,
            Self::Asset | Self::Incomplete => ServiceErrorCode::InvalidAsset,
            Self::Budget => ServiceErrorCode::BudgetExceeded,
            Self::Storage(_) => ServiceErrorCode::Internal,
        }
    }
}

pub(crate) struct StagedRuntime {
    resources: Arc<RuntimeResources>,
    configuration_directory: PathBuf,
    configuration: Mapping,
    required_assets: BTreeSet<String>,
    uploads: BTreeMap<String, Upload>,
    geodata: BTreeSet<String>,
    uploaded_bytes: u64,
    storage_failed: bool,
    formal_snapshot: Option<Mapping>,
    previous_resources: Mutex<Option<Arc<RuntimeResources>>>,
}

struct RuntimeResources {
    directory: PathBuf,
    prepared_directory: PathBuf,
    manifest: Mutex<Option<RuntimeManifest>>,
    #[cfg(test)]
    readback_gate: Option<Arc<ReadbackCopyGate>>,
}

#[cfg(test)]
pub(crate) struct ReadbackCopyGate {
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

#[cfg(test)]
impl ReadbackCopyGate {
    pub(crate) fn new() -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        (
            Arc::new(Self {
                entered: Mutex::new(Some(entered)),
                release: Mutex::new(released),
            }),
            waiting,
            release,
        )
    }

    fn wait(&self) -> Result<(), RuntimeError> {
        if let Some(entered) = self.entered.lock().map_err(|_| RuntimeError::Asset)?.take() {
            let _ = entered.send(());
        }
        self.release
            .lock()
            .map_err(|_| RuntimeError::Asset)?
            .recv_timeout(std::time::Duration::from_secs(3))
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "readback fixture gate ended"))?;
        Ok(())
    }
}

pub(crate) struct ProviderCacheSource {
    resources: Arc<RuntimeResources>,
    relative: String,
    proxy: bool,
}

impl ProviderCacheSource {
    /// Reads only a manifest-owned cache after the server has quiesced its writer.
    /// The counted bytes include failed reads and rejected proxy parsing.
    pub(crate) fn snapshot(
        self,
        budget: usize,
        deadline: std::time::Instant,
    ) -> (Result<Option<Vec<u8>>, RuntimeError>, usize) {
        let mut scanned = 0;
        let result = (|| {
            #[cfg(test)]
            if let Some(gate) = &self.resources.readback_gate {
                gate.wait()?;
            }
            let mut file = match open_resource(
                &self.resources.prepared_directory,
                Path::new(&self.relative),
            ) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            let before = file.metadata()?;
            let limit = budget.min(if self.proxy {
                MAX_CONFIG_BYTES
            } else {
                MAX_ASSET_BYTES as usize
            });
            if before.len() > limit as u64 {
                return Err(RuntimeError::Budget);
            }
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(before.len() as usize)
                .map_err(|_| RuntimeError::Budget)?;
            let mut buffer = [0; 32 * 1024];
            loop {
                if std::time::Instant::now() >= deadline {
                    return Err(RuntimeError::Budget);
                }
                let count = file.read(&mut buffer)?;
                scanned += count;
                if scanned > limit {
                    return Err(RuntimeError::Budget);
                }
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            let after = file.metadata()?;
            if before.len() != scanned as u64
                || before.len() != after.len()
                || before.modified().ok() != after.modified().ok()
            {
                return Err(RuntimeError::Asset);
            }
            drop(file);
            if self.proxy {
                let mut nodes: Value =
                    serde_yaml::from_slice(&bytes).map_err(|_| RuntimeError::Configuration)?;
                if !nodes.get("proxies").is_some_and(Value::is_sequence) {
                    return Err(RuntimeError::Configuration);
                }
                let manifest = self
                    .resources
                    .manifest
                    .lock()
                    .map_err(|_| RuntimeError::Asset)?
                    .clone()
                    .ok_or(RuntimeError::Incomplete)?;
                // Reverse only approved absolute references. HTTP transport paths and inline keys remain intact.
                remap_cached_assets(
                    &mut nodes,
                    &self.resources,
                    &manifest,
                    &manifest,
                    Path::new(""),
                    0,
                )?;
                bytes = serde_yaml::to_string(&nodes)
                    .map_err(|_| RuntimeError::Configuration)?
                    .into_bytes();
                if bytes.len() > limit {
                    return Err(RuntimeError::Budget);
                }
            }
            if std::time::Instant::now() >= deadline {
                return Err(RuntimeError::Budget);
            }
            Ok(Some(bytes))
        })();
        (result, scanned)
    }
}

struct CacheBudget<'a> {
    replacement: &'a mut ReplacementBudget,
    scan_bytes: u64,
    retry_index: usize,
}

struct PriorManifest<'a> {
    resources: &'a RuntimeResources,
    manifest: RuntimeManifest,
}

pub(crate) struct RetiredRuntime {
    configuration_directory: PathBuf,
    resources: Arc<RuntimeResources>,
}

impl RetiredRuntime {
    pub(crate) fn directories(&self, retained: &[&StagedRuntime]) -> Vec<PathBuf> {
        let shared = retained.iter().any(|runtime| {
            runtime.resources.directory == self.resources.directory
                || runtime.resources.prepared_directory == self.resources.prepared_directory
        });
        let mut directories = Vec::with_capacity(3);
        if !shared
            || (!self
                .configuration_directory
                .starts_with(&self.resources.directory)
                && !self
                    .configuration_directory
                    .starts_with(&self.resources.prepared_directory))
        {
            directories.push(self.configuration_directory.clone());
        }
        if !shared {
            directories.push(self.resources.prepared_directory.clone());
            directories.push(self.resources.directory.clone());
        }
        directories
    }
}

struct Upload {
    file: Option<File>,
    bytes: u64,
    finished: bool,
    hash: Sha256,
}

impl Upload {
    fn identity(&self) -> SourceIdentity {
        SourceIdentity {
            len: self.bytes,
            sha256: self.hash.clone().finalize().into(),
        }
    }
}

pub(crate) struct ValidationConfig {
    path: PathBuf,
    cleaned: bool,
}

impl ValidationConfig {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn cleanup(mut self) -> Result<(), RuntimeError> {
        remove_validation(&self.path)?;
        self.cleaned = true;
        Ok(())
    }
}

impl Drop for ValidationConfig {
    fn drop(&mut self) {
        if !self.cleaned
            && let Err(error) = remove_validation(&self.path)
        {
            eprintln!("private validation configuration cleanup failed: {error}");
        }
    }
}

fn remove_validation(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

impl StagedRuntime {
    #[cfg(test)]
    pub(crate) fn set_readback_gate(&mut self, gate: Arc<ReadbackCopyGate>) {
        Arc::get_mut(&mut self.resources).unwrap().readback_gate = Some(gate);
    }
    pub(crate) fn provider_cache_source(
        &self,
        kind: crate::protocol::ProviderKind,
        name: &str,
    ) -> Result<ProviderCacheSource, RuntimeError> {
        if name.is_empty() || name.len() > 4096 || name.chars().any(char::is_control) {
            return Err(RuntimeError::Asset);
        }
        let (namespace, proxy) = match kind {
            crate::protocol::ProviderKind::Proxy => ("proxy-providers", true),
            crate::protocol::ProviderKind::Rule => ("rule-providers", false),
        };
        let relative = provider_destination(namespace, name, true);
        if !self
            .resources
            .manifest
            .lock()
            .map_err(|_| RuntimeError::Asset)?
            .as_ref()
            .is_some_and(|manifest| manifest.remote_providers.contains_key(&relative))
        {
            return Err(RuntimeError::Asset);
        }
        Ok(ProviderCacheSource {
            resources: self.resources.clone(),
            relative,
            proxy,
        })
    }
    pub(crate) fn has_materialized(&self) -> Result<bool, RuntimeError> {
        match fs::symlink_metadata(self.configuration_directory.join("runtime.yaml")) {
            Ok(metadata) => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        return Err(RuntimeError::Asset);
                    }
                }
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(RuntimeError::Asset);
                }
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn materialize_validation(&self) -> Result<ValidationConfig, RuntimeError> {
        let mut configuration = if self.has_materialized()? {
            // This fixed leaf was generated by the service in its protected
            // configuration directory. Validation must not rewrite live assets.
            let file = crate::platform::open_pinned_file(
                &self.configuration_directory.join("runtime.yaml"),
            )?;
            let mut bytes = Vec::new();
            file.take((MAX_CONFIG_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_CONFIG_BYTES {
                return Err(RuntimeError::Budget);
            }
            serde_yaml::from_slice::<Mapping>(&bytes).map_err(|_| RuntimeError::Configuration)?
        } else {
            self.prepare_configuration(&mut ReplacementBudget::default())?
        };
        for key in [
            "external-controller",
            "external-controller-pipe",
            "external-controller-unix",
            "external-controller-tls",
            "secret",
        ] {
            configuration.remove(Value::from(key));
        }
        configuration.insert(Value::from("external-controller"), Value::from(""));
        #[cfg(windows)]
        configuration.insert(Value::from("external-controller-pipe"), Value::from(""));
        #[cfg(unix)]
        configuration.insert(Value::from("external-controller-unix"), Value::from(""));
        configuration.insert(Value::from("secret"), Value::from(""));
        let yaml =
            serde_yaml::to_string(&configuration).map_err(|_| RuntimeError::Configuration)?;
        if yaml.len() > MAX_CONFIG_BYTES {
            return Err(RuntimeError::Budget);
        }
        let path = self.configuration_directory.join("validation.yaml");
        // An unfinished previous cleanup is evidence, not permission to replace
        // an existing file. There is at most one bounded validation leaf.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let guard = ValidationConfig {
            path,
            cleaned: false,
        };
        let result = file
            .write_all(yaml.as_bytes())
            .and_then(|()| file.sync_all());
        drop(file);
        if let Err(error) = result {
            guard.cleanup()?;
            return Err(error.into());
        }
        Ok(guard)
    }

    /// `directory` is a fresh private directory created by the installer/server.
    pub(crate) fn new(directory: PathBuf, yaml: &str) -> Result<Self, RuntimeError> {
        let (configuration, required_assets) = validate_configuration(yaml)?;
        Ok(Self {
            resources: Arc::new(RuntimeResources {
                prepared_directory: directory.join("prepared"),
                directory: directory.clone(),
                manifest: Mutex::new(None),
                #[cfg(test)]
                readback_gate: None,
            }),
            configuration_directory: directory.clone(),
            configuration,
            required_assets,
            uploads: BTreeMap::new(),
            geodata: BTreeSet::new(),
            uploaded_bytes: 0,
            storage_failed: false,
            formal_snapshot: None,
            previous_resources: Mutex::new(None),
        })
    }

    pub(crate) fn new_with_configuration(
        directory: PathBuf,
        configuration_directory: PathBuf,
        yaml: &str,
    ) -> Result<Self, RuntimeError> {
        let revision = directory.file_name().ok_or(RuntimeError::Asset)?;
        let session = directory
            .parent()
            .and_then(Path::parent)
            .ok_or(RuntimeError::Asset)?;
        let prepared_directory = session.join("assets").join(revision);
        let mut runtime = Self::new(directory, yaml)?;
        runtime.resources = Arc::new(RuntimeResources {
            directory: runtime.resources.directory.clone(),
            prepared_directory,
            manifest: Mutex::new(None),
            #[cfg(test)]
            readback_gate: None,
        });
        runtime.configuration_directory = configuration_directory;
        Ok(runtime)
    }

    pub(crate) fn inherit_resources(&mut self, previous: &Self) {
        self.previous_resources = Mutex::new(Some(previous.resources.clone()));
    }

    #[cfg(test)]
    pub(crate) fn prepared_directory(&self) -> &Path {
        &self.resources.prepared_directory
    }
    pub(crate) fn into_retired(self) -> RetiredRuntime {
        RetiredRuntime {
            configuration_directory: self.configuration_directory,
            resources: self.resources,
        }
    }

    pub(crate) fn retirement_for_configuration(
        &self,
        configuration_directory: PathBuf,
    ) -> RetiredRuntime {
        RetiredRuntime {
            configuration_directory,
            resources: self.resources.clone(),
        }
    }

    pub(crate) fn fork_patch(
        &self,
        configuration_directory: PathBuf,
        patch: &serde_json::Value,
    ) -> Result<Self, RuntimeError> {
        let file =
            crate::platform::open_pinned_file(&self.configuration_directory.join("runtime.yaml"))?;
        let mut bytes = Vec::new();
        file.take((MAX_CONFIG_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(RuntimeError::Budget);
        }
        let mut configuration: serde_yaml::Value =
            serde_yaml::from_slice(&bytes).map_err(|_| RuntimeError::Configuration)?;
        let patch = serde_yaml::to_value(patch).map_err(|_| RuntimeError::Configuration)?;
        merge_mapping(&mut configuration, patch)?;
        let configuration = configuration
            .as_mapping()
            .cloned()
            .ok_or(RuntimeError::Configuration)?;
        let runtime = Self {
            resources: self.resources.clone(),
            configuration_directory,
            configuration: Mapping::new(),
            formal_snapshot: Some(configuration),
            required_assets: BTreeSet::new(),
            uploads: BTreeMap::new(),
            geodata: self.geodata.clone(),
            uploaded_bytes: 0,
            storage_failed: false,
            previous_resources: Mutex::new(None),
        };
        runtime.write_configuration(
            runtime
                .formal_snapshot
                .as_ref()
                .ok_or(RuntimeError::Configuration)?,
            &mut ReplacementBudget::default(),
        )?;
        Ok(runtime)
    }

    pub(crate) fn upload(
        &mut self,
        path: &str,
        offset: u64,
        bytes: &[u8],
        finished: bool,
    ) -> Result<(), RuntimeError> {
        if self.storage_failed {
            return Err(RuntimeError::Incomplete);
        }
        validate_asset_path(path)?;
        if bytes.len() > ASSET_CHUNK_BYTES {
            return Err(RuntimeError::Budget);
        }
        let next_total = self
            .uploaded_bytes
            .checked_add(bytes.len() as u64)
            .filter(|total| *total <= MAX_RUNTIME_BYTES)
            .ok_or(RuntimeError::Budget)?;
        if !self.uploads.contains_key(path) {
            if offset != 0 {
                return Err(RuntimeError::Asset);
            }
            if self.uploads.len() >= MAX_ASSETS {
                return Err(RuntimeError::Budget);
            }
            let target = self.resources.directory.join(path);
            create_relative_parents(&self.resources.directory, Path::new(path))?;
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(target)?;
            self.uploads.insert(
                path.to_owned(),
                Upload {
                    file: Some(file),
                    bytes: 0,
                    finished: false,
                    hash: Sha256::new(),
                },
            );
            if matches!(
                path,
                "assets/geodata/GeoIP.dat"
                    | "assets/geodata/geosite.dat"
                    | "assets/geodata/country.mmdb"
                    | "assets/geodata/ASN.mmdb"
            ) {
                self.geodata.insert(path.to_owned());
            }
        }
        let upload = self.uploads.get_mut(path).ok_or(RuntimeError::Asset)?;
        if upload.finished || upload.bytes != offset {
            return Err(RuntimeError::Asset);
        }
        let next_bytes = upload
            .bytes
            .checked_add(bytes.len() as u64)
            .filter(|count| *count <= MAX_ASSET_BYTES)
            .ok_or(RuntimeError::Budget)?;
        let file = upload.file.as_mut().ok_or(RuntimeError::Incomplete)?;
        if let Err(error) = file.write_all(bytes) {
            self.storage_failed = true;
            return Err(error.into());
        }
        upload.bytes = next_bytes;
        upload.hash.update(bytes);
        self.uploaded_bytes = next_total;
        if finished {
            if let Err(error) = file.sync_all() {
                self.storage_failed = true;
                return Err(error.into());
            }
            upload.finished = true;
            // Approved readers pin against writers on Windows. Close the
            // upload writer only after the last bytes have reached storage.
            upload.file.take();
        }
        Ok(())
    }

    pub(crate) fn materialize(
        &self,
        controller: &str,
        secret: &str,
    ) -> Result<PathBuf, RuntimeError> {
        let mut budget = ReplacementBudget::default();
        let mut configuration = self.prepare_configuration(&mut budget)?;
        // Privileged controllers are reachable only through the service. The
        // GUI never receives their address or secret.
        configuration.insert(Value::from("external-controller"), Value::from(""));
        configuration.insert(Value::from("secret"), Value::from(secret));
        #[cfg(windows)]
        let key = "external-controller-pipe";
        #[cfg(unix)]
        let key = "external-controller-unix";
        configuration.insert(Value::from(key), Value::from(controller));
        self.write_configuration(&configuration, &mut budget)
    }

    fn write_configuration(
        &self,
        configuration: &Mapping,
        budget: &mut ReplacementBudget,
    ) -> Result<PathBuf, RuntimeError> {
        let yaml =
            serde_yaml::to_string(&configuration).map_err(|_| RuntimeError::Configuration)?;
        if yaml.len() > MAX_CONFIG_BYTES {
            return Err(RuntimeError::Budget);
        }
        let path = self.configuration_directory.join("runtime.yaml");
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random)
            .map_err(|_| RuntimeError::Storage(io::Error::other("random source unavailable")))?;
        let temporary = self
            .configuration_directory
            .join(format!("runtime-{:x}.tmp", u64::from_le_bytes(random)));
        let result = (|| -> io::Result<()> {
            let mut file = create_temporary(&temporary)?;
            file.write_all(yaml.as_bytes())?;
            file.sync_all()?;
            replace(&temporary, &path, &file, budget)
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(temporary);
            return Err(error.into());
        }
        Ok(path)
    }

    fn prepare_configuration(
        &self,
        budget: &mut ReplacementBudget,
    ) -> Result<Mapping, RuntimeError> {
        if let Some(configuration) = &self.formal_snapshot {
            return Ok(configuration.clone());
        }
        if self.storage_failed
            || self.uploads.values().any(|upload| !upload.finished)
            || self
                .required_assets
                .iter()
                .any(|path| !self.uploads.get(path).is_some_and(|upload| upload.finished))
        {
            return Err(RuntimeError::Incomplete);
        }
        if self
            .uploaded_bytes
            .saturating_mul(2)
            .saturating_add(2 * MAX_CONFIG_BYTES as u64)
            > MAX_RUNTIME_BYTES
        {
            return Err(RuntimeError::Budget);
        }
        let previous = self
            .resources
            .manifest
            .lock()
            .map_err(|_| RuntimeError::Asset)?
            .clone()
            .unwrap_or_default();
        let assets = self
            .uploads
            .iter()
            .map(|(path, upload)| (path.clone(), upload.identity()))
            .collect::<BTreeMap<_, _>>();
        let remote = self.remote_providers(&assets.keys().cloned().collect())?;
        let mut plan = plan_stage(&previous, assets, remote);
        // Even unchanged uploads are verified against their approved bytes. A
        // privileged kernel can write resources, so cached metadata is no proof.
        for path in plan.copies.iter().chain(&plan.skipped) {
            copy_prepared(
                &self.resources.directory.join(path),
                &self.resources.prepared_directory,
                Path::new(path),
                plan.manifest.assets.get(path).ok_or(RuntimeError::Asset)?,
                budget,
            )?;
        }
        for path in &plan.required_deletes {
            // Only this candidate's previously declared cache can be deleted.
            if previous.remote_providers.contains_key(path) {
                match fs::remove_file(self.resources.prepared_directory.join(path)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        let mut configuration = self.configuration.clone();
        let prior = self
            .previous_resources
            .lock()
            .map_err(|_| RuntimeError::Asset)?
            .clone();
        self.prepare_providers(
            &mut configuration,
            &mut plan.manifest,
            prior.as_deref(),
            budget,
        )?;
        let mut value = Value::Mapping(configuration);
        rewrite_asset_paths(&mut value, &self.resources.prepared_directory)?;
        configuration = value
            .as_mapping()
            .cloned()
            .ok_or(RuntimeError::Configuration)?;
        *self
            .resources
            .manifest
            .lock()
            .map_err(|_| RuntimeError::Asset)? = Some(plan.manifest);
        // No chain of prior owners survives preparation or partial sharing.
        self.previous_resources
            .lock()
            .map_err(|_| RuntimeError::Asset)?
            .take();
        Ok(configuration)
    }

    fn remote_providers(
        &self,
        assets: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, String>, RuntimeError> {
        let mut remote = Vec::new();
        for provider_kind in ["proxy-providers", "rule-providers"] {
            let Some(providers) = self
                .configuration
                .get(Value::from(provider_kind))
                .and_then(Value::as_mapping)
            else {
                continue;
            };
            for (name, provider) in providers {
                if provider["type"] == "http" {
                    let name = name.as_str().ok_or(RuntimeError::Configuration)?;
                    let url = provider["url"]
                        .as_str()
                        .ok_or(RuntimeError::Configuration)?;
                    remote.push((
                        provider_destination(provider_kind, name, true),
                        url.to_owned(),
                    ));
                }
            }
        }
        declared_remote_providers(remote, assets)
    }

    fn prepare_providers(
        &self,
        configuration: &mut Mapping,
        manifest: &mut RuntimeManifest,
        prior: Option<&RuntimeResources>,
        replacement_budget: &mut ReplacementBudget,
    ) -> Result<(), RuntimeError> {
        // One consistent, bounded snapshot per preparation, not one full
        // manifest clone per provider. It does not own or retain a prior group.
        let prior = prior
            .map(|resources| {
                resources
                    .manifest
                    .lock()
                    .map_err(|_| RuntimeError::Asset)
                    .map(|manifest| {
                        manifest.clone().map(|manifest| PriorManifest {
                            resources,
                            manifest,
                        })
                    })
            })
            .transpose()?
            .flatten();
        let mut reserved = self
            .uploaded_bytes
            .saturating_mul(2)
            .saturating_add(2 * MAX_CONFIG_BYTES as u64);
        let mut remote_count = 0_u64;
        for provider_kind in ["proxy-providers", "rule-providers"] {
            let Some(providers) = configuration
                .get(Value::from(provider_kind))
                .and_then(Value::as_mapping)
            else {
                continue;
            };
            for provider in providers.values() {
                let kind = provider["type"]
                    .as_str()
                    .ok_or(RuntimeError::Configuration)?;
                if kind == "http" {
                    remote_count += 1;
                }
                if kind == "file" {
                    let path = provider["path"].as_str().ok_or(RuntimeError::Asset)?;
                    let bytes = if provider_kind == "proxy-providers" {
                        self.prepare_proxy_provider(path)?.len() as u64
                    } else {
                        self.uploads
                            .get(path)
                            .ok_or(RuntimeError::Incomplete)?
                            .bytes
                    };
                    reserved = reserved.checked_add(bytes).ok_or(RuntimeError::Budget)?;
                }
            }
        }
        let remaining = MAX_RUNTIME_BYTES
            .checked_sub(reserved)
            .ok_or(RuntimeError::Budget)?;
        let cache_limit = remaining
            .checked_div(remote_count)
            .unwrap_or(0)
            .min(MAX_ASSET_BYTES);
        if remote_count > 0 && cache_limit == 0 {
            return Err(RuntimeError::Budget);
        }
        let mut cache_budget = CacheBudget {
            replacement: replacement_budget,
            scan_bytes: remaining,
            retry_index: 0,
        };
        for provider_kind in ["proxy-providers", "rule-providers"] {
            let Some(providers) = configuration.get_mut(Value::from(provider_kind)) else {
                continue;
            };
            let providers = providers
                .as_mapping_mut()
                .ok_or(RuntimeError::Configuration)?;
            for (name, provider) in providers {
                let provider = provider
                    .as_mapping_mut()
                    .ok_or(RuntimeError::Configuration)?;
                let kind = provider
                    .get(Value::from("type"))
                    .and_then(Value::as_str)
                    .ok_or(RuntimeError::Configuration)?;
                if kind == "inline" {
                    continue;
                }
                let path = provider
                    .get(Value::from("path"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let relative = provider_destination(
                    provider_kind,
                    name.as_str().ok_or(RuntimeError::Configuration)?,
                    kind == "http",
                );
                ensure_prepared_root(&self.resources.prepared_directory)?;
                let destination = self.resources.prepared_directory.join(&relative);
                if let Ok(metadata) = fs::symlink_metadata(&destination)
                    && (!metadata.is_file() || metadata.file_type().is_symlink())
                {
                    return Err(RuntimeError::Asset);
                }
                let uploaded = self.uploads.get(path).is_some_and(|upload| upload.finished);
                if kind == "file" && !uploaded {
                    return Err(RuntimeError::Incomplete);
                }
                if kind == "http" && !destination.exists() {
                    let requested = provider
                        .get(Value::from("size-limit"))
                        .and_then(Value::as_u64)
                        .filter(|limit| *limit > 0);
                    let limit = requested.map_or(cache_limit, |limit| limit.min(cache_limit));
                    let seed_changed = if uploaded {
                        prior.as_ref().is_some_and(|prior| {
                            prior.manifest.assets.get(path) != manifest.assets.get(path)
                        })
                    } else {
                        false
                    };
                    let disposition = if seed_changed {
                        CacheDisposition::ResourceChanged
                    } else {
                        self.inherit_cache(
                            prior.as_ref(),
                            manifest,
                            &relative,
                            provider_kind == "proxy-providers",
                            limit,
                            &mut cache_budget,
                        )?
                    };
                    manifest
                        .cache_dispositions
                        .insert(relative.clone(), disposition);
                }
                if uploaded && (kind != "http" || !destination.exists()) {
                    let source = self.resources.directory.join(path);
                    if provider_kind == "proxy-providers" {
                        let bytes = self.prepare_proxy_provider(path)?;
                        if kind == "http" && bytes.len() as u64 > cache_limit {
                            return Err(RuntimeError::Budget);
                        }
                        write_prepared(
                            &self.resources.prepared_directory,
                            Path::new(&relative),
                            &bytes,
                            cache_budget.replacement,
                        )?;
                    } else {
                        copy_prepared(
                            &source,
                            &self.resources.prepared_directory,
                            Path::new(&relative),
                            &self
                                .uploads
                                .get(path)
                                .ok_or(RuntimeError::Incomplete)?
                                .identity(),
                            cache_budget.replacement,
                        )?;
                    }
                } else {
                    create_relative_parents(
                        &self.resources.prepared_directory,
                        Path::new(&relative),
                    )?;
                }
                if kind == "http" {
                    let requested = provider
                        .get(Value::from("size-limit"))
                        .map(|value| value.as_u64().ok_or(RuntimeError::Configuration))
                        .transpose()?
                        .filter(|limit| *limit > 0);
                    let limit =
                        requested.map_or(cache_limit, |requested| requested.min(cache_limit));
                    if fs::metadata(&destination).is_ok_and(|metadata| metadata.len() > limit) {
                        return Err(RuntimeError::Budget);
                    }
                    provider.insert(Value::from("size-limit"), Value::from(limit));
                }
                provider.insert(
                    Value::from("path"),
                    Value::from(destination.to_str().ok_or(RuntimeError::Asset)?),
                );
            }
        }
        Ok(())
    }

    fn inherit_cache(
        &self,
        prior: Option<&PriorManifest<'_>>,
        manifest: &RuntimeManifest,
        relative: &str,
        proxy: bool,
        limit: u64,
        budget: &mut CacheBudget<'_>,
    ) -> Result<CacheDisposition, RuntimeError> {
        let Some(prior) = prior else {
            return Ok(CacheDisposition::MissingProvenance);
        };
        let previous = &prior.manifest;
        if previous.remote_providers.get(relative) != manifest.remote_providers.get(relative) {
            return Ok(CacheDisposition::ChangedUrl);
        }
        let file = loop {
            match open_resource(&prior.resources.prepared_directory, Path::new(relative)) {
                Ok(file) => break file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Ok(CacheDisposition::MissingCache);
                }
                Err(error) => {
                    let Some(delay) = cache_retry_delay(&error, budget.retry_index) else {
                        return Ok(CacheDisposition::InvalidCache);
                    };
                    budget.retry_index += 1;
                    std::thread::sleep(delay);
                }
            }
        };
        let before = file.metadata()?;
        let read_limit = if proxy {
            limit.min(MAX_CONFIG_BYTES as u64)
        } else {
            limit
        };
        if before.len() > read_limit {
            return Ok(if proxy && before.len() > MAX_CONFIG_BYTES as u64 {
                CacheDisposition::ParseBudget
            } else {
                CacheDisposition::ByteBudget
            });
        }
        if before.len() > budget.scan_bytes {
            return Ok(CacheDisposition::ByteBudget);
        }
        let mut bytes = Vec::new();
        let mut source = file.take(
            read_limit
                .saturating_add(1)
                .min(budget.scan_bytes.saturating_add(1)),
        );
        source.read_to_end(&mut bytes)?;
        budget.scan_bytes = budget.scan_bytes.saturating_sub(bytes.len() as u64);
        let after = source.get_ref().metadata()?;
        if bytes.len() as u64 > read_limit
            || before.len() != bytes.len() as u64
            || before.len() != after.len()
            || before.modified().ok() != after.modified().ok()
        {
            return Ok(CacheDisposition::InvalidCache);
        }
        if proxy {
            let Ok(mut nodes) = serde_yaml::from_slice::<Value>(&bytes) else {
                return Ok(CacheDisposition::InvalidCache);
            };
            if !nodes.get("proxies").is_some_and(Value::is_sequence) {
                return Ok(CacheDisposition::InvalidCache);
            }
            if remap_cached_assets(
                &mut nodes,
                prior.resources,
                previous,
                manifest,
                &self.resources.prepared_directory,
                0,
            )
            .is_err()
            {
                return Ok(CacheDisposition::ResourceChanged);
            }
            bytes = serde_yaml::to_string(&nodes)
                .map_err(|_| RuntimeError::Configuration)?
                .into_bytes();
            if bytes.len() as u64 > read_limit {
                return Ok(CacheDisposition::ByteBudget);
            }
        }
        write_prepared(
            &self.resources.prepared_directory,
            Path::new(relative),
            &bytes,
            budget.replacement,
        )?;
        Ok(CacheDisposition::Inherited)
    }

    fn prepare_proxy_provider(&self, path: &str) -> Result<Vec<u8>, RuntimeError> {
        let mut bytes = vec![];
        crate::platform::open_pinned_file(&self.resources.directory.join(path))?
            .take((MAX_CONFIG_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(RuntimeError::Budget);
        }
        let expected = self
            .uploads
            .get(path)
            .ok_or(RuntimeError::Incomplete)?
            .identity();
        if bytes.len() as u64 != expected.len
            || <[u8; 32]>::from(Sha256::digest(&bytes)) != expected.sha256
        {
            return Err(RuntimeError::Asset);
        }
        let mut nodes: Value =
            serde_yaml::from_slice(&bytes).map_err(|_| RuntimeError::Configuration)?;
        if !nodes.get("proxies").is_some_and(Value::is_sequence) {
            return Err(RuntimeError::Configuration);
        }
        let mut assets = BTreeSet::new();
        validate_tls_paths(&nodes, &mut assets, 0)?;
        if assets
            .iter()
            .any(|path| !self.uploads.get(path).is_some_and(|upload| upload.finished))
        {
            return Err(RuntimeError::Incomplete);
        }
        rewrite_asset_paths(&mut nodes, &self.resources.prepared_directory)?;
        let yaml = serde_yaml::to_string(&nodes).map_err(|_| RuntimeError::Configuration)?;
        Ok(yaml.into_bytes())
    }

    pub(crate) fn install_geodata(&self, home: &Path) -> Result<(), RuntimeError> {
        let mut budget = ReplacementBudget::default();
        for name in ["GeoIP.dat", "geosite.dat", "country.mmdb", "ASN.mmdb"] {
            let relative = format!("assets/geodata/{name}");
            if self.geodata.contains(&relative) {
                if self.storage_failed
                    || self
                        .uploads
                        .get(&relative)
                        .is_some_and(|upload| !upload.finished)
                {
                    return Err(RuntimeError::Incomplete);
                }
                let expected = self
                    .resources
                    .manifest
                    .lock()
                    .map_err(|_| RuntimeError::Asset)?
                    .as_ref()
                    .and_then(|manifest| manifest.assets.get(&relative))
                    .cloned()
                    .or_else(|| {
                        self.uploads
                            .get(&relative)
                            .filter(|upload| upload.finished)
                            .map(Upload::identity)
                    })
                    .ok_or(RuntimeError::Incomplete)?;
                copy_prepared(
                    &self.resources.directory.join(&relative),
                    home,
                    Path::new(name),
                    &expected,
                    &mut budget,
                )?;
            }
        }
        Ok(())
    }
}

fn ensure_prepared_root(root: &Path) -> Result<(), RuntimeError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(RuntimeError::Asset),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(root)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn write_prepared(
    root: &Path,
    path: &Path,
    bytes: &[u8],
    budget: &mut ReplacementBudget,
) -> Result<(), RuntimeError> {
    ensure_prepared_root(root)?;
    create_relative_parents(root, path)?;
    let target = root.join(path);
    let mut random = [0; 8];
    getrandom::fill(&mut random).map_err(|_| io::Error::other("random source unavailable"))?;
    let temporary = target.with_extension(format!("{:x}.new", u64::from_le_bytes(random)));
    let result = (|| -> io::Result<()> {
        let mut file = create_temporary(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        replace(&temporary, &target, &file, budget)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result.map_err(RuntimeError::Storage)
}

fn copy_prepared(
    source: &Path,
    root: &Path,
    path: &Path,
    expected: &SourceIdentity,
    budget: &mut ReplacementBudget,
) -> Result<(), RuntimeError> {
    let mut source = crate::platform::open_pinned_file(source)?;
    if source.metadata()?.len() != expected.len {
        return Err(RuntimeError::Asset);
    }
    ensure_prepared_root(root)?;
    create_relative_parents(root, path)?;
    let target = root.join(path);
    let mut random = [0; 8];
    getrandom::fill(&mut random).map_err(|_| io::Error::other("random source unavailable"))?;
    let temporary = target.with_extension(format!("{:x}.new", u64::from_le_bytes(random)));
    let result = (|| -> Result<(), RuntimeError> {
        let mut output = create_temporary(&temporary)?;
        let mut hash = Sha256::new();
        let mut count = 0_u64;
        let mut buffer = [0_u8; 32 * 1024];
        loop {
            let bytes = source.read(&mut buffer)?;
            if bytes == 0 {
                break;
            }
            count = count
                .checked_add(bytes as u64)
                .ok_or(RuntimeError::Budget)?;
            if count > expected.len || count > MAX_ASSET_BYTES {
                return Err(RuntimeError::Asset);
            }
            hash.update(&buffer[..bytes]);
            output.write_all(&buffer[..bytes])?;
        }
        if count != expected.len || <[u8; 32]>::from(hash.finalize()) != expected.sha256 {
            return Err(RuntimeError::Asset);
        }
        output.sync_all()?;
        replace(&temporary, &target, &output, budget)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn file_field(key: &str, kind: Option<&str>) -> bool {
    matches!(
        key,
        "path"
            | "certificate"
            | "ca"
            | "ca-file"
            | "client-auth-cert"
            | "ech-key"
            | "planet"
            | "state-dir"
    ) || (key == "private-key" && !matches!(kind, Some("wireguard" | "masque")))
}

fn provider_destination(kind: &str, name: &str, remote: bool) -> String {
    let digest = format!("{:x}", Sha256::digest(format!("{kind}:{name}").as_bytes()));
    if remote {
        format!("cache/providers/{digest}")
    } else {
        format!("providers/{digest}")
    }
}

// Adapt the upstream handle-contention retry model with one stage-wide cap:
// at most three waits (175 ms total), rather than a new allowance per provider.
fn cache_retry_delay(error: &io::Error, index: usize) -> Option<std::time::Duration> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{
            ERROR_DELETE_PENDING, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION,
        };
        if matches!(error.raw_os_error(), Some(code) if code == ERROR_SHARING_VIOLATION as i32 || code == ERROR_LOCK_VIOLATION as i32 || code == ERROR_DELETE_PENDING as i32)
        {
            return [25, 50, 100]
                .get(index)
                .copied()
                .map(std::time::Duration::from_millis);
        }
    }
    let _ = (error, index);
    None
}

fn open_resource(root: &Path, relative: &Path) -> io::Result<File> {
    let mut path = root.to_path_buf();
    for part in relative.components() {
        let Component::Normal(part) = part else {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "invalid declared resource",
            ));
        };
        path.push(part);
        let metadata = fs::symlink_metadata(&path)?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "resource reparse point",
                ));
            }
        }
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "resource symlink",
            ));
        }
    }
    crate::platform::open_pinned_file(&path)
}

fn remap_cached_assets(
    value: &mut Value,
    prior: &RuntimeResources,
    previous: &RuntimeManifest,
    current: &RuntimeManifest,
    directory: &Path,
    depth: usize,
) -> Result<(), RuntimeError> {
    if depth > 64 {
        return Err(RuntimeError::Configuration);
    }
    match value {
        Value::Mapping(mapping) => {
            let kind = mapping
                .get(Value::from("type"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            for (key, value) in mapping {
                // `path` is also an HTTP transport setting. Only resource paths
                // or the explicit file-valued fields below can refer to assets.
                let explicit_file = key
                    .as_str()
                    .is_some_and(|key| key != "path" && file_field(key, kind.as_deref()));
                if let Some(path) = value.as_str()
                    && (explicit_file
                        || (key.as_str() == Some("path")
                            && (path.starts_with("assets/")
                                || Path::new(path).starts_with(&prior.prepared_directory))))
                    && !path.is_empty()
                    && !path.starts_with("-----BEGIN ")
                {
                    let relative = if path.starts_with("assets/") {
                        path.to_owned()
                    } else {
                        Path::new(path)
                            .strip_prefix(&prior.prepared_directory)
                            .map_err(|_| RuntimeError::Asset)?
                            .to_str()
                            .ok_or(RuntimeError::Asset)?
                            .replace('\\', "/")
                    };
                    validate_asset_path(&relative)?;
                    let old = previous.assets.get(&relative).ok_or(RuntimeError::Asset)?;
                    if current.assets.get(&relative) != Some(old) {
                        return Err(RuntimeError::Asset);
                    }
                    *value = Value::from(
                        directory
                            .join(relative)
                            .to_str()
                            .ok_or(RuntimeError::Asset)?,
                    );
                }
                remap_cached_assets(value, prior, previous, current, directory, depth + 1)?;
            }
        }
        Value::Sequence(values) => {
            for value in values {
                remap_cached_assets(value, prior, previous, current, directory, depth + 1)?;
            }
        }
        Value::Tagged(_) => return Err(RuntimeError::Configuration),
        _ => {}
    }
    Ok(())
}

fn merge_mapping(target: &mut Value, patch: Value) -> Result<(), RuntimeError> {
    let target = target.as_mapping_mut().ok_or(RuntimeError::Configuration)?;
    let patch = patch.as_mapping().ok_or(RuntimeError::Configuration)?;
    for (key, value) in patch {
        if value.is_mapping() {
            let entry = target
                .entry(key.clone())
                .or_insert_with(|| Value::Mapping(Mapping::new()));
            merge_mapping(entry, value.clone())?;
        } else {
            target.insert(key.clone(), value.clone());
        }
    }
    Ok(())
}

fn rewrite_asset_paths(value: &mut Value, directory: &Path) -> Result<(), RuntimeError> {
    match value {
        Value::Mapping(mapping) => {
            let kind = mapping
                .get(Value::from("type"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            for (key, value) in mapping {
                if key
                    .as_str()
                    .is_some_and(|key| file_field(key, kind.as_deref()))
                    && let Some(path) = value.as_str()
                    && path.starts_with("assets/")
                {
                    validate_asset_path(path)?;
                    *value = Value::from(directory.join(path).to_str().ok_or(RuntimeError::Asset)?);
                }
                rewrite_asset_paths(value, directory)?;
            }
        }
        Value::Sequence(values) => {
            for value in values {
                rewrite_asset_paths(value, directory)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn validate_asset_path(path: &str) -> Result<(), RuntimeError> {
    if path.is_empty()
        || path.len() > 240
        || path.split('/').count() > 16
        || path.contains(['\\', ':', '\0'])
        || path.starts_with('/')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(RuntimeError::Asset);
    }
    // The client can upload only resources, not controller sockets, active
    // configuration, or files the service manages itself.
    if !path.starts_with("assets/") {
        return Err(RuntimeError::Asset);
    }
    #[cfg(windows)]
    for part in path.split('/') {
        let stem = part
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if part.ends_with([' ', '.'])
            || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit())
        {
            return Err(RuntimeError::Asset);
        }
    }
    Ok(())
}

fn create_relative_parents(root: &Path, path: &Path) -> Result<(), RuntimeError> {
    let mut directory = root.to_path_buf();
    let parent = path.parent().ok_or(RuntimeError::Asset)?;
    for part in parent.components() {
        let Component::Normal(part) = part else {
            return Err(RuntimeError::Asset);
        };
        directory.push(part);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(RuntimeError::Asset),
            Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&directory)?,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn validate_configuration(yaml: &str) -> Result<(Mapping, BTreeSet<String>), RuntimeError> {
    if yaml.len() > MAX_CONFIG_BYTES {
        return Err(RuntimeError::Budget);
    }
    let value: Value = serde_yaml::from_str(yaml).map_err(|_| RuntimeError::Configuration)?;
    let mut configuration = value
        .as_mapping()
        .cloned()
        .ok_or(RuntimeError::Configuration)?;
    let mut required_assets = BTreeSet::new();
    for key in [
        "external-controller",
        "external-controller-tls",
        "external-controller-unix",
        "external-controller-pipe",
        "secret",
        "external-controller-cors",
        "external-ui",
        "external-ui-url",
        "external-ui-name",
        "external-doh-server",
        "profile-debug",
    ] {
        configuration.remove(Value::from(key));
    }
    let mut provider_count = 0_usize;
    for provider_kind in ["proxy-providers", "rule-providers"] {
        if let Some(providers) = configuration.get_mut(Value::from(provider_kind)) {
            let providers = providers
                .as_mapping_mut()
                .ok_or(RuntimeError::Configuration)?;
            provider_count = provider_count
                .checked_add(providers.len())
                .ok_or(RuntimeError::Budget)?;
            if provider_count > MAX_ASSETS {
                return Err(RuntimeError::Budget);
            }
            for provider in providers.values_mut() {
                let provider = provider
                    .as_mapping_mut()
                    .ok_or(RuntimeError::Configuration)?;
                let kind = provider.get(Value::from("type")).and_then(Value::as_str);
                if !matches!(kind, Some("file" | "http" | "inline")) {
                    return Err(RuntimeError::Configuration);
                }
                if let Some(path) = provider.get(Value::from("path")) {
                    let path = path.as_str().ok_or(RuntimeError::Asset)?;
                    validate_asset_path(path)?;
                    if kind == Some("file") {
                        required_assets.insert(path.to_owned());
                    }
                } else if !matches!(kind, Some("inline" | "http")) {
                    return Err(RuntimeError::Asset);
                }
            }
        }
    }
    // A supplied TLS file path is a privileged read even when no provider is
    // involved. Inline PEM values are data and require no filesystem access.
    validate_tls_paths(
        &Value::Mapping(configuration.clone()),
        &mut required_assets,
        0,
    )?;
    Ok((configuration, required_assets))
}

fn validate_tls_paths(
    value: &Value,
    assets: &mut BTreeSet<String>,
    depth: usize,
) -> Result<(), RuntimeError> {
    if depth > 64 {
        return Err(RuntimeError::Configuration);
    }
    match value {
        Value::Mapping(mapping) => {
            for (key, value) in mapping {
                if matches!(
                    key.as_str(),
                    Some(
                        "certificate"
                            | "private-key"
                            | "ca"
                            | "ca-file"
                            | "client-auth-cert"
                            | "ech-key"
                            | "planet"
                            | "state-dir"
                    )
                ) && !(key.as_str() == Some("private-key")
                    && matches!(
                        mapping.get(Value::from("type")).and_then(Value::as_str),
                        Some("wireguard" | "masque")
                    ))
                    && let Some(path) = value.as_str()
                    && !path.is_empty()
                    && !path.starts_with("-----BEGIN ")
                {
                    validate_asset_path(path)?;
                    if key.as_str() != Some("state-dir") {
                        assets.insert(path.to_owned());
                    }
                }
                validate_tls_paths(value, assets, depth + 1)?;
            }
        }
        Value::Sequence(values) => {
            for value in values {
                validate_tls_paths(value, assets, depth + 1)?;
            }
        }
        Value::Tagged(_) => return Err(RuntimeError::Configuration),
        _ => {}
    }
    Ok(())
}

#[path = "runtime_replace.rs"]
mod replacement;
use replacement::{ReplacementBudget, create_temporary, replace};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readback_preserves_opaque_rules_and_never_opens_undeclared_names() {
        let directory = Directory::new();
        let runtime = StagedRuntime::new(directory.0.clone(),
            "rule-providers:\n  r:\n    type: http\n    behavior: domain\n    format: mrs\n    url: https://example.invalid\n").unwrap();
        runtime
            .materialize("private-controller", "private-secret")
            .unwrap();
        let bytes = b"\0MRS\xff\0opaque bytes";
        fs::write(provider_cache(&runtime, "rule-providers", "r"), bytes).unwrap();
        assert!(
            runtime
                .provider_cache_source(crate::protocol::ProviderKind::Rule, "../../runtime.yaml")
                .is_err()
        );
        assert!(
            runtime
                .provider_cache_source(crate::protocol::ProviderKind::Proxy, "r")
                .is_err()
        );
        let (result, scanned) = runtime
            .provider_cache_source(crate::protocol::ProviderKind::Rule, "r")
            .unwrap()
            .snapshot(
                MAX_ASSET_BYTES as usize,
                std::time::Instant::now() + std::time::Duration::from_secs(3),
            );
        assert_eq!(result.unwrap().unwrap(), bytes);
        assert_eq!(scanned, bytes.len());
    }

    #[test]
    fn readback_proxy_references_return_only_approved_logical_assets() {
        let directory = Directory::new();
        let mut runtime = StagedRuntime::new(
            directory.0.clone(),
            "proxy-providers:\n  p:\n    type: http\n    url: https://example.invalid\n",
        )
        .unwrap();
        runtime
            .upload("assets/ca.pem", 0, b"approved cert", true)
            .unwrap();
        runtime
            .materialize("private-controller", "private-secret")
            .unwrap();
        let cache = provider_cache(&runtime, "proxy-providers", "p");
        let nodes = serde_yaml::to_string(&serde_json::json!({"proxies":[{
            "name":"n", "type":"ss", "ca":runtime.prepared_directory().join("assets/ca.pem").to_str().unwrap(),
            "ws-opts":{"path":"/transport-path"}
        }]})).unwrap();
        fs::write(&cache, &nodes).unwrap();
        let (result, scanned) = runtime
            .provider_cache_source(crate::protocol::ProviderKind::Proxy, "p")
            .unwrap()
            .snapshot(
                MAX_CONFIG_BYTES,
                std::time::Instant::now() + std::time::Duration::from_secs(3),
            );
        let bytes = result.unwrap().unwrap();
        let value: Value = serde_yaml::from_slice(&bytes).unwrap();
        assert_eq!(value["proxies"][0]["ca"].as_str(), Some("assets/ca.pem"));
        assert_eq!(
            value["proxies"][0]["ws-opts"]["path"].as_str(),
            Some("/transport-path")
        );
        assert_eq!(scanned, nodes.len());
        assert!(
            !String::from_utf8(bytes)
                .unwrap()
                .contains(runtime.prepared_directory().to_str().unwrap())
        );
        fs::write(
            &cache,
            "proxies: [{name: n, type: ss, ca: /system/private/file}]\n",
        )
        .unwrap();
        let (result, scanned) = runtime
            .provider_cache_source(crate::protocol::ProviderKind::Proxy, "p")
            .unwrap()
            .snapshot(
                MAX_CONFIG_BYTES,
                std::time::Instant::now() + std::time::Duration::from_secs(3),
            );
        assert!(matches!(result, Err(RuntimeError::Asset)));
        assert!(
            scanned > 0,
            "rejected parsing still consumes the scan budget"
        );
    }

    #[test]
    fn provenance_rejects_same_length_changed_completed_upload() {
        let directory = Directory::new();
        let mut runtime =
            StagedRuntime::new(directory.0.clone(), "tls:\n  ca: assets/ca.pem\n").unwrap();
        runtime
            .upload("assets/ca.pem", 0, b"approved", true)
            .unwrap();
        fs::write(directory.0.join("assets/ca.pem"), b"tampered").unwrap();
        assert!(matches!(
            runtime.materialize("controller", "secret"),
            Err(RuntimeError::Asset)
        ));
        assert!(!directory.0.join("runtime.yaml").exists());
    }

    #[test]
    fn provenance_rejects_grown_completed_upload_before_prepared_copy() {
        let directory = Directory::new();
        let mut runtime =
            StagedRuntime::new(directory.0.clone(), "tls:\n  ca: assets/ca.pem\n").unwrap();
        runtime
            .upload("assets/ca.pem", 0, b"approved", true)
            .unwrap();
        fs::write(directory.0.join("assets/ca.pem"), b"larger unapproved file").unwrap();
        assert!(matches!(
            runtime.materialize("controller", "secret"),
            Err(RuntimeError::Asset)
        ));
        assert!(!runtime.prepared_directory().join("assets/ca.pem").exists());
    }

    #[test]
    fn cache_inheritance_copies_fresh_bytes_without_mutating_the_accepted_cache() {
        let old_directory = Directory::new();
        let new_directory = Directory::new();
        let yaml = "rule-providers:\n  rules:\n    type: http\n    behavior: classical\n    url: https://example.invalid/rules\n";
        let old = StagedRuntime::new(old_directory.0.clone(), yaml).unwrap();
        let old_config = old.materialize("controller", "secret").unwrap();
        let value: Value = serde_yaml::from_slice(&fs::read(old_config).unwrap()).unwrap();
        let old_cache = PathBuf::from(value["rule-providers"]["rules"]["path"].as_str().unwrap());
        fs::write(&old_cache, b"fresh native cache bytes").unwrap();
        let mut candidate = StagedRuntime::new(new_directory.0.clone(), yaml).unwrap();
        candidate.inherit_resources(&old);
        let config = candidate.materialize("controller2", "secret2").unwrap();
        let value: Value = serde_yaml::from_slice(&fs::read(config).unwrap()).unwrap();
        let cache = PathBuf::from(value["rule-providers"]["rules"]["path"].as_str().unwrap());
        assert_eq!(fs::read(&cache).unwrap(), b"fresh native cache bytes");
        fs::write(cache, b"new candidate cache").unwrap();
        assert_eq!(fs::read(old_cache).unwrap(), b"fresh native cache bytes");
    }

    fn provider_cache(runtime: &StagedRuntime, kind: &str, name: &str) -> PathBuf {
        runtime
            .prepared_directory()
            .join(provider_destination(kind, name, true))
    }

    #[test]
    fn cache_inheritance_changed_url_cold_fetch_keeps_the_accepted_and_partial_group() {
        let old_directory = Directory::new();
        let new_directory = Directory::new();
        let yaml = "rule-providers:\n  r:\n    type: http\n    behavior: classical\n    url: https://old.invalid\n";
        let old = StagedRuntime::new(old_directory.0.clone(), yaml).unwrap();
        old.materialize("controller", "secret").unwrap();
        let cache = provider_cache(&old, "rule-providers", "r");
        fs::write(&cache, b"accepted bytes").unwrap();
        let partial_directory = Directory::new();
        let partial = old
            .fork_patch(
                partial_directory.0.clone(),
                &serde_json::json!({"mode":"global"}),
            )
            .unwrap();
        let mut candidate = StagedRuntime::new(
            new_directory.0.clone(),
            &yaml.replace("old.invalid", "new.invalid"),
        )
        .unwrap();
        candidate.inherit_resources(&partial);
        candidate.materialize("controller2", "secret2").unwrap();
        assert!(!provider_cache(&candidate, "rule-providers", "r").exists());
        assert_eq!(fs::read(&cache).unwrap(), b"accepted bytes");
        let key = provider_destination("rule-providers", "r", true);
        assert_eq!(
            candidate
                .resources
                .manifest
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .cache_dispositions[&key],
            CacheDisposition::ChangedUrl
        );
        assert_eq!(Arc::strong_count(&old.resources), 2);
        assert!(old.into_retired().directories(&[&partial]).is_empty());
    }

    #[test]
    fn cache_inheritance_remaps_only_content_identical_approved_node_assets() {
        let old_directory = Directory::new();
        let new_directory = Directory::new();
        let yaml = "proxy-providers:\n  p:\n    type: http\n    url: https://nodes.invalid\n";
        let mut old = StagedRuntime::new(old_directory.0.clone(), yaml).unwrap();
        old.upload("assets/ca.pem", 0, b"approved cert", true)
            .unwrap();
        old.materialize("controller", "secret").unwrap();
        let old_cache = provider_cache(&old, "proxy-providers", "p");
        let nodes = serde_yaml::to_string(&serde_json::json!({"proxies":[{"name":"node","type":"ss","ca":old.prepared_directory().join("assets/ca.pem").to_str().unwrap()}]})).unwrap();
        fs::write(&old_cache, &nodes).unwrap();
        let mut candidate = StagedRuntime::new(new_directory.0.clone(), yaml).unwrap();
        candidate
            .upload("assets/ca.pem", 0, b"approved cert", true)
            .unwrap();
        candidate.inherit_resources(&old);
        candidate.materialize("controller2", "secret2").unwrap();
        let copied: Value = serde_yaml::from_slice(
            &fs::read(provider_cache(&candidate, "proxy-providers", "p")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            copied["proxies"][0]["ca"].as_str().unwrap(),
            candidate
                .prepared_directory()
                .join("assets/ca.pem")
                .to_str()
                .unwrap()
        );
        assert_eq!(fs::read_to_string(old_cache).unwrap(), nodes);
    }

    #[test]
    fn cache_inheritance_changed_asset_cold_fetch_does_not_reuse_old_node_identity() {
        let old_directory = Directory::new();
        let new_directory = Directory::new();
        let yaml = "proxy-providers:\n  p:\n    type: http\n    url: https://nodes.invalid\n";
        let mut old = StagedRuntime::new(old_directory.0.clone(), yaml).unwrap();
        old.upload("assets/ca.pem", 0, b"old cert", true).unwrap();
        old.materialize("controller", "secret").unwrap();
        let old_cache = provider_cache(&old, "proxy-providers", "p");
        let nodes = b"proxies:\n  - {name: node, type: ss, ca: assets/ca.pem}\n";
        fs::write(&old_cache, nodes).unwrap();
        let mut candidate = StagedRuntime::new(new_directory.0.clone(), yaml).unwrap();
        candidate
            .upload("assets/ca.pem", 0, b"new cert", true)
            .unwrap();
        candidate.inherit_resources(&old);
        candidate.materialize("controller2", "secret2").unwrap();
        assert!(!provider_cache(&candidate, "proxy-providers", "p").exists());
        assert_eq!(fs::read(old_cache).unwrap(), nodes);
        assert_eq!(
            candidate
                .resources
                .manifest
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .cache_dispositions[&provider_destination("proxy-providers", "p", true)],
            CacheDisposition::ResourceChanged
        );
    }

    #[test]
    fn cache_inheritance_unknown_system_path_is_never_opened_or_copied() {
        let old_directory = Directory::new();
        let new_directory = Directory::new();
        let secret_directory = Directory::new();
        let secret = secret_directory.0.join("secret");
        fs::write(&secret, b"private material").unwrap();
        let yaml = "proxy-providers:\n  p:\n    type: http\n    url: https://nodes.invalid\n";
        let old = StagedRuntime::new(old_directory.0.clone(), yaml).unwrap();
        old.materialize("controller", "secret").unwrap();
        let cache = provider_cache(&old, "proxy-providers", "p");
        fs::write(&cache, serde_yaml::to_string(&serde_json::json!({"proxies":[{"name":"node","type":"ss","ca":secret.to_str().unwrap()}]})).unwrap()).unwrap();
        let mut candidate = StagedRuntime::new(new_directory.0.clone(), yaml).unwrap();
        candidate.inherit_resources(&old);
        candidate.materialize("controller2", "secret2").unwrap();
        assert!(!provider_cache(&candidate, "proxy-providers", "p").exists());
        assert_eq!(fs::read(secret).unwrap(), b"private material");
    }

    #[test]
    fn cache_inheritance_parse_budget_does_not_lower_native_download_limit() {
        let old_directory = Directory::new();
        let new_directory = Directory::new();
        let yaml = "proxy-providers:\n  p:\n    type: http\n    url: https://nodes.invalid\n";
        let old = StagedRuntime::new(old_directory.0.clone(), yaml).unwrap();
        old.materialize("controller", "secret").unwrap();
        let cache = provider_cache(&old, "proxy-providers", "p");
        File::create(&cache)
            .unwrap()
            .set_len(MAX_CONFIG_BYTES as u64 + 1)
            .unwrap();
        let mut candidate = StagedRuntime::new(new_directory.0.clone(), yaml).unwrap();
        candidate.inherit_resources(&old);
        let configuration = candidate.materialize("controller2", "secret2").unwrap();
        let value: Value = serde_yaml::from_slice(&fs::read(configuration).unwrap()).unwrap();
        assert_eq!(
            value["proxy-providers"]["p"]["size-limit"].as_u64(),
            Some(MAX_ASSET_BYTES)
        );
        assert!(!provider_cache(&candidate, "proxy-providers", "p").exists());
        assert_eq!(
            candidate
                .resources
                .manifest
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .cache_dispositions[&provider_destination("proxy-providers", "p", true)],
            CacheDisposition::ParseBudget
        );
        assert_eq!(
            fs::metadata(cache).unwrap().len(),
            MAX_CONFIG_BYTES as u64 + 1
        );
    }

    #[test]
    fn provenance_validation_then_additional_upload_updates_only_completed_manifest() {
        let directory = Directory::new();
        let mut runtime = StagedRuntime::new(directory.0.clone(), "mode: rule").unwrap();
        runtime.upload("assets/a", 0, b"first", true).unwrap();
        runtime.materialize_validation().unwrap().cleanup().unwrap();
        runtime.upload("assets/b", 0, b"sec", false).unwrap();
        assert!(matches!(
            runtime.materialize("controller", "secret"),
            Err(RuntimeError::Incomplete)
        ));
        assert!(
            !runtime
                .resources
                .manifest
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .assets
                .contains_key("assets/b")
        );
        runtime.upload("assets/b", 3, b"ond", true).unwrap();
        runtime.materialize("controller", "secret").unwrap();
        assert_eq!(
            runtime
                .resources
                .manifest
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .assets["assets/b"],
            SourceIdentity {
                len: 6,
                sha256: Sha256::digest(b"second").into()
            }
        );
    }

    #[test]
    fn a_committed_partial_can_restore_its_held_geodata_after_a_later_reload() {
        let directory = Directory::new();
        let home = Directory::new();
        let mut runtime = StagedRuntime::new(directory.0.clone(), "mode: rule").unwrap();
        runtime
            .upload("assets/geodata/GeoIP.dat", 0, b"accepted geo data", true)
            .unwrap();
        runtime.materialize("controller", "secret").unwrap();
        let config = directory.0.join("partial-config");
        fs::create_dir(&config).unwrap();
        let partial = runtime
            .fork_patch(config, &serde_json::json!({"mode":"global"}))
            .unwrap();
        runtime.install_geodata(&home.0).unwrap();
        assert_eq!(
            fs::read(home.0.join("GeoIP.dat")).unwrap(),
            b"accepted geo data"
        );
        fs::write(home.0.join("GeoIP.dat"), b"later uncommitted geo data").unwrap();
        partial.install_geodata(&home.0).unwrap();
        assert_eq!(
            fs::read(home.0.join("GeoIP.dat")).unwrap(),
            b"accepted geo data"
        );
    }

    #[test]
    fn validation_keeps_materialized_controller_and_resources_unchanged() {
        let directory = Directory::new();
        let mut runtime = StagedRuntime::new(
            directory.0.clone(),
            "tls:\n  certificate: assets/cert.pem\n",
        )
        .unwrap();
        runtime
            .upload("assets/cert.pem", 0, b"original certificate", true)
            .unwrap();
        let config = runtime
            .materialize("running-controller", "running-secret")
            .unwrap();
        let before = fs::read(&config).unwrap();
        let source = fs::read(directory.0.join("assets/cert.pem")).unwrap();
        let prepared = runtime.prepared_directory().join("assets/cert.pem");
        fs::write(&prepared, b"prepared runtime mutation").unwrap();
        let validation = runtime.materialize_validation().unwrap();
        assert_eq!(fs::read(&config).unwrap(), before);
        assert_eq!(fs::read(&prepared).unwrap(), b"prepared runtime mutation");
        assert_eq!(
            fs::read(directory.0.join("assets/cert.pem")).unwrap(),
            source
        );
        assert_ne!(validation.path(), config);
        let value: Value = serde_yaml::from_slice(&fs::read(validation.path()).unwrap()).unwrap();
        assert_eq!(value["secret"], "");
        assert_eq!(value["external-controller"], "");
        #[cfg(windows)]
        assert_eq!(value["external-controller-pipe"], "");
        #[cfg(unix)]
        assert_eq!(value["external-controller-unix"], "");
    }

    #[test]
    fn validation_drop_cleans_its_configuration_without_deleting_runtime() {
        let directory = Directory::new();
        let runtime = StagedRuntime::new(directory.0.clone(), "mode: rule").unwrap();
        let runtime_path = runtime.materialize("controller", "secret").unwrap();
        let validation = runtime.materialize_validation().unwrap();
        let validation_path = validation.path().to_path_buf();
        drop(validation);
        assert!(!validation_path.exists());
        assert!(runtime_path.exists());
    }

    #[tokio::test]
    async fn cancelling_validation_cleans_the_private_file() {
        let directory = Directory::new();
        let runtime = StagedRuntime::new(directory.0.clone(), "mode: rule").unwrap();
        let validation = runtime.materialize_validation().unwrap();
        let path = validation.path().to_path_buf();
        let (ready, started) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _guard = validation;
            ready.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        started.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(!path.exists());
        assert!(!directory.0.join("runtime.yaml").exists());
    }

    #[test]
    fn validation_cleanup_failure_preserves_evidence_and_rejects_replacement() {
        let directory = Directory::new();
        let runtime = StagedRuntime::new(directory.0.clone(), "mode: rule").unwrap();
        let validation = runtime.materialize_validation().unwrap();
        let path = validation.path().to_path_buf();
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(matches!(
            validation.cleanup(),
            Err(RuntimeError::Storage(_))
        ));
        assert!(path.is_dir());
        assert!(matches!(
            runtime.materialize_validation(),
            Err(RuntimeError::Storage(_))
        ));
        assert!(path.is_dir());
    }

    #[test]
    fn validation_rejects_oversized_or_nonregular_materialized_configuration() {
        let directory = Directory::new();
        let runtime = StagedRuntime::new(directory.0.clone(), "mode: rule").unwrap();
        let path = directory.0.join("runtime.yaml");
        fs::write(&path, vec![b' '; MAX_CONFIG_BYTES + 1]).unwrap();
        assert!(matches!(
            runtime.materialize_validation(),
            Err(RuntimeError::Budget)
        ));
        assert!(!directory.0.join("validation.yaml").exists());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(matches!(
            runtime.materialize_validation(),
            Err(RuntimeError::Asset)
        ));
        assert!(!directory.0.join("validation.yaml").exists());
    }

    #[test]
    fn validation_preserves_live_http_provider_cache() {
        let directory = Directory::new();
        let runtime = StagedRuntime::new(
            directory.0.clone(),
            "proxy-providers:\n  remote:\n    type: http\n    url: https://example.test/nodes\n",
        )
        .unwrap();
        let config = runtime.materialize("controller", "secret").unwrap();
        let value: Value = serde_yaml::from_slice(&fs::read(&config).unwrap()).unwrap();
        let cache = PathBuf::from(value["proxy-providers"]["remote"]["path"].as_str().unwrap());
        fs::write(&cache, b"updated provider bytes").unwrap();
        let validation = runtime.materialize_validation().unwrap();
        assert_eq!(fs::read(&cache).unwrap(), b"updated provider bytes");
        let validated: Value =
            serde_yaml::from_slice(&fs::read(validation.path()).unwrap()).unwrap();
        assert_eq!(validated["proxy-providers"], value["proxy-providers"]);
        validation.cleanup().unwrap();
    }

    #[test]
    fn http_cache_limits_share_the_stage_budget_and_preserve_smaller_user_limits() {
        let directory = Directory::new();
        let runtime = StagedRuntime::new(directory.0.clone(), "proxy-providers:\n  first:\n    type: http\n    url: https://example.test/a\n    size-limit: 1024\n  second:\n    type: http\n    url: https://example.test/b\n    size-limit: 0\n").unwrap();
        let path = runtime.materialize("controller", "secret").unwrap();
        let value: Value = serde_yaml::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["proxy-providers"]["first"]["size-limit"], 1024);
        let second = value["proxy-providers"]["second"]["size-limit"]
            .as_u64()
            .unwrap();
        assert!(second > 0 && second <= (MAX_RUNTIME_BYTES - 2 * MAX_CONFIG_BYTES as u64) / 2);
        runtime.materialize("controller", "secret").unwrap();
        let repeated: Value = serde_yaml::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(repeated["proxy-providers"]["second"]["size-limit"], second);
    }

    #[test]
    fn too_many_providers_are_rejected_before_creating_any_cache() {
        let mut yaml = String::from("proxy-providers:\n");
        for index in 0..=MAX_ASSETS {
            yaml.push_str(&format!(
                "  p{index}:\n    type: http\n    url: https://example.test/{index}\n"
            ));
        }
        assert!(validate_configuration(&yaml).is_err());
    }

    #[test]
    fn separated_runtime_configuration_never_sits_under_the_approved_asset_root() {
        let directory = Directory::new();
        for name in ["assets", "snapshots", "configurations"] {
            fs::create_dir(directory.0.join(name)).unwrap();
        }
        let snapshot = directory.0.join("snapshots/revision-1");
        let configurations = directory.0.join("configurations/revision-1");
        fs::create_dir(&snapshot).unwrap();
        fs::create_dir(&configurations).unwrap();
        let mut runtime = StagedRuntime::new_with_configuration(
            snapshot.clone(),
            configurations.clone(),
            "tls:\n  certificate: assets/cert.pem\n",
        )
        .unwrap();
        runtime.upload("assets/cert.pem", 0, b"cert", true).unwrap();
        let config = runtime.materialize("controller", "private-secret").unwrap();
        let value: Value = serde_yaml::from_slice(&fs::read(&config).unwrap()).unwrap();
        let certificate = Path::new(value["tls"]["certificate"].as_str().unwrap());
        assert!(config.starts_with(configurations));
        assert!(!config.starts_with(directory.0.join("assets")));
        assert!(certificate.starts_with(directory.0.join("assets/revision-1")));
        assert!(!certificate.starts_with(snapshot));
        fs::write(certificate, b"runtime mutation").unwrap();
        runtime.materialize("controller", "new-secret").unwrap();
        assert_eq!(fs::read(certificate).unwrap(), b"cert");
    }

    #[test]
    fn remote_providers_without_an_explicit_path_get_a_private_cache_and_keep_overrides() {
        let directory = Directory::new();
        let runtime = StagedRuntime::new(directory.0.clone(), "proxy-providers:\n  remote:\n    type: http\n    url: https://example.test/nodes\n    interval: 60\n    proxy: DIRECT\n    override:\n      override-expr: ['.udp = true']\n").unwrap();
        let path = runtime.materialize("controller", "secret").unwrap();
        let main: Value = serde_yaml::from_slice(&fs::read(path).unwrap()).unwrap();
        assert!(
            Path::new(main["proxy-providers"]["remote"]["path"].as_str().unwrap()).is_absolute()
        );
        assert_eq!(main["proxy-providers"]["remote"]["proxy"], "DIRECT");
        assert_eq!(
            main["proxy-providers"]["remote"]["override"]["override-expr"][0],
            ".udp = true"
        );
    }

    #[test]
    fn prepared_provider_never_accepts_a_missing_certificate_resource() {
        let directory = Directory::new();
        let mut runtime = StagedRuntime::new(
            directory.0.clone(),
            "proxy-providers:\n  local:\n    type: file\n    path: assets/provider.yaml\n",
        )
        .unwrap();
        runtime
            .upload(
                "assets/provider.yaml",
                0,
                b"proxies:\n- name: ssh\n  type: ssh\n  private-key: assets/missing.pem\n",
                true,
            )
            .unwrap();
        assert!(matches!(
            runtime.materialize("controller", "secret"),
            Err(RuntimeError::Incomplete)
        ));
    }

    #[test]
    fn local_provider_nodes_are_rewritten_without_modifying_the_uploaded_snapshot() {
        let directory = Directory::new();
        let mut runtime = StagedRuntime::new(
            directory.0.clone(),
            "proxy-providers:\n  local:\n    type: file\n    path: assets/provider.yaml\n",
        )
        .unwrap();
        let original = b"proxies:\n- name: ssh\n  type: ssh\n  private-key: assets/ssh.pem\n";
        runtime
            .upload("assets/provider.yaml", 0, original, true)
            .unwrap();
        runtime
            .upload("assets/ssh.pem", 0, b"key bytes", true)
            .unwrap();
        let path = runtime.materialize("controller", "secret").unwrap();
        let main: Value = serde_yaml::from_slice(&fs::read(path).unwrap()).unwrap();
        let prepared = main["proxy-providers"]["local"]["path"].as_str().unwrap();
        let nodes: Value = serde_yaml::from_slice(&fs::read(prepared).unwrap()).unwrap();
        assert!(Path::new(nodes["proxies"][0]["private-key"].as_str().unwrap()).is_absolute());
        assert_eq!(
            fs::read(directory.0.join("assets/provider.yaml")).unwrap(),
            original
        );
        fs::write(prepared, b"changed working copy").unwrap();
        runtime.materialize("controller", "new-secret").unwrap();
        assert_eq!(
            fs::read(directory.0.join("assets/provider.yaml")).unwrap(),
            original
        );
        assert!(
            serde_yaml::from_slice::<Value>(&fs::read(prepared).unwrap()).unwrap()["proxies"]
                .is_sequence()
        );
    }

    #[test]
    fn provider_nodes_cannot_introduce_host_reads_or_reference_missing_uploads() {
        for key in ["private-key", "certificate", "planet"] {
            let directory = Directory::new();
            let mut runtime = StagedRuntime::new(
                directory.0.clone(),
                "proxy-providers:\n  local:\n    type: file\n    path: assets/provider.yaml\n",
            )
            .unwrap();
            let nodes = format!("proxies:\n- name: node\n  type: ssh\n  {key}: /etc/secret\n");
            runtime
                .upload("assets/provider.yaml", 0, nodes.as_bytes(), true)
                .unwrap();
            assert!(
                runtime.materialize("controller", "secret").is_err(),
                "accepted {key}"
            );
        }
    }

    #[test]
    fn http_provider_cache_does_not_reuse_uploaded_files_or_disable_refresh() {
        let directory = Directory::new();
        let mut runtime = StagedRuntime::new(directory.0.clone(), "proxy-providers:\n  remote:\n    type: http\n    url: https://example.test/nodes\n    interval: 60\n    path: assets/provider.yaml\n").unwrap();
        runtime
            .upload("assets/provider.yaml", 0, b"proxies: []", true)
            .unwrap();
        let path = runtime.materialize("controller", "secret").unwrap();
        let main: Value = serde_yaml::from_slice(&fs::read(path).unwrap()).unwrap();
        let cache = Path::new(main["proxy-providers"]["remote"]["path"].as_str().unwrap());
        assert_ne!(cache, directory.0.join("assets/provider.yaml"));
        fs::write(cache, b"provider update").unwrap();
        assert_eq!(
            fs::read(directory.0.join("assets/provider.yaml")).unwrap(),
            b"proxies: []"
        );
        assert_eq!(main["proxy-providers"]["remote"]["type"], "http");
        assert_eq!(main["proxy-providers"]["remote"]["interval"], 60);
    }

    #[test]
    fn materialized_provider_paths_remain_bound_to_their_own_stage_after_reload() {
        let directory = Directory::new();
        let mut runtime = StagedRuntime::new(
            directory.0.clone(),
            "proxy-providers:\n  local:\n    type: file\n    path: assets/proxy.yaml\n",
        )
        .unwrap();
        runtime
            .upload("assets/proxy.yaml", 0, b"proxies: []", true)
            .unwrap();
        let config = runtime.materialize("controller", "secret").unwrap();
        let value: Value = serde_yaml::from_slice(&fs::read(config).unwrap()).unwrap();
        assert_eq!(
            value["proxy-providers"]["local"]["path"].as_str().unwrap(),
            runtime
                .prepared_directory()
                .join(format!(
                    "providers/{:x}",
                    Sha256::digest(b"proxy-providers:local")
                ))
                .to_str()
                .unwrap()
        );
    }

    #[test]
    fn all_global_tls_file_fields_reject_host_paths_and_sequences_are_checked() {
        for field in ["client-auth-cert", "ech-key"] {
            let yaml = format!("tls:\n  {field}: /etc/secret");
            assert!(validate_configuration(&yaml).is_err(), "accepted {field}");
        }
    }

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let mut random = [0_u8; 8];
            getrandom::fill(&mut random).unwrap();
            let path = std::env::temp_dir()
                .join(format!("zenclash-stage-{:x}", u64::from_le_bytes(random)));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn paths_cannot_escape_the_resource_namespace() {
        for path in [
            "",
            "../x",
            "assets/../x",
            "assets//x",
            "/etc/shadow",
            "C:/x",
            "assets/x:stream",
            "assets\\x",
            "runtime.yaml",
        ] {
            assert!(validate_asset_path(path).is_err(), "accepted {path}");
        }
        assert!(validate_asset_path("assets/providers/nodes.yaml").is_ok());
    }

    #[test]
    fn an_unfinished_or_missing_file_provider_cannot_start() {
        let directory = Directory::new();
        let mut runtime = StagedRuntime::new(
            directory.0.clone(),
            "proxy-providers:\n  local:\n    type: file\n    path: assets/proxy.yaml\n",
        )
        .unwrap();
        assert!(matches!(
            runtime.materialize("controller", "secret"),
            Err(RuntimeError::Incomplete)
        ));
        runtime
            .upload("assets/proxy.yaml", 0, b"proxies: []", false)
            .unwrap();
        assert!(matches!(
            runtime.materialize("controller", "secret"),
            Err(RuntimeError::Incomplete)
        ));
        runtime
            .upload("assets/proxy.yaml", 11, b"\n", true)
            .unwrap();
        assert!(runtime.materialize("controller", "secret").is_ok());
    }

    #[test]
    fn rejected_offsets_do_not_modify_an_uploaded_resource() {
        let directory = Directory::new();
        let mut runtime = StagedRuntime::new(directory.0.clone(), "mode: rule").unwrap();
        runtime.upload("assets/a", 0, b"first", false).unwrap();
        assert!(runtime.upload("assets/a", 0, b"replacement", true).is_err());
        runtime.upload("assets/a", 5, b"second", true).unwrap();
        assert_eq!(
            fs::read(directory.0.join("assets/a")).unwrap(),
            b"firstsecond"
        );
        assert!(runtime.upload("assets/a", 11, b"third", true).is_err());
    }

    #[test]
    fn a_storage_failure_cannot_be_retried_into_an_accepted_stage() {
        let directory = Directory::new();
        let mut runtime = StagedRuntime::new(directory.0.clone(), "mode: rule").unwrap();
        runtime.upload("assets/a", 0, b"first", false).unwrap();
        runtime.uploads.get_mut("assets/a").unwrap().file =
            Some(File::open(directory.0.join("assets/a")).unwrap());
        assert!(matches!(
            runtime.upload("assets/a", 5, b"next", true),
            Err(RuntimeError::Storage(_))
        ));
        assert!(matches!(
            runtime.upload("assets/a", 5, b"", true),
            Err(RuntimeError::Incomplete)
        ));
        assert!(matches!(
            runtime.materialize("controller", "secret"),
            Err(RuntimeError::Incomplete)
        ));
    }

    #[test]
    fn remote_controllers_and_ui_are_removed_before_privileged_launch() {
        let directory = Directory::new();
        let runtime = StagedRuntime::new(directory.0.clone(), "external-controller: 0.0.0.0:9090\nexternal-controller-tls: 0.0.0.0:9091\nexternal-doh-server: /dns-query\nexternal-ui: /etc\nsecret: user-secret\nmode: rule\n").unwrap();
        let path = runtime
            .materialize("private-controller", "private-secret")
            .unwrap();
        let value: Value = serde_yaml::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(value["external-controller"], "");
        assert_eq!(value["secret"], "private-secret");
        assert!(value.get("external-ui").is_none());
        assert!(value.get("external-controller-tls").is_none());
        assert!(value.get("external-doh-server").is_none());
    }

    #[test]
    fn a_stopped_stage_can_materialize_again_with_fresh_controller_credentials() {
        let directory = Directory::new();
        let runtime = StagedRuntime::new(directory.0.clone(), "mode: rule").unwrap();
        runtime
            .materialize("first-controller", "first-secret")
            .unwrap();
        let path = runtime
            .materialize("second-controller", "second-secret")
            .unwrap();
        let value: Value = serde_yaml::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(value["secret"], "second-secret");
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    }

    #[test]
    fn tls_and_provider_paths_cannot_read_privileged_host_files() {
        for config in [
            "proxy-providers:\n  local:\n    type: file\n    path: /etc/shadow",
            "proxies:\n- name: secret\n  certificate: /etc/shadow",
            "proxies:\n- name: secret\n  private-key: ../../secret",
        ] {
            assert!(validate_configuration(config).is_err());
        }
    }

    #[test]
    fn oversized_upload_chunks_do_not_create_a_resource_file() {
        let directory = Directory::new();
        let mut runtime = StagedRuntime::new(directory.0.clone(), "mode: rule").unwrap();
        assert!(matches!(
            runtime.upload("assets/a", 0, &vec![0; ASSET_CHUNK_BYTES + 1], true),
            Err(RuntimeError::Budget)
        ));
        assert!(!directory.0.join("assets/a").exists());
    }
}
