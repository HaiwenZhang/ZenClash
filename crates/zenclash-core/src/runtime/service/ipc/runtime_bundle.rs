// Forked from Clash Verge Rev; adapted for ZenClash on 2026-10-04.
// GPL-3.0-only; original authors and changes are recorded in NOTICE.md.
use anyhow::{Context as _, Result, bail};
use serde_yaml::{Mapping, Value};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};
use zenclash_service::{RemoteProvider, RuntimeAsset, RuntimeBundle};

pub(crate) const GEO_ASSETS: &[&str] = &[
    "Country.mmdb",
    "ASN.mmdb",
    "GeoIP.dat",
    "geoip.dat",
    "geosite.dat",
    "GeoSite.dat",
    "geoip.metadb",
];

/// Provider identity used by the runtime config and core API.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RemoteProviderRef {
    /// Configuration section containing this provider.
    pub section: &'static str,
    /// Provider key within its configuration section.
    pub name: String,
    /// Remote cache declaration sent to the service.
    pub provider: RemoteProvider,
}

impl RemoteProviderRef {
    /// Returns the provider configuration section.
    pub fn section(&self) -> &'static str {
        self.section
    }
    /// Returns the provider key.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns the remote cache declaration.
    pub fn provider(&self) -> &RemoteProvider {
        &self.provider
    }
}

struct RuntimeParts {
    config: Value,
    assets: Vec<RuntimeAsset>,
    remote_providers: Vec<RemoteProviderRef>,
}

pub(crate) async fn collect_runtime_bundle(
    config_file: &Path,
    core_path: &Path,
) -> Result<RuntimeBundle> {
    let yaml = tokio::fs::read_to_string(config_file)
        .await
        .with_context(|| format!("failed to read runtime config {config_file:?}"))?;
    let config: Value = serde_yaml::from_str(&yaml)
        .with_context(|| format!("failed to parse runtime config {config_file:?}"))?;
    let config_root = config_file
        .parent()
        .ok_or_else(|| anyhow::anyhow!("runtime config has no parent directory"))?;
    let parts = collect_runtime_parts(config, config_root)?;
    Ok(RuntimeBundle {
        yaml: serde_yaml::to_string(&parts.config)
            .context("failed to serialize service runtime config")?,
        assets: parts.assets,
        remote_providers: parts
            .remote_providers
            .into_iter()
            .map(|declared| declared.provider)
            .collect(),
        core_path: core_path.to_string_lossy().into_owned(),
    })
}

pub(crate) fn remote_providers_of(
    config: &Mapping,
    config_root: &Path,
) -> Result<Vec<RemoteProviderRef>> {
    Ok(collect_runtime_parts(Value::Mapping(config.clone()), config_root)?.remote_providers)
}

