// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
//! Runtime generation planning, materialization, and stale generation cleanup.

use super::validate::{
    RUNTIME_CONFIG_FILE_NAME, ResolvedCore, destination_key, invalid_asset, prepare_owner_ipc_directory,
    resolve_in_generation, set_private_directory_permissions, validate_core_path, validate_destination,
    validate_source,
};
use crate::core::auth::{AuthenticatedOwner, ServiceError};
use crate::core::paths::ensure_owner_state_directory;
use crate::core::runtime_generation::staging;
use crate::{ClashConfig, CoreConfig, RuntimeBundle, WriterConfig, mihomo_ipc_path};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Stable per-owner directory that preserves core-managed state across restarts.
const RUNTIME_GENERATION_DIRECTORY_NAME: &str = "runtime";

#[cfg(windows)]
const WINDOWS_RUNTIME_RETRY_DELAYS: [Duration; 6] = [
    Duration::from_millis(25),
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(200),
    Duration::from_millis(400),
    Duration::from_millis(800),
];

/// A planned refresh of the owner's durable runtime generation.
/// Old per-start directories are retired only after the new core starts successfully.
#[derive(Debug)]
pub(crate) struct PreparedRuntime {
    pub(super) clash_config: ClashConfig,
    runtime: PathBuf,
    stale_runtime_paths: Vec<PathBuf>,
    plan: staging::StagePlan,
    yaml: String,
}

impl PreparedRuntime {
    pub(crate) fn clash_config(&self) -> &ClashConfig {
        &self.clash_config
    }

    /// Writes the plan after the outgoing core has stopped.
    /// Keeping this separate from planning avoids touching files still held open by that core.
    pub(crate) async fn materialize(&self) -> Result<(), ServiceError> {
        materialize_plan(&self.runtime, &self.plan, &self.yaml).await
    }

    /// Retires directories left by the old per-start layout after a successful start.
    pub(crate) fn commit(mut self) {
        let stale_paths = std::mem::take(&mut self.stale_runtime_paths);
        if stale_paths.is_empty() {
            return;
        }
        let active_runtime = self.runtime.clone();
        tokio::spawn(async move {
            cleanup_stale_runtime_directories(stale_paths, active_runtime).await;
        });
    }
}

async fn inspect_path(path: &Path) -> String {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => "symlink".to_owned(),
        Ok(metadata) if metadata.is_dir() => "directory".to_owned(),
        Ok(metadata) if metadata.is_file() => "file".to_owned(),
        Ok(_) => "other".to_owned(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "missing".to_owned(),
        Err(error) => format!("inaccessible: {error}"),
    }
}

pub(super) async fn snapshot_stale_runtime_directories(owner_root: &Path, active_runtime: &Path) -> Vec<PathBuf> {
    let mut stale_paths = Vec::new();
    let mut entries = match tokio::fs::read_dir(owner_root).await {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                owner_root = ?owner_root,
                error = %error,
                "Failed to enumerate stale runtime directories"
            );
            return stale_paths;
        }
    };

    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(
                    owner_root = ?owner_root,
                    error = %error,
                    "Failed while enumerating stale runtime directories"
                );
                break;
            }
        };
        let path = entry.path();
        if path == active_runtime {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let is_runtime_directory = name == "runtime"
            || name == "runtime.backup"
            || name.starts_with("runtime.generation-")
            || name.starts_with("runtime.staging-");
        if !is_runtime_directory {
            continue;
        }
        stale_paths.push(path);
    }
    stale_paths
}

pub(super) async fn cleanup_stale_runtime_directories(stale_paths: Vec<PathBuf>, active_runtime: PathBuf) {
    for path in stale_paths {
        if let Err(error) = remove_runtime_directory(&path, "failed to remove stale runtime directory").await {
            let state = inspect_path(&path).await;
            tracing::warn!(
                path = ?path,
                state = %state,
                error = %error,
                active_runtime = ?active_runtime,
                "Failed to remove stale runtime directory after committing new generation"
            );
        }
    }
}

