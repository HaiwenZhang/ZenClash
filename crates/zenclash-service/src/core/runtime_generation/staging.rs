//! Updates the runtime generation while its core is still running.
//! Only manifest-owned files are deleted, provider caches are invalidated when their URL changes,
//! and asset metadata avoids hashing large unchanged files. Configuration is committed last.

use super::assets::{
    destination_key, invalid_asset, resolve_in_generation, runtime_cleanup_retry_delay, validate_core_path,
    validate_destination,
};
use crate::core::auth::{AuthenticatedOwner, ServiceError};
use crate::core::manager::CORE_MANAGER;
use crate::{RemoteProvider, RuntimeAsset, RuntimeBundle, StageRejection, StageRuntimeOutcome};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) const MANIFEST_FILE_NAME: &str = ".runtime-manifest.json";

/// Source metadata recorded after a copy, avoiding content hashing on later staging.
/// An unknown modification time never matches because length alone is not a safe identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct SourceIdentity {
    pub source: String,
    pub len: u64,
    #[serde(default)]
    pub mtime_ns: Option<u128>,
}

impl SourceIdentity {
    fn still_matches(&self, current: &Self) -> bool {
        self.mtime_ns.is_some() && self == current
    }
}

/// Service-managed files recorded after a successful start or staging.
/// A missing manifest means no file ownership or cache provenance can be trusted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RuntimeManifest {
    #[serde(default)]
    pub assets: BTreeMap<String, SourceIdentity>,
    #[serde(default)]
    pub remote_providers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlannedCopy {
    pub source: String,
    pub destination: String,
    pub identity: SourceIdentity,
}

/// Planned changes, grouped by their required execution order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct StagePlan {
    /// Stale provider caches removed before reload.
    pub required_deletes: Vec<String>,
    pub copies: Vec<PlannedCopy>,
    /// Unchanged assets, retained for logging.
    pub skipped: Vec<String>,
    /// Obsolete service-managed files removed after commit.
    pub hygiene_deletes: Vec<String>,
    pub manifest: RuntimeManifest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AssetSource {
    pub asset: RuntimeAsset,
    pub len: u64,
    pub mtime_ns: Option<u128>,
}

/// Builds a pure staging plan from pre-read metadata.
pub(super) fn plan_stage(previous: &RuntimeManifest, sources: &[AssetSource], remote: &[RemoteProvider]) -> StagePlan {
    let mut plan = StagePlan::default();

    for source in sources {
        let identity = SourceIdentity {
            source: source.asset.source.clone(),
            len: source.len,
            mtime_ns: source.mtime_ns,
        };
        let destination = source.asset.destination.clone();
        if previous
            .assets
            .get(&destination)
            .is_some_and(|recorded| recorded.still_matches(&identity))
        {
            plan.skipped.push(destination.clone());
        } else {
            plan.copies.push(PlannedCopy {
                source: source.asset.source.clone(),
                destination: destination.clone(),
                identity: identity.clone(),
            });
        }
        plan.manifest.assets.insert(destination, identity);
    }

    for provider in remote {
        // Missing provenance is treated like a changed URL and cannot reuse the cache.
        if previous.remote_providers.get(&provider.destination) != Some(&provider.url) {
            plan.required_deletes.push(provider.destination.clone());
        }
        plan.manifest
            .remote_providers
            .insert(provider.destination.clone(), provider.url.clone());
    }

    for recorded in previous.assets.keys().chain(previous.remote_providers.keys()) {
        if !plan.manifest.assets.contains_key(recorded) && !plan.manifest.remote_providers.contains_key(recorded) {
            plan.hygiene_deletes.push(recorded.clone());
        }
    }
    // The two input maps are sorted individually, not after concatenation.
    plan.hygiene_deletes.sort();
    plan.hygiene_deletes.dedup();

    plan
}

/// Validates provider destinations and rejects conflicting URLs or asset ownership.
pub(super) fn declared_remote_providers(
    declared: &[RemoteProvider],
    asset_destinations: &BTreeSet<String>,
) -> Result<Vec<RemoteProvider>, ServiceError> {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for provider in declared {
        let destination = destination_key(&validate_destination(&provider.destination)?)?;
        if asset_destinations.contains(&destination) {
            return Err(invalid_asset(format!(
                "runtime destination {destination:?} is declared both as a copied asset and as a remote provider"
            )));
        }
        match seen.entry(destination) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(provider.url.clone());
            }
            std::collections::btree_map::Entry::Occupied(slot) if slot.get() == &provider.url => {}
            std::collections::btree_map::Entry::Occupied(slot) => {
                return Err(invalid_asset(format!(
                    "runtime destination {:?} is declared for two different provider sources",
                    slot.key()
                )));
            }
        }
    }
    Ok(seen
        .into_iter()
        .map(|(destination, url)| RemoteProvider { destination, url })
        .collect())
}