#[derive(Default)]
struct ProviderPathOwners {
    remote: BTreeMap<String, Vec<(&'static str, Value)>>,
    local: bool,
}

pub(crate) fn resolve_provider_path_conflicts(
    config: &mut Mapping,
    config_root: &Path,
) -> Result<()> {
    let config_root = std::fs::canonicalize(config_root)?;
    let mut paths = BTreeMap::<String, ProviderPathOwners>::new();
    let mut reserved: HashSet<String> = GEO_ASSETS.iter().map(|name| (*name).to_owned()).collect();
    for section in ["proxy-providers", "rule-providers"] {
        let Some(providers) = config.get(section).and_then(Value::as_mapping) else {
            continue;
        };
        for (name, provider) in providers {
            let Some(raw_path) = provider.get("path").and_then(Value::as_str) else {
                continue;
            };
            let is_remote = provider.get("type").and_then(Value::as_str) == Some("http");
            let destination = if is_remote {
                provider_destination(&config_root, raw_path)
            } else {
                local_provider_source(&config_root, raw_path)
                    .and_then(|source| destination_below_root(&config_root, &source))
                    .or_else(|_| provider_destination(&config_root, raw_path))
            };
            let Ok(destination) = destination else {
                continue;
            };
            reserved.insert(destination.clone());
            let owners = paths.entry(destination).or_default();
            match (is_remote, provider.get("url").and_then(Value::as_str)) {
                (true, Some(url)) => owners
                    .remote
                    .entry(url.to_owned())
                    .or_default()
                    .push((section, name.clone())),
                _ => owners.local = true,
            }
        }
    }

    for (destination, owners) in paths {
        if owners.local || owners.remote.len() < 2 {
            continue;
        }
        // Allocate every source in a conflict group, so declaration order cannot select a cache's owner.
        for (url, providers) in owners.remote {
            let replacement = allocate_provider_destination(&destination, &url, &mut reserved);
            for (section, name) in providers {
                config[section][&name]["path"] = Value::String(replacement.clone());
                tracing::warn!(
                    "provider {section}/{name:?} shared cache {destination:?} with another source; using {replacement:?}"
                );
            }
        }
    }
    Ok(())
}

fn allocate_provider_destination(
    destination: &str,
    url: &str,
    reserved: &mut HashSet<String>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(destination.as_bytes());
    digest.update([0]);
    digest.update(url.as_bytes());
    let digest: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let path = Path::new(destination);
    let extension = path
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy()))
        .unwrap_or_default();
    let mut suffix = String::new();
    let mut sequence = 0;
    loop {
        let candidate = path
            .with_file_name(format!("zenclash-{digest}{suffix}{extension}"))
            .to_string_lossy()
            .replace('\\', "/");
        if reserved.insert(candidate.clone()) {
            return candidate;
        }
        sequence += 1;
        suffix = format!("-{sequence}");
    }
}

fn collect_runtime_parts(mut config: Value, config_root: &Path) -> Result<RuntimeParts> {
    let config_root = std::fs::canonicalize(config_root)?;
    let mut assets = Vec::new();
    let mut destinations = HashSet::new();
    let mut remote_providers = Vec::new();

    collect_provider_assets(
        &mut config,
        "proxy-providers",
        &config_root,
        &mut destinations,
        &mut assets,
        &mut remote_providers,
    )?;
    collect_provider_assets(
        &mut config,
        "rule-providers",
        &config_root,
        &mut destinations,
        &mut assets,
        &mut remote_providers,
    )?;
    collect_geodata_assets(&config_root, &mut destinations, &mut assets)?;

    Ok(RuntimeParts {
        config,
        assets,
        remote_providers,
    })
}

