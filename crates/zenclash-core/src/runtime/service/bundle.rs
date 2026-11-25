//! User-authority preparation of bounded service runtime resources.

use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde_yaml::Value;

use crate::{MihomoError, MihomoResult};

mod local_geodata;
pub use local_geodata::LocalGeoDataRecovery;
mod local_runtime;
mod native_snapshot;
pub(crate) use native_snapshot::FrozenRuntime;

pub(crate) const MAX_ASSET_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;
const MAX_ASSETS: usize = 256;
const MAX_CONFIG_BYTES: usize = 4 * 1024 * 1024;
const GEODATA: [&str; 3] = ["GeoIP.dat", "geosite.dat", "ASN.mmdb"];

#[derive(Clone)]
struct RuntimeAsset {
    path: String,
    bytes: Arc<[u8]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProviderKind {
    Proxy,
    Rule,
}

pub(crate) struct CacheProvider {
    pub(crate) kind: ProviderKind,
    pub(crate) path: String,
}

/// An immutable resource snapshot prepared entirely with ordinary user authority.
/// Source files remain unchanged; staged YAML only names relative service assets.
#[derive(Clone)]
pub struct ServiceRuntimeBundle {
    yaml: String,
    assets: Vec<RuntimeAsset>,
}

impl std::fmt::Debug for ServiceRuntimeBundle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceRuntimeBundle")
            .field("yaml_bytes", &self.yaml.len())
            .field("asset_count", &self.assets.len())
            .finish_non_exhaustive()
    }
}

impl ServiceRuntimeBundle {
    /// Reads and rewrites provider, TLS and named GeoData resources in a worker.
    /// Missing required files or any byte/count budget violation reject the bundle.
    ///
    /// # Errors
    /// Returns input, resource-read, serialization or background-worker errors.
    pub async fn prepare(payload: &str, source_home: PathBuf) -> MihomoResult<Self> {
        if payload.len() > MAX_CONFIG_BYTES {
            return Err(invalid("Service runtime YAML exceeds its byte budget"));
        }
        let payload = payload.to_owned();
        tokio::task::spawn_blocking(move || prepare_bundle(&payload, &source_home))
            .await
            .map_err(|_| MihomoError::Process("Service resource worker failed".into()))?
    }

    /// Returns the rewritten YAML containing only the prepared relative assets.
    #[must_use]
    pub fn yaml(&self) -> &str {
        &self.yaml
    }

    pub(crate) fn controller_secret(&self) -> MihomoResult<String> {
        let value: Value = serde_yaml::from_str(&self.yaml)
            .map_err(|_| invalid("Invalid prepared service runtime YAML"))?;
        match value.get("secret") {
            None | Some(Value::Null) => Ok(String::new()),
            Some(Value::String(secret)) => Ok(secret.clone()),
            Some(_) => Err(invalid("Mihomo controller secret must be a string")),
        }
    }