/// Stages a live generation or returns a restart fallback.
/// Required deletes and copies precede the atomic configuration commit; obsolete-file cleanup is
/// best-effort afterward.
pub(crate) async fn stage_runtime(
    owner: &AuthenticatedOwner,
    bundle: &RuntimeBundle,
) -> Result<StageRuntimeOutcome, ServiceError> {
    let Some((core_pid, running)) = CORE_MANAGER.lock().await.running_core_config().await else {
        return Ok(StageRuntimeOutcome::RestartRequired {
            reason: StageRejection::CoreNotRunning,
        });
    };

    let core = validate_core_path(&bundle.core_path)?;
    if Path::new(&running.core_config.core_path) != core.executable() {
        return Ok(StageRuntimeOutcome::RestartRequired {
            reason: StageRejection::CorePathChanged,
        });
    }

    let generation = PathBuf::from(&running.core_config.config_dir);
    let super::assets::GatheredBundle { sources, remote } = super::assets::gather_bundle(owner, bundle, &core).await?;

    let previous = match read_manifest(&generation).await {
        Ok(previous) => previous,
        Err(detail) => {
            return Ok(StageRuntimeOutcome::RestartRequired {
                reason: StageRejection::RuntimeUnwritable { detail },
            });
        }
    };
    let plan = plan_stage(&previous, &sources, &remote);

    for destination in &plan.required_deletes {
        let target = resolve_in_generation(&generation, destination)?;
        if let Err(error) = remove_staged_file(&target).await {
            return Ok(StageRuntimeOutcome::RestartRequired {
                reason: StageRejection::RuntimeUnwritable {
                    detail: format!("failed to discard the stale cache {destination}: {error}"),
                },
            });
        }
    }

    for copy in &plan.copies {
        let target = resolve_in_generation(&generation, &copy.destination)?;
        if let Err(error) = copy_staged_file(&copy.source, &target).await {
            return Ok(StageRuntimeOutcome::RestartRequired {
                reason: StageRejection::RuntimeUnwritable {
                    detail: format!("failed to refresh the runtime asset {}: {error}", copy.destination),
                },
            });
        }
        // Re-stat after copying to avoid recording stale source metadata.
        if source_identity_changed(&copy.source, &copy.identity).await {
            return Ok(StageRuntimeOutcome::RestartRequired {
                reason: StageRejection::RuntimeUnwritable {
                    detail: format!("runtime asset {} changed while it was being copied", copy.destination),
                },
            });
        }
    }

    // The watchdog can replace the core without the lifecycle lock. Never commit provenance built
    // for an earlier process; force a clean restart instead.
    if CORE_MANAGER
        .lock()
        .await
        .running_core_config()
        .await
        .map(|(pid, _)| pid)
        != Some(core_pid)
    {
        return Ok(StageRuntimeOutcome::RestartRequired {
            reason: StageRejection::CoreRestarted,
        });
    }

    let config_path = PathBuf::from(&running.core_config.config_path);
    if let Err(error) = commit_staged_config(&generation, &config_path, &bundle.yaml, &plan.manifest).await {
        // A failed config commit leaves the newly written manifest untrusted; discard it.
        if let Err(discard) = remove_staged_file(&generation.join(MANIFEST_FILE_NAME)).await {
            tracing::warn!(
                error = %discard,
                "Left a manifest behind that describes an uncommitted configuration"
            );
        }
        return Ok(StageRuntimeOutcome::RestartRequired {
            reason: StageRejection::RuntimeUnwritable {
                detail: format!("failed to commit the staged configuration: {error}"),
            },
        });
    }

    for destination in &plan.hygiene_deletes {
        match resolve_in_generation(&generation, destination) {
            Ok(target) => {
                if let Err(error) = remove_staged_file(&target).await {
                    tracing::warn!(
                        destination = %destination,
                        error = %error,
                        "Left an undeclared file behind in the staged runtime generation"
                    );
                }
            }
            Err(error) => tracing::warn!(
                destination = %destination,
                error = %error,
                "Refused to sweep a recorded destination that no longer validates"
            ),
        }
    }

    tracing::info!(
        copied = plan.copies.len(),
        skipped = plan.skipped.len(),
        discarded = plan.required_deletes.len(),
        "Staged a runtime generation in place"
    );
    Ok(StageRuntimeOutcome::Staged {
        config_path: config_path.to_string_lossy().into_owned(),
    })
}