fn collect_geodata_assets(
    config_root: &Path,
    destinations: &mut HashSet<String>,
    assets: &mut Vec<RuntimeAsset>,
) -> Result<()> {
    let entries = match std::fs::read_dir(config_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut matched = Vec::new();
    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !GEO_ASSETS
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
        {
            continue;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        matched.push((name.to_owned(), path));
    }
    matched.sort_by(|left, right| left.0.cmp(&right.0));
    let mut seen_folded = HashSet::new();
    for (name, path) in matched {
        let folded = name.to_ascii_lowercase();
        if !seen_folded.insert(folded) {
            continue;
        }
        if destinations.insert(name.clone()) {
            assets.push(RuntimeAsset {
                source: std::fs::canonicalize(&path)?.to_string_lossy().into_owned(),
                destination: name,
            });
        }
    }
    Ok(())
}

/// Rewrites provider paths and records copied assets separately from core-downloaded providers.
/// Remote URLs let the service decide whether a core download cache remains reusable.
fn collect_provider_assets(
    config: &mut Value,
    section: &'static str,
    config_root: &Path,
    destinations: &mut HashSet<String>,
    assets: &mut Vec<RuntimeAsset>,
    remote_providers: &mut Vec<RemoteProviderRef>,
) -> Result<()> {
    let Some(providers) = config
        .as_mapping_mut()
        .and_then(|mapping| mapping.get_mut(section))
        .and_then(Value::as_mapping_mut)
    else {
        return Ok(());
    };

    for (name, provider) in providers.iter_mut() {
        let Some(provider) = provider.as_mapping_mut() else {
            continue;
        };
        let Some(raw_path) = provider.get("path").and_then(Value::as_str) else {
            continue;
        };
        // Match mihomo's `type` classification and leave malformed-but-accepted providers untouched.
        let is_remote = provider.get("type").and_then(Value::as_str) == Some("http");
        let url = provider
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let destination = match (is_remote, url) {
            (true, Some(url)) => {
                let destination = provider_destination(config_root, raw_path)?;
                // Reserve remote destinations too; copied and downloaded files must not collide.
                match remote_providers
                    .iter()
                    .find(|declared| declared.provider.destination == destination)
                {
                    Some(declared) if declared.provider.url == url => {}
                    Some(_) => {
                        bail!(
                            "runtime provider destination {destination:?} is declared for two different sources"
                        )
                    }
                    None => {
                        if !destinations.insert(destination.clone()) {
                            bail!(
                                "runtime provider destination {destination:?} is claimed more than once"
                            );
                        }
                        remote_providers.push(RemoteProviderRef {
                            section,
                            name: name.as_str().unwrap_or_default().to_owned(),
                            provider: RemoteProvider {
                                destination: destination.clone(),
                                url,
                            },
                        });
                    }
                }
                destination
            }
            (true, None) => {
                tracing::warn!(
                    "remote provider {name:?} declares no url; leaving its path to the core"
                );
                continue;
            }
            (false, _) => {
                let Ok(source) = local_provider_source(config_root, raw_path) else {
                    tracing::warn!(
                        "local provider {name:?} is unavailable at {raw_path:?}; leaving its path to the core"
                    );
                    continue;
                };
                let destination = destination_below_root(config_root, &source)?;
                if destinations.insert(destination.clone()) {
                    assets.push(RuntimeAsset {
                        source: source.to_string_lossy().into_owned(),
                        destination: destination.clone(),
                    });
                }
                destination
            }
        };
        provider.insert(Value::String("path".to_owned()), Value::String(destination));
    }
    Ok(())
}

fn local_provider_source(config_root: &Path, raw_path: &str) -> Result<PathBuf> {
    let path = Path::new(raw_path);
    let source = if path.is_absolute() {
        path.to_path_buf()
    } else {
        config_root.join(path)
    };
    let canonical_source = std::fs::canonicalize(&source)
        .with_context(|| format!("local runtime provider is unavailable: {source:?}"))?;
    canonical_source.strip_prefix(config_root).map_err(|_| {
        anyhow::anyhow!("local runtime provider is outside the config root: {canonical_source:?}")
    })?;
    Ok(canonical_source)
}

fn provider_destination(config_root: &Path, raw_path: &str) -> Result<String> {
    let path = Path::new(raw_path);
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("runtime provider destination traverses outside the runtime");
    }
    if path.is_absolute() {
        let normalized = canonicalize_with_missing_tail(path)?;
        destination_below_root(config_root, &normalized)
    } else {
        normalized_destination(path)
    }
}

fn canonicalize_with_missing_tail(path: &Path) -> Result<PathBuf> {
    let mut ancestor = path;
    let mut tail = Vec::new();
    while !ancestor.exists() {
        let name = ancestor.file_name().ok_or_else(|| {
            anyhow::anyhow!("runtime provider path has no existing ancestor: {path:?}")
        })?;
        tail.push(name.to_owned());
        ancestor = ancestor.parent().ok_or_else(|| {
            anyhow::anyhow!("runtime provider path has no existing ancestor: {path:?}")
        })?;
    }
    let mut normalized = std::fs::canonicalize(ancestor)?;
    for component in tail.iter().rev() {
        normalized.push(component);
    }
    Ok(normalized)
}

fn destination_below_root(config_root: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(config_root).map_err(|_| {
        anyhow::anyhow!("runtime provider path is outside the config root: {path:?}")
    })?;
    normalized_destination(relative)
}

fn normalized_destination(relative: &Path) -> Result<String> {
    let mut destination = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::Normal(component) => destination.push(component),
            Component::CurDir => {}
            _ => bail!("local runtime provider destination traverses outside the runtime"),
        }
    }
    if destination.as_os_str().is_empty() {
        bail!("local runtime provider destination is empty");
    }
    Ok(destination.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests;