    pub(crate) fn cache_providers(&self) -> MihomoResult<Vec<CacheProvider>> {
        let value: Value = serde_yaml::from_str(&self.yaml)
            .map_err(|_| invalid("Invalid accepted service runtime YAML"))?;
        let mut providers = Vec::new();
        for (field, kind) in [
            ("proxy-providers", ProviderKind::Proxy),
            ("rule-providers", ProviderKind::Rule),
        ] {
            let Some(mapping) = value.get(field) else {
                continue;
            };
            let mapping = mapping
                .as_mapping()
                .ok_or_else(|| invalid("Invalid provider mapping"))?;
            for (name, provider) in mapping {
                let provider = provider
                    .as_mapping()
                    .ok_or_else(|| invalid("Invalid provider definition"))?;
                if provider
                    .get(Value::from("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("http")
                    != "http"
                {
                    continue;
                }
                name.as_str()
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| invalid("Invalid provider name"))?;
                let path = provider
                    .get(Value::from("path"))
                    .and_then(Value::as_str)
                    .filter(|path| path.starts_with("assets/providers/"))
                    .ok_or_else(|| invalid("Invalid prepared cache destination"))?;
                providers.push(CacheProvider {
                    kind,
                    path: path.to_owned(),
                });
                if providers.len() > MAX_ASSETS {
                    return Err(invalid("Service provider count exceeds its budget"));
                }
            }
        }
        Ok(providers)
    }

    pub(crate) fn cache_export_base(&self, providers: &[CacheProvider]) -> Self {
        let mut bundle = self.clone();
        bundle
            .assets
            .retain(|asset| !providers.iter().any(|provider| provider.path == asset.path));
        bundle
    }

    pub(crate) fn replace_provider_cache(
        &mut self,
        provider: &CacheProvider,
        bytes: Option<Vec<u8>>,
    ) -> MihomoResult<()> {
        self.assets.retain(|asset| asset.path != provider.path);
        if let Some(bytes) = bytes {
            if self.assets.len() >= MAX_ASSETS
                || self
                    .assets
                    .iter()
                    .map(|asset| asset.bytes.len())
                    .sum::<usize>()
                    .checked_add(bytes.len())
                    .is_none_or(|total| total > MAX_TOTAL_BYTES)
            {
                return Err(invalid("Service exported resources exceed their budget"));
            }
            self.assets.push(RuntimeAsset {
                path: provider.path.clone(),
                bytes: bytes.into(),
            });
        }
        Ok(())
    }

    pub(crate) fn with_delta(&self, delta: &serde_json::Value) -> MihomoResult<Self> {
        if !delta.is_object() {
            return Err(invalid("Service runtime delta must be an object"));
        }
        let mut value: Value = serde_yaml::from_str(&self.yaml)
            .map_err(|_| invalid("Invalid accepted service runtime YAML"))?;
        let patch =
            serde_yaml::to_value(delta).map_err(|_| invalid("Invalid service runtime delta"))?;
        crate::mihomo::config::merge::merge_yaml(&mut value, patch);
        if let Some(root) = value.as_mapping_mut() {
            crate::mihomo::tun::config::normalize(root);
        }
        let yaml = serde_yaml::to_string(&value)
            .map_err(|_| invalid("Invalid patched service runtime YAML"))?;
        if yaml.len() > MAX_CONFIG_BYTES {
            return Err(invalid("Service runtime YAML exceeds its byte budget"));
        }
        Ok(Self {
            yaml,
            assets: self.assets.clone(),
        })
    }
}

struct BundleBuilder<'a> {
    home: &'a Path,
    assets: Vec<RuntimeAsset>,
    named_sources: BTreeMap<PathBuf, String>,
    total: usize,
    next_name: usize,
}