pub(super) fn modified_nanos(metadata: &std::fs::Metadata) -> Option<u128> {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_nanos())
}

/// Returns true when source metadata changed or can no longer be read.
pub(super) async fn source_identity_changed(source: &str, recorded: &SourceIdentity) -> bool {
    match tokio::fs::metadata(source).await {
        Ok(metadata) => metadata.len() != recorded.len || modified_nanos(&metadata) != recorded.mtime_ns,
        Err(_) => true,
    }
}

/// Reads the manifest; absence is empty state, while malformed contents require a clean restart.
pub(super) async fn read_manifest(generation: &Path) -> Result<RuntimeManifest, String> {
    let path = generation.join(MANIFEST_FILE_NAME);
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|error| format!("runtime manifest {path:?} is unreadable: {error}"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(RuntimeManifest::default()),
        Err(error) => Err(format!("runtime manifest {path:?} cannot be read: {error}")),
    }
}

/// Retries transient Windows failures while the running core releases file handles.
async fn while_the_core_lets_go<Operation, Attempt>(mut operation: Operation) -> std::io::Result<()>
where
    Operation: FnMut() -> Attempt,
    Attempt: std::future::Future<Output = std::io::Result<()>>,
{
    let mut retry_index = 0;
    loop {
        match operation().await {
            Ok(()) => return Ok(()),
            Err(error) => {
                let Some(delay) = runtime_cleanup_retry_delay(&error, retry_index) else {
                    return Err(error);
                };
                retry_index += 1;
                tokio::time::sleep(delay).await;
            }
        }
    }
}

pub(super) async fn remove_staged_file(path: &Path) -> std::io::Result<()> {
    while_the_core_lets_go(|| async {
        match tokio::fs::remove_file(path).await {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    })
    .await
}

/// Atomically replaces a destination and removes the temporary on failure.
async fn replace_staged_file(staged: &Path, destination: &Path) -> std::io::Result<()> {
    let result = while_the_core_lets_go(|| crate::core::atomic_file::replace(staged, destination)).await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(staged).await;
    }
    result
}

pub(super) async fn copy_staged_file(source: &str, destination: &Path) -> std::io::Result<()> {
    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let staged = staging_temp_path(destination);
    if let Err(error) = tokio::fs::copy(source, &staged).await {
        let _ = tokio::fs::remove_file(&staged).await;
        return Err(error);
    }
    replace_staged_file(&staged, destination).await
}

pub(super) async fn commit_staged_config(
    generation: &Path,
    config_path: &Path,
    yaml: &str,
    manifest: &RuntimeManifest,
) -> std::io::Result<()> {
    // Record completed file changes first; the caller removes it if config commit fails.
    let manifest_path = generation.join(MANIFEST_FILE_NAME);
    let encoded =
        serde_json::to_vec(manifest).map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    write_atomically(&manifest_path, &encoded).await?;
    write_atomically(config_path, yaml.as_bytes()).await
}

pub(super) async fn write_atomically(destination: &Path, contents: &[u8]) -> std::io::Result<()> {
    async fn write_temp(staged: &Path, contents: &[u8]) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt as _;

        let mut file = tokio::fs::File::create(staged).await?;
        file.write_all(contents).await?;
        file.sync_all().await
    }

    let staged = staging_temp_path(destination);
    // Temporaries are never manifest-owned, so clean them up immediately on write failure.
    if let Err(error) = write_temp(&staged, contents).await {
        let _ = tokio::fs::remove_file(&staged).await;
        return Err(error);
    }
    replace_staged_file(&staged, destination).await
}

/// Creates a collision-resistant name that bundle destinations are forbidden to claim.
fn staging_temp_path(destination: &Path) -> PathBuf {
    let sequence = STAGING_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = destination
        .file_name()
        .map_or_else(|| "staged".to_owned(), |name| name.to_string_lossy().into_owned());
    destination.with_file_name(format!(".{name}.staging-{}-{sequence}", std::process::id()))
}