async fn remove_runtime_directory(path: &Path, operation: &str) -> std::io::Result<()> {
    let mut retry_index = 0;
    loop {
        match tokio::fs::remove_dir_all(path).await {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                let Some(delay) = runtime_cleanup_retry_delay(&error, retry_index) else {
                    return Err(error);
                };
                retry_index += 1;
                tracing::warn!(
                    path = ?path,
                    retry = retry_index,
                    delay_ms = delay.as_millis(),
                    error = %error,
                    operation,
                    "Retrying transient Windows runtime directory cleanup failure"
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}

/// Returns a bounded retry delay for Windows errors caused by live file handles.
/// Mapped files are included so callers eventually fall back to restarting the core.
#[cfg(windows)]
pub(in crate::core::runtime_generation) fn runtime_cleanup_retry_delay(
    error: &std::io::Error,
    retry_index: usize,
) -> Option<Duration> {
    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_DELETE_PENDING, ERROR_DIR_NOT_EMPTY, ERROR_SHARING_VIOLATION,
        ERROR_UNABLE_TO_MOVE_REPLACEMENT, ERROR_UNABLE_TO_MOVE_REPLACEMENT_2, ERROR_UNABLE_TO_REMOVE_REPLACED,
        ERROR_USER_MAPPED_FILE,
    };

    matches!(
        error.raw_os_error(),
        Some(code)
            if code == ERROR_ACCESS_DENIED as i32
                || code == ERROR_SHARING_VIOLATION as i32
                || code == ERROR_DIR_NOT_EMPTY as i32
                || code == ERROR_DELETE_PENDING as i32
                || code == ERROR_UNABLE_TO_REMOVE_REPLACED as i32
                || code == ERROR_UNABLE_TO_MOVE_REPLACEMENT as i32
                || code == ERROR_UNABLE_TO_MOVE_REPLACEMENT_2 as i32
                || code == ERROR_USER_MAPPED_FILE as i32
    )
    .then(|| WINDOWS_RUNTIME_RETRY_DELAYS.get(retry_index).copied())
    .flatten()
}

#[cfg(not(windows))]
pub(in crate::core::runtime_generation) fn runtime_cleanup_retry_delay(
    _error: &std::io::Error,
    _retry_index: usize,
) -> Option<Duration> {
    None
}

/// Ensures the private, durable generation used by both normal starts and desired-state restore.
/// Reuse preserves core-owned state such as `cache.db` selections and fake-IP leases.
async fn ensure_runtime_generation(owner_root: &Path) -> Result<PathBuf, ServiceError> {
    let runtime = owner_root.join(RUNTIME_GENERATION_DIRECTORY_NAME);
    tokio::fs::create_dir_all(&runtime)
        .await
        .map_err(|error| invalid_asset(format!("failed to create runtime generation {runtime:?}: {error}")))?;
    set_private_directory_permissions(&runtime).await?;
    Ok(runtime)
}

pub(crate) async fn prepare_runtime(
    owner: &AuthenticatedOwner,
    bundle: &RuntimeBundle,
) -> Result<PreparedRuntime, ServiceError> {
    let core = validate_core_path(&bundle.core_path)?;
    let owner_paths = ensure_owner_state_directory(&owner.identity)
        .map_err(|error| invalid_asset(format!("failed to secure owner state root: {error:#}")))?;
    let owner_root = owner_paths.root();
    crate::core::maintenance::persist_owner_identity(&owner.identity, owner_root)
        .await
        .map_err(|error| invalid_asset(format!("failed to persist owner identity: {error:#}")))?;
    prepare_owner_ipc_directory(owner).await?;

    let logs = owner_paths.logs_dir();
    tokio::fs::create_dir_all(&logs)
        .await
        .map_err(|error| invalid_asset(format!("failed to create owner log directory: {error}")))?;
    set_private_directory_permissions(&logs).await?;
    let log_config = WriterConfig {
        directory: logs.to_string_lossy().into_owned(),
        ..Default::default()
    };

    let runtime = ensure_runtime_generation(owner_root).await?;
    let mut prepared = PreparedRuntime {
        clash_config: ClashConfig {
            core_config: CoreConfig {
                core_path: core.executable().to_string_lossy().into_owned(),
                core_ipc_path: mihomo_ipc_path(&owner.identity),
                config_path: runtime.join(RUNTIME_CONFIG_FILE_NAME).to_string_lossy().into_owned(),
                config_dir: runtime.to_string_lossy().into_owned(),
            },
            log_config,
        },
        runtime: runtime.clone(),
        stale_runtime_paths: Vec::new(),
        plan: plan_runtime_refresh(owner, bundle, &core, &runtime).await?,
        yaml: bundle.yaml.clone(),
    };
    prepared.stale_runtime_paths = snapshot_stale_runtime_directories(owner_root, &runtime).await;
    Ok(prepared)
}

/// Plans a runtime refresh without writing; only manifest-recorded files may be deleted.
/// A missing manifest copies all declared assets and preserves unknown core-owned files.
async fn plan_runtime_refresh(
    owner: &AuthenticatedOwner,
    bundle: &RuntimeBundle,
    core: &ResolvedCore,
    runtime: &Path,
) -> Result<staging::StagePlan, ServiceError> {
    let gathered = gather_bundle(owner, bundle, core).await?;
    // A corrupt manifest must not prevent the restart that staging uses as its fallback.
    // Treat it as untrusted: copy everything and sweep nothing.
    let previous = staging::read_manifest(runtime).await.unwrap_or_else(|detail| {
        tracing::warn!(
            detail = %detail,
            "Rebuilding the runtime generation from nothing: its manifest could not be read"
        );
        staging::RuntimeManifest::default()
    });
    Ok(staging::plan_stage(&previous, &gathered.sources, &gathered.remote))
}

/// Applies a plan, committing `config.yaml` last so partial failure leaves the old configuration.
async fn materialize_plan(runtime: &Path, plan: &staging::StagePlan, yaml: &str) -> Result<(), ServiceError> {
    // Do not record a source that changed while it was copied.
    let mut manifest = plan.manifest.clone();
    for destination in &plan.required_deletes {
        let target = resolve_in_generation(runtime, destination)?;
        staging::remove_staged_file(&target)
            .await
            .map_err(|error| invalid_asset(format!("failed to discard the stale cache {destination}: {error}")))?;
    }

    for copy in &plan.copies {
        let target = resolve_in_generation(runtime, &copy.destination)?;
        staging::copy_staged_file(&copy.source, &target)
            .await
            .map_err(|error| {
                invalid_asset(format!(
                    "failed to write the runtime asset {}: {error}",
                    copy.destination
                ))
            })?;
        // Re-stat after copying. If the source moved, omit its proof so the next staging retries;
        // a concurrent geo-data update should not fail the current start.
        if staging::source_identity_changed(&copy.source, &copy.identity).await {
            tracing::warn!(
                destination = %copy.destination,
                "Runtime asset changed while being copied; not recording what it was copied from"
            );
            manifest.assets.remove(&copy.destination);
        }
    }

    let config_path = runtime.join(RUNTIME_CONFIG_FILE_NAME);
    if let Err(error) = staging::commit_staged_config(runtime, &config_path, yaml, &manifest).await {
        // A manifest written before a failed config commit may claim cache provenance for a URL
        // the core never loaded; discard it rather than trust stale provider data next time.
        if let Err(discard) = staging::remove_staged_file(&runtime.join(staging::MANIFEST_FILE_NAME)).await {
            tracing::warn!(
                error = %discard,
                "Left a manifest behind that describes an uncommitted configuration"
            );
        }
        return Err(invalid_asset(format!(
            "failed to commit the runtime configuration: {error}"
        )));
    }

    // Post-commit cleanup is best-effort because it cannot affect what the core observes.
    for destination in &plan.hygiene_deletes {
        match resolve_in_generation(runtime, destination) {
            Ok(target) => {
                if let Err(error) = staging::remove_staged_file(&target).await {
                    tracing::warn!(
                        destination = %destination,
                        error = %error,
                        "Left an undeclared file behind in the runtime generation"
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
        "Prepared the runtime generation"
    );
    Ok(())
}

/// Validated, stat'd bundle data shared by start and staging plans.
pub(in crate::core::runtime_generation) struct GatheredBundle {
    pub sources: Vec<staging::AssetSource>,
    pub remote: Vec<crate::RemoteProvider>,
}

pub(in crate::core::runtime_generation) async fn gather_bundle(
    owner: &AuthenticatedOwner,
    bundle: &RuntimeBundle,
    core: &ResolvedCore,
) -> Result<GatheredBundle, ServiceError> {
    let app_bundle_root = core.trusted_asset_bundle_root();
    let mut sources = Vec::with_capacity(bundle.assets.len());
    let mut asset_keys = std::collections::BTreeSet::new();
    for asset in &bundle.assets {
        let source = validate_source(owner, app_bundle_root.as_deref(), &asset.source)?;
        let destination = destination_key(&validate_destination(&asset.destination)?)?;
        let metadata = tokio::fs::metadata(&source)
            .await
            .map_err(|error| invalid_asset(format!("failed to inspect runtime asset {source:?}: {error}")))?;
        if !asset_keys.insert(destination.clone()) {
            return Err(invalid_asset(format!(
                "runtime destination {destination:?} is declared as a copied asset twice"
            )));
        }
        sources.push(staging::AssetSource {
            asset: crate::RuntimeAsset {
                source: source.to_string_lossy().into_owned(),
                destination,
            },
            len: metadata.len(),
            mtime_ns: staging::modified_nanos(&metadata),
        });
    }
    let remote = staging::declared_remote_providers(&bundle.remote_providers, &asset_keys)?;
    Ok(GatheredBundle { sources, remote })
}