fn prepare_bundle(payload: &str, source_home: &Path) -> MihomoResult<ServiceRuntimeBundle> {
    let mut value: Value =
        serde_yaml::from_str(payload).map_err(|_| invalid("Invalid service runtime YAML"))?;
    crate::mihomo::config::merge::expand_yaml_merges(&mut value)
        .map_err(|_| invalid("Invalid service runtime YAML merge"))?;
    if !value.is_mapping() {
        return Err(invalid("Service runtime must be a YAML mapping"));
    }
    if let Some(root) = value.as_mapping_mut() {
        crate::mihomo::tun::config::normalize(root);
    }
    let mut builder = BundleBuilder {
        home: source_home,
        assets: Vec::new(),
        named_sources: BTreeMap::new(),
        total: 0,
        next_name: 0,
    };
    for provider_kind in ["proxy-providers", "rule-providers"] {
        if let Some(providers) = value.get_mut(provider_kind) {
            let providers = providers
                .as_mapping_mut()
                .ok_or_else(|| invalid("Invalid provider mapping"))?;
            for provider in providers.values_mut() {
                let provider = provider
                    .as_mapping_mut()
                    .ok_or_else(|| invalid("Invalid provider definition"))?;
                let kind = provider
                    .get(Value::from("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("http")
                    .to_owned();
                if kind == "inline" {
                    continue;
                }
                if kind != "file" && kind != "http" {
                    return Err(invalid("Unsupported service provider type"));
                }
                let source = provider
                    .get(Value::from("path"))
                    .map(|path| {
                        path.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| invalid("Invalid provider path"))
                    })
                    .transpose()?;
                if kind == "file" && source.as_deref().is_none_or(str::is_empty) {
                    return Err(invalid("File provider requires a source path"));
                }
                let destination = builder.unique_name("providers", "yaml");
                if let Some(source) = source {
                    let source = builder.resolve(&source);
                    match read_asset(&source) {
                        Ok(mut bytes) => {
                            if provider_kind == "proxy-providers" {
                                let mut provider_yaml: Value = serde_yaml::from_slice(&bytes)
                                    .map_err(|_| invalid("Invalid proxy provider YAML"))?;
                                crate::mihomo::config::merge::expand_yaml_merges(
                                    &mut provider_yaml,
                                )
                                .map_err(|_| invalid("Invalid proxy provider YAML merge"))?;
                                builder.rewrite_tls(&mut provider_yaml, 0)?;
                                bytes = serde_yaml::to_string(&provider_yaml)
                                    .map_err(|_| invalid("Cannot encode proxy provider YAML"))?
                                    .into_bytes();
                            }
                            builder.insert(destination.clone(), bytes)?;
                        }
                        Err(error)
                            if kind == "http" && error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(_) => return Err(invalid("Cannot read provider resource")),
                    }
                }
                provider.insert(Value::from("path"), Value::from(destination));
            }
        }
    }
    builder.rewrite_tls(&mut value, 0)?;
    for (canonical, aliases) in [
        ("GeoIP.dat", &["GeoIP.dat", "geoip.dat"][..]),
        ("geosite.dat", &["geosite.dat", "GeoSite.dat"][..]),
        ("ASN.mmdb", &["ASN.mmdb", "asn.mmdb"][..]),
    ] {
        if let Some(source) = case_insensitive_geodata_source(source_home, aliases)? {
            let bytes =
                read_asset(&source).map_err(|_| invalid("Cannot read named GeoData resource"))?;
            builder.insert(format!("assets/geodata/{canonical}"), bytes)?;
        }
    }
    if let Some(source) = mmdb_source(source_home)? {
        let bytes =
            read_asset(&source).map_err(|_| invalid("Cannot read named GeoData resource"))?;
        builder.insert("assets/geodata/country.mmdb".into(), bytes)?;
    }
    let yaml =
        serde_yaml::to_string(&value).map_err(|_| invalid("Cannot encode service runtime YAML"))?;
    if yaml.len() > MAX_CONFIG_BYTES {
        return Err(invalid("Service runtime YAML exceeds its byte budget"));
    }
    Ok(ServiceRuntimeBundle {
        yaml,
        assets: builder.assets,
    })
}

fn case_insensitive_geodata_source(home: &Path, aliases: &[&str]) -> MihomoResult<Option<PathBuf>> {
    let entries = match std::fs::read_dir(home) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(invalid("Cannot inspect named GeoData resources")),
    };
    let mut first: Option<PathBuf> = None;
    for entry in entries {
        let entry = entry.map_err(|_| invalid("Cannot inspect named GeoData resources"))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !aliases.iter().any(|alias| name.eq_ignore_ascii_case(alias)) {
            continue;
        }
        if entry
            .file_type()
            .map_err(|_| invalid("Cannot inspect named GeoData resource"))?
            .is_dir()
        {
            continue;
        }
        if first
            .as_ref()
            .is_none_or(|previous| entry.file_name() < previous.file_name().unwrap_or_default())
        {
            first = Some(entry.path());
        }
    }
    Ok(first)
}

fn mmdb_source(home: &Path) -> MihomoResult<Option<PathBuf>> {
    case_insensitive_geodata_source(home, &["country.mmdb", "geoip.db", "geoip.metadb"])
}