static STAGING_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(source: &str, len: u64, mtime_ns: u128) -> SourceIdentity {
        SourceIdentity {
            source: source.to_owned(),
            len,
            mtime_ns: Some(mtime_ns),
        }
    }

    fn asset_source(source: &str, destination: &str, len: u64, mtime_ns: u128) -> AssetSource {
        AssetSource {
            asset: RuntimeAsset {
                source: source.to_owned(),
                destination: destination.to_owned(),
            },
            len,
            mtime_ns: Some(mtime_ns),
        }
    }

    fn remote(destination: &str, url: &str) -> RemoteProvider {
        RemoteProvider {
            destination: destination.to_owned(),
            url: url.to_owned(),
        }
    }

    fn manifest(assets: &[(&str, SourceIdentity)], providers: &[(&str, &str)]) -> RuntimeManifest {
        RuntimeManifest {
            assets: assets.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect(),
            remote_providers: providers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    #[test]
    fn an_asset_made_from_the_same_source_is_not_copied_again() {
        let previous = manifest(&[("geoip.metadb", identity("/app/geoip.metadb", 60_000_000, 42))], &[]);
        let sources = [asset_source("/app/geoip.metadb", "geoip.metadb", 60_000_000, 42)];

        let plan = plan_stage(&previous, &sources, &[]);

        assert!(plan.copies.is_empty(), "unchanged geo data must not be re-copied");
        assert_eq!(plan.skipped, ["geoip.metadb"]);
        assert!(plan.required_deletes.is_empty());
    }

    #[test]
    fn an_asset_whose_source_changed_is_copied() {
        let previous = manifest(&[("geoip.metadb", identity("/app/geoip.metadb", 60_000_000, 42))], &[]);
        let sources = [asset_source("/app/geoip.metadb", "geoip.metadb", 61_000_000, 99)];

        let plan = plan_stage(&previous, &sources, &[]);

        assert_eq!(plan.copies.len(), 1);
        assert_eq!(plan.copies[0].destination, "geoip.metadb");
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn an_asset_copied_from_a_different_source_path_is_copied_again() {
        // Equal metadata cannot make a copy from another source valid.
        let previous = manifest(&[("providers/p.yaml", identity("/app/one.yaml", 128, 7))], &[]);
        let sources = [asset_source("/app/two.yaml", "providers/p.yaml", 128, 7)];

        let plan = plan_stage(&previous, &sources, &[]);

        assert_eq!(plan.copies.len(), 1);
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn a_remote_cache_from_the_same_url_is_kept() {
        let previous = manifest(&[], &[("rules/ads.yaml", "https://one.example/ads.yaml")]);

        let plan = plan_stage(
            &previous,
            &[],
            &[remote("rules/ads.yaml", "https://one.example/ads.yaml")],
        );

        assert!(
            plan.required_deletes.is_empty(),
            "an unchanged source must keep its download cache"
        );
        assert!(plan.copies.is_empty());
    }

    #[test]
    fn a_remote_cache_whose_url_changed_must_be_deleted_before_reload() {
        let previous = manifest(&[], &[("rules/ads.yaml", "https://one.example/ads.yaml")]);

        let plan = plan_stage(
            &previous,
            &[],
            &[remote("rules/ads.yaml", "https://two.example/ads.yaml")],
        );

        assert_eq!(plan.required_deletes, ["rules/ads.yaml"]);
        assert!(
            plan.hygiene_deletes.is_empty(),
            "declared destinations must not be swept"
        );
    }

    #[test]
    fn without_a_manifest_nothing_is_skipped_and_every_cache_is_discarded() {
        let sources = [asset_source("/app/geoip.metadb", "geoip.metadb", 60_000_000, 42)];

        let plan = plan_stage(
            &RuntimeManifest::default(),
            &sources,
            &[remote("rules/ads.yaml", "https://one.example/ads.yaml")],
        );

        assert_eq!(plan.copies.len(), 1);
        assert_eq!(plan.required_deletes, ["rules/ads.yaml"]);
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn only_paths_a_previous_staging_recorded_are_swept() {
        let previous = manifest(
            &[("providers/gone.yaml", identity("/app/gone.yaml", 10, 1))],
            &[("rules/gone.yaml", "https://one.example/gone.yaml")],
        );

        let plan = plan_stage(&previous, &[], &[]);

        assert_eq!(plan.hygiene_deletes, ["providers/gone.yaml", "rules/gone.yaml"]);
        assert!(
            plan.required_deletes.is_empty() && plan.copies.is_empty(),
            "housekeeping alone must not make staging look like it has work to do"
        );
    }

    #[test]
    fn one_destination_declared_for_two_sources_is_refused() {
        let declared = [
            remote("rules/ads.yaml", "https://one.example/ads.yaml"),
            remote("rules/ads.yaml", "https://two.example/ads.yaml"),
        ];

        let error = declared_remote_providers(&declared, &BTreeSet::new())
            .expect_err("a destination cannot be owned by two sources at once");

        assert!(
            error.message.contains("two different provider sources"),
            "{}",
            error.message
        );
    }

    #[test]
    fn repeating_one_provider_verbatim_still_keeps_its_cache() {
        let declared = [
            remote("rules/ads.yaml", "https://one.example/ads.yaml"),
            remote("rules/ads.yaml", "https://one.example/ads.yaml"),
        ];
        let previous = manifest(&[], &[("rules/ads.yaml", "https://one.example/ads.yaml")]);

        let resolved =
            declared_remote_providers(&declared, &BTreeSet::new()).expect("an identical repeat is not a conflict");
        let plan = plan_stage(&previous, &[], &resolved);

        assert_eq!(resolved.len(), 1, "one destination yields one decision");
        assert!(
            plan.required_deletes.is_empty(),
            "a cache attributable to the declared url must be reused"
        );
    }

    #[test]
    fn a_destination_claimed_by_both_an_asset_and_a_provider_is_refused() {
        // Conflicting declarations make file ownership ambiguous.
        let declared = [remote("providers/p.yaml", "https://one.example/p.yaml")];
        let assets = BTreeSet::from(["providers/p.yaml".to_owned()]);

        let error = declared_remote_providers(&declared, &assets)
            .expect_err("one destination cannot be both copied and downloaded");

        assert!(error.message.contains("copied asset"), "{}", error.message);
    }

    #[test]
    fn an_asset_whose_modification_time_is_unknown_is_never_skipped() {
        // Length alone must not validate an asset with unknown modification time.
        let unknown = SourceIdentity {
            source: "/app/geo.dat".to_owned(),
            len: 10,
            mtime_ns: None,
        };
        let previous = manifest(&[("geo.dat", unknown)], &[]);
        let sources = [AssetSource {
            asset: RuntimeAsset {
                source: "/app/geo.dat".to_owned(),
                destination: "geo.dat".to_owned(),
            },
            len: 10,
            mtime_ns: None,
        }];

        let plan = plan_stage(&previous, &sources, &[]);

        assert_eq!(plan.copies.len(), 1);
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn a_name_shaped_like_a_staging_temporary_is_refused_but_a_lookalike_is_not() {
        use super::destination_key;
        use std::path::Path;

        for temporary in [".config.yaml.staging-123-4", ".geoip.metadb.staging-1-0"] {
            assert!(
                destination_key(Path::new(temporary)).is_err(),
                "{temporary} is one of staging's own temporaries"
            );
        }
        for legitimate in [
            "company.staging-prod.yaml",
            "providers/x.staging-.yaml",
            ".hidden.staging-notanumber",
        ] {
            assert!(
                destination_key(Path::new(legitimate)).is_ok(),
                "{legitimate} is an ordinary name the core would happily load"
            );
        }
    }

    #[test]
    fn the_names_the_generation_owns_are_refused_whatever_their_case() {
        use super::destination_key;
        use std::path::Path;

        // Service-owned names are case-insensitive for Windows compatibility.
        for reserved in [
            "config.yaml",
            "Config.yaml",
            "CONFIG.YAML",
            ".runtime-manifest.json",
            ".Runtime-Manifest.JSON",
        ] {
            assert!(
                destination_key(Path::new(reserved)).is_err(),
                "{reserved} names a file the generation owns"
            );
        }
        // Reserved names apply only at the generation root.
        assert!(destination_key(Path::new("providers/config.yaml")).is_ok());
    }

    #[test]
    fn a_destination_recorded_as_both_kinds_is_swept_once() {
        // Individually sorted maps still require sorting after concatenation.
        let previous = manifest(
            &[
                ("a.yaml", identity("/app/a.yaml", 1, 1)),
                ("z.yaml", identity("/app/z.yaml", 1, 1)),
            ],
            &[("a.yaml", "https://one.example/a.yaml")],
        );

        let plan = plan_stage(&previous, &[], &[]);

        assert_eq!(plan.hygiene_deletes, ["a.yaml", "z.yaml"]);
    }
}
