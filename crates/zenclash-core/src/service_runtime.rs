//! User-authority preparation of bounded service runtime resources.

use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde_yaml::Value;
use zenclash_service::ServiceClient;

use crate::{MihomoError, MihomoResult};

const MAX_ASSET_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;
const MAX_ASSETS: usize = 256;
const MAX_CONFIG_BYTES: usize = 4 * 1024 * 1024;
const CHUNK_BYTES: usize = 256 * 1024;
const GEODATA: [&str; 4] = ["GeoIP.dat", "geosite.dat", "country.mmdb", "ASN.mmdb"];

#[derive(Clone)]
struct RuntimeAsset {
    path: String,
    bytes: Arc<[u8]>,
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

    /// Uploads this exact snapshot; it never rereads a mutable source file.
    ///
    /// # Errors
    /// Returns a native transport or service policy rejection without retrying uploads.
    pub async fn stage(&self, service: &ServiceClient) -> MihomoResult<u64> {
        let revision = service.stage(self.yaml()).await?;
        for asset in &self.assets {
            if asset.bytes.is_empty() {
                service.upload_asset(&asset.path, 0, &[], true).await?;
            } else {
                for (index, chunk) in asset.bytes.chunks(CHUNK_BYTES).enumerate() {
                    let offset = index * CHUNK_BYTES;
                    service
                        .upload_asset(
                            &asset.path,
                            offset as u64,
                            chunk,
                            offset + chunk.len() == asset.bytes.len(),
                        )
                        .await?;
                }
            }
        }
        Ok(revision)
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
    if !value.is_mapping() {
        return Err(invalid("Service runtime must be a YAML mapping"));
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
    for name in GEODATA {
        match read_asset(&source_home.join(name)) {
            Ok(bytes) => builder.insert(format!("assets/geodata/{name}"), bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(invalid("Cannot read named GeoData resource")),
        }
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
    let mut options = File::options();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_ASSET_BYTES {
        return Err(std::io::Error::other(
            "Service resource is not a bounded regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_ASSET_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ASSET_BYTES {
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
mod tests {
    use super::*;

    struct TestHome(PathBuf);
    impl TestHome {
        fn new() -> Self {
            let mut random = [0_u8; 16];
            getrandom::fill(&mut random).unwrap();
            let path = std::env::temp_dir().join(format!(
                "zenclash-service-bundle-{:x}",
                u128::from_ne_bytes(random)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn prepares_provider_and_nested_tls_without_rewriting_user_files() {
        let home = TestHome::new();
        let provider = "proxies:\n- name: node\n  type: http\n  certificate: cert.pem\n";
        std::fs::write(home.0.join("nodes.yaml"), provider).unwrap();
        std::fs::write(home.0.join("cert.pem"), b"public certificate").unwrap();
        std::fs::write(home.0.join("GeoIP.dat"), b"geodata").unwrap();
        let yaml = "proxy-providers:\n  local:\n    type: file\n    path: nodes.yaml\n";
        let bundle = ServiceRuntimeBundle::prepare(yaml, home.0.clone())
            .await
            .unwrap();
        let value: Value = serde_yaml::from_str(bundle.yaml()).unwrap();
        let path = value["proxy-providers"]["local"]["path"].as_str().unwrap();
        assert!(path.starts_with("assets/providers/"));
        let provider_asset = bundle
            .assets
            .iter()
            .find(|asset| asset.path == path)
            .unwrap();
        let provider_yaml: Value = serde_yaml::from_slice(&provider_asset.bytes).unwrap();
        assert!(
            provider_yaml["proxies"][0]["certificate"]
                .as_str()
                .unwrap()
                .starts_with("assets/tls/")
        );
        assert!(
            bundle
                .assets
                .iter()
                .any(|asset| asset.path == "assets/geodata/GeoIP.dat")
        );
        assert_eq!(
            std::fs::read_to_string(home.0.join("nodes.yaml")).unwrap(),
            provider
        );
    }

    #[tokio::test]
    async fn missing_file_provider_rejects_before_any_service_operation() {
        let home = TestHome::new();
        let result = ServiceRuntimeBundle::prepare(
            "proxy-providers:\n  local:\n    type: file\n    path: missing.yaml\n",
            home.0.clone(),
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn absent_http_cache_gets_a_safe_destination_without_a_fake_asset() {
        let home = TestHome::new();
        let bundle = ServiceRuntimeBundle::prepare(
            "proxy-providers:\n  remote:\n    type: http\n    url: https://example.com/nodes\n",
            home.0.clone(),
        )
        .await
        .unwrap();
        let value: Value = serde_yaml::from_str(bundle.yaml()).unwrap();
        assert!(
            value["proxy-providers"]["remote"]["path"]
                .as_str()
                .unwrap()
                .starts_with("assets/providers/")
        );
        assert!(bundle.assets.is_empty());
    }

    #[tokio::test]
    async fn inline_pem_remains_data_and_oversized_assets_are_rejected() {
        let home = TestHome::new();
        let bundle = ServiceRuntimeBundle::prepare(
            "proxies:\n- certificate: |\n    -----BEGIN CERTIFICATE-----\n    encoded\n",
            home.0.clone(),
        )
        .await
        .unwrap();
        assert!(bundle.assets.is_empty());
        let file = std::fs::File::create(home.0.join("large.pem")).unwrap();
        file.set_len(MAX_ASSET_BYTES + 1).unwrap();
        assert!(
            ServiceRuntimeBundle::prepare("proxies:\n- certificate: large.pem\n", home.0.clone())
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn wireguard_masque_and_inline_ca_keys_are_preserved_as_data() {
        let home = TestHome::new();
        let yaml = "tls:\n  custom-certifactes: [inline-DER]\nproxies:\n- type: wireguard\n  private-key: base64-wg\n- type: masque\n  private-key: base64-ec\n- type: openvpn\n  ca: inline-ca\n";
        let bundle = ServiceRuntimeBundle::prepare(yaml, home.0.clone())
            .await
            .unwrap();
        let value: Value = serde_yaml::from_str(bundle.yaml()).unwrap();
        assert_eq!(
            value["proxies"][0]["private-key"].as_str(),
            Some("base64-wg")
        );
        assert_eq!(
            value["proxies"][1]["private-key"].as_str(),
            Some("base64-ec")
        );
        assert_eq!(value["proxies"][2]["ca"].as_str(), Some("inline-ca"));
        assert!(bundle.assets.is_empty());
    }
}