impl BundleBuilder<'_> {
    fn resolve(&self, path: &str) -> PathBuf {
        let path = Path::new(path);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.home.join(path)
        }
    }
    fn unique_name(&mut self, category: &str, extension: &str) -> String {
        let name = format!("assets/{category}/{}.{}", self.next_name, extension);
        self.next_name += 1;
        name
    }
    fn insert(&mut self, path: String, bytes: Vec<u8>) -> MihomoResult<()> {
        if bytes.len() as u64 > MAX_ASSET_BYTES
            || self.assets.len() >= MAX_ASSETS
            || bytes.len() > MAX_TOTAL_BYTES.saturating_sub(self.total)
        {
            return Err(invalid("Service resource budget exceeded"));
        }
        self.total += bytes.len();
        self.assets.push(RuntimeAsset {
            path,
            bytes: bytes.into(),
        });
        Ok(())
    }
    fn rewrite_tls(&mut self, value: &mut Value, depth: usize) -> MihomoResult<()> {
        self.rewrite_tls_with_kind(value, depth, false)
    }

    fn rewrite_tls_with_kind(
        &mut self,
        value: &mut Value,
        depth: usize,
        inline_private_key: bool,
    ) -> MihomoResult<()> {
        if depth > 64 {
            return Err(invalid("Service runtime nesting limit exceeded"));
        }
        match value {
            Value::Mapping(mapping) => {
                let is_zerotier =
                    mapping.get(Value::from("type")).and_then(Value::as_str) == Some("zerotier");
                let is_ssh =
                    mapping.get(Value::from("type")).and_then(Value::as_str) == Some("ssh");
                let inline_private_key = mapping
                    .get(Value::from("type"))
                    .and_then(Value::as_str)
                    .map_or(inline_private_key, |kind| {
                        matches!(kind, "wireguard" | "masque")
                    });
                for (key, value) in mapping.iter_mut() {
                    let reads_file = matches!(
                        key.as_str(),
                        Some("certificate" | "private-key" | "client-auth-cert" | "ech-key")
                    ) || (key.as_str() == Some("planet") && is_zerotier);
                    if reads_file
                        && !(key.as_str() == Some("private-key") && inline_private_key)
                        && let Some(path) = value.as_str()
                        && !path.is_empty()
                        && !path.trim_start().starts_with("-----BEGIN ")
                        && !(key.as_str() == Some("private-key")
                            && is_ssh
                            && path.contains("PRIVATE KEY"))
                    {
                        let source = self.resolve(path);
                        let source = std::fs::canonicalize(source)
                            .map_err(|_| invalid("Cannot resolve TLS resource"))?;
                        let destination = if let Some(destination) = self.named_sources.get(&source)
                        {
                            destination.clone()
                        } else {
                            let bytes = read_asset(&source)
                                .map_err(|_| invalid("Cannot read TLS resource"))?;
                            let destination = if key.as_str() == Some("planet") {
                                self.unique_name("planet", "bin")
                            } else {
                                self.unique_name("tls", "pem")
                            };
                            self.insert(destination.clone(), bytes)?;
                            self.named_sources.insert(source, destination.clone());
                            destination
                        };
                        *value = Value::from(destination);
                    }
                    self.rewrite_tls_with_kind(value, depth + 1, inline_private_key)?;
                }
            }
            Value::Sequence(values) => {
                for value in values {
                    self.rewrite_tls_with_kind(value, depth + 1, inline_private_key)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn read_asset(path: &Path) -> std::io::Result<Vec<u8>> {
    read_asset_with_limit(path, MAX_ASSET_BYTES)
}

fn read_asset_with_limit(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let limit = limit.min(MAX_ASSET_BYTES);
    let mut options = File::options();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(std::io::Error::other(
            "Service resource is not a bounded regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::other(
            "Service resource grew beyond its byte budget",
        ));
    }
    Ok(bytes)
}

fn invalid(message: &str) -> MihomoError {
    MihomoError::InvalidInput(message.into())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod test_support;
