// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
use crate::core::auth::{AuthenticatedOwner, ServiceError, hash_session_token};
use crate::core::logger::set_or_update_writer;
use crate::core::manager::CORE_MANAGER;
use crate::core::paths::service_paths;
use crate::core::state::set_core_lifecycle_state;
use crate::{ClashConfig, OwnerIdentity, ServiceErrorCode, ServiceLifecycleState, WriterConfig};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::{
    LazyLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Mutex;
use tracing::{info, warn};

static DESIRED_STATE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static STATE_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DesiredState {
    pub core_should_be_running: bool,
    pub last_clash_config: Option<ClashConfig>,
    pub last_writer_config: Option<WriterConfig>,
    pub generation: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveOwnerState {
    pub owner_key: String,
    pub identity: OwnerIdentity,
    pub app_data_root: String,
    #[serde(default)]
    pub generation: u64,
    #[serde(default)]
    pub session_token_hash: String,
}

#[cfg(test)]
impl From<&AuthenticatedOwner> for ActiveOwnerState {
    fn from(owner: &AuthenticatedOwner) -> Self {
        Self {
            owner_key: owner.key.clone(),
            identity: owner.identity.clone(),
            app_data_root: owner.app_data_root.to_string_lossy().into_owned(),
            generation: 0,
            session_token_hash: String::new(),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct OwnerGenerationState {
    generation: u64,
}

///
/// # Errors
/// Returns errors resolving, securing, reading or decoding the owner state file.
pub async fn load_owner_desired_state(owner_key: &str) -> Result<DesiredState> {
    let path = service_paths()?.for_owner_key(owner_key).desired_state_path();
    Ok(read_owner_desired_state_resilient(&path).await)
}

pub async fn persist_owner_core_started(owner: &AuthenticatedOwner, config: &ClashConfig) -> Result<DesiredState> {
    update_owner_desired_state(&owner.key, |state| {
        state.core_should_be_running = true;
        state.last_clash_config = Some(config.clone());
        state.last_writer_config = Some(config.log_config.clone());
    })
    .await
}

pub async fn persist_owner_core_stopped(owner: &AuthenticatedOwner) -> Result<DesiredState> {
    persist_owner_core_stopped_by_key(&owner.key).await
}

pub async fn persist_owner_core_stopped_by_key(owner_key: &str) -> Result<DesiredState> {
    update_owner_desired_state(owner_key, |state| {
        state.core_should_be_running = false;
    })
    .await
}

pub async fn persist_owner_writer_config(owner: &AuthenticatedOwner, config: &WriterConfig) -> Result<DesiredState> {
    update_owner_desired_state(&owner.key, |state| {
        state.last_writer_config = Some(config.clone());
        if let Some(clash_config) = state.last_clash_config.as_mut() {
            clash_config.log_config = config.clone();
        }
    })
    .await
}

///
/// # Errors
/// Returns errors securing, reading or decoding active-owner state; absence is not an error.
pub async fn load_active_owner() -> Result<Option<ActiveOwnerState>> {
    let path = service_paths()?.active_owner_path();
    secure_state_file_if_exists(&path)?;
    match tokio::fs::read(&path).await {
        Ok(content) => serde_json::from_slice(&content)
            .map(Some)
            .with_context(|| format!("failed to parse active owner {path:?}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read active owner {path:?}")),
    }
}

#[cfg(test)]
pub async fn persist_active_owner(owner: &AuthenticatedOwner) -> Result<ActiveOwnerState> {
    let _guard = DESIRED_STATE_LOCK.lock().await;
    let state = ActiveOwnerState::from(owner);
    write_json_atomic(&service_paths()?.active_owner_path(), &state).await?;
    Ok(state)
}

pub async fn commit_active_owner_session(owner: &AuthenticatedOwner, session_token: &str) -> Result<ActiveOwnerState> {
    let session_token_hash = hash_session_token(session_token)?;
    let _guard = DESIRED_STATE_LOCK.lock().await;
    let paths = service_paths()?;
    let generation_path = paths.owner_generation_path();
    let mut generation_state: OwnerGenerationState = read_json_or_default(&generation_path).await?;
    generation_state.generation = generation_state.generation.saturating_add(1);
    write_json_atomic(&generation_path, &generation_state).await?;

    let state = ActiveOwnerState {
        owner_key: owner.key.clone(),
        identity: owner.identity.clone(),
        app_data_root: owner.app_data_root.to_string_lossy().into_owned(),
        generation: generation_state.generation,
        session_token_hash,
    };
    write_json_atomic(&paths.active_owner_path(), &state).await?;
    Ok(state)
}

pub async fn clear_active_owner() -> Result<()> {
    let _guard = DESIRED_STATE_LOCK.lock().await;
    let path = service_paths()?.active_owner_path();
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove active owner {path:?}")),
    }
}

///
/// # Errors
/// Returns persistence, owner-validation or core-recovery errors; restoration may have begun.
pub async fn restore_desired_state() -> Result<()> {
    backup_legacy_desired_states().await;

    let Some(active_owner) = load_active_owner().await? else {
        info!("No active owner to restore");
        return Ok(());
    };
    let state = load_owner_desired_state(&active_owner.owner_key).await?;

    if let Some(writer_config) = state.last_writer_config.as_ref()
        && let Err(error) = set_or_update_writer(writer_config)
    {
        warn!("Failed to restore writer config: {}", error);
    }

    if !state.core_should_be_running {
        info!("Desired state does not require core restore");
        return Ok(());
    }

    let Some(config) = state.last_clash_config else {
        warn!("Desired state requests core restore but has no ClashConfig");
        return Ok(());
    };

    info!("Restoring core from desired state generation {}", state.generation);
    if let Err(error) = CORE_MANAGER
        .lock()
        .await
        .start_core(config, active_owner.identity.clone())
        .await
    {
        // A missing core path makes the desired state stale; retain intent for other errors.
        if core_path_is_unusable(&error) {
            warn!(
                "Persisted core path cannot be launched (missing, moved, or no longer in a \
                 location this build trusts); clearing desired core-run state to stop retrying: {error:#}"
            );
            if let Err(clear_error) = persist_owner_core_stopped_by_key(&active_owner.owner_key).await {
                warn!("Failed to clear stale desired state after not-found core path: {clear_error:#}");
            }
            set_core_lifecycle_state(ServiceLifecycleState::Running);
            return Ok(());
        }
        set_core_lifecycle_state(ServiceLifecycleState::Fatal);
        return Err(error);
    }
    Ok(())
}

/// Clears intent only for core-path validation errors, including metadata/ACL failures.
/// An unrelated `NotFound`, such as a missing volatile IPC directory, must retain intent.
fn core_path_is_unusable(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<ServiceError>().is_some_and(|service_error| {
            matches!(
                service_error.code,
                ServiceErrorCode::InvalidInstallLocation | ServiceErrorCode::InvalidRuntimeAsset
            )
        })
    })
}

async fn backup_legacy_desired_states() {
    let paths = match service_paths() {
        Ok(paths) => paths,
        Err(error) => {
            warn!("Could not locate legacy desired state for backup: {error}");
            return;
        }
    };
    let legacy_files = vec![paths.desired_state_path().to_path_buf()];
    #[cfg(target_os = "macos")]
    let legacy_files = legacy_files
        .into_iter()
        .chain([
            std::path::PathBuf::from("/var/lib/zenclash-service/desired-state.json"),
            std::path::PathBuf::from("/var/root/.local/state/zenclash-service/desired-state.json"),
        ])
        .collect::<Vec<_>>();

    for legacy in legacy_files {
        match backup_legacy_state_file(&legacy).await {
            Ok(Some(backup)) => info!("Backed up legacy desired-state {:?} -> {:?}", legacy, backup),
            Ok(None) => {}
            Err(error) => warn!("Failed to back up legacy desired-state {:?}: {}", legacy, error),
        }
    }
}

async fn backup_legacy_state_file(path: &std::path::Path) -> Result<Option<std::path::PathBuf>> {
    match tokio::fs::try_exists(path).await {
        Ok(false) => return Ok(None),
        Ok(true) => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect legacy state {path:?}"));
        }
    }

    let backup = path.with_extension("json.legacy.bak");
    tokio::fs::rename(path, &backup)
        .await
        .with_context(|| format!("failed to back up legacy state {path:?} to {backup:?}"))?;
    Ok(Some(backup))
}

async fn update_owner_desired_state(owner_key: &str, update: impl FnOnce(&mut DesiredState)) -> Result<DesiredState> {
    let _guard = DESIRED_STATE_LOCK.lock().await;
    let path = service_paths()?.for_owner_key(owner_key).desired_state_path();
    let mut state = read_owner_desired_state_resilient(&path).await;
    update(&mut state);
    state.generation = state.generation.saturating_add(1);
    state.updated_at = unix_timestamp_secs();
    if let Err(error) = write_json_atomic(&path, &state).await {
        warn!(
            "Failed to persist owner desired state {:?}; continuing with live core state: {error:#}",
            path
        );
    }
    Ok(state)
}

// Desired state is best-effort; authorization state remains strict.
async fn read_owner_desired_state_resilient(path: &std::path::Path) -> DesiredState {
    if let Err(error) = secure_state_file_if_exists(path) {
        warn!(
            "Owner desired state {:?} cannot be secured; ignoring the persisted recovery hint: {error:#}",
            path
        );
        return DesiredState::default();
    }

    let content = match tokio::fs::read(path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return DesiredState::default();
        }
        Err(error) => {
            warn!(
                "Owner desired state {:?} cannot be read; ignoring the persisted recovery hint: {error:#}",
                path
            );
            return DesiredState::default();
        }
    };

    match serde_json::from_slice(&content) {
        Ok(state) => state,
        Err(parse_error) => {
            match quarantine_corrupt_owner_desired_state(path).await {
                Ok(backup) => warn!(
                    "Quarantined corrupt owner desired state {:?} at {:?}: {}",
                    path, backup, parse_error
                ),
                Err(quarantine_error) => warn!(
                    "Owner desired state {:?} is corrupt and could not be quarantined: {}; quarantine failed: {quarantine_error:#}",
                    path, parse_error
                ),
            }
            DesiredState::default()
        }
    }
}

async fn quarantine_corrupt_owner_desired_state(path: &std::path::Path) -> Result<std::path::PathBuf> {
    let backup = sibling_state_path(path, "corrupt");
    tokio::fs::rename(path, &backup)
        .await
        .with_context(|| format!("failed to quarantine corrupt state {path:?} at {backup:?}"))?;
    Ok(backup)
}

async fn read_json_or_default<T>(path: &std::path::Path) -> Result<T>
where
    T: for<'de> Deserialize<'de> + Default,
{
    secure_state_file_if_exists(path)?;
    match tokio::fs::read(path).await {
        Ok(content) => serde_json::from_slice(&content).with_context(|| format!("failed to parse state {path:?}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error).with_context(|| format!("failed to read state {path:?}")),
    }
}

async fn write_json_atomic<T>(path: &std::path::Path, value: &T) -> Result<()>
where
    T: Serialize + Sync,
{
    crate::core::paths::ensure_persistent_state_layout()?;
    if let Some(parent) = path.parent() {
        crate::core::platform_security::ensure_private_service_directory(parent)?;
    }

    let temp_path = sibling_state_path(path, "tmp");
    let json = serde_json::to_vec_pretty(value)?;
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .await
            .with_context(|| format!("failed to create state temp file {temp_path:?}"))?;
        file.write_all(&json)
            .await
            .with_context(|| format!("failed to write state temp file {temp_path:?}"))?;
        file.flush()
            .await
            .with_context(|| format!("failed to flush state temp file {temp_path:?}"))?;
        file.sync_all()
            .await
            .with_context(|| format!("failed to sync state temp file {temp_path:?}"))?;
        drop(file);
        secure_state_file_if_exists(&temp_path)?;
        crate::core::atomic_file::replace(&temp_path, path)
            .await
            .with_context(|| format!("failed to move state into {path:?}"))?;
        secure_state_file_if_exists(path)?;
        Ok(())
    }
    .await;

    if result.is_err()
        && let Err(cleanup_error) = tokio::fs::remove_file(&temp_path).await
        && cleanup_error.kind() != std::io::ErrorKind::NotFound
    {
        warn!("Failed to clean up state temp file {:?}: {}", temp_path, cleanup_error);
    }
    result
}

fn sibling_state_path(path: &std::path::Path, role: &str) -> std::path::PathBuf {
    let sequence = STATE_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(
        "{file_name}.{role}-{}-{timestamp}-{sequence}",
        std::process::id()
    ))
}

fn secure_state_file_if_exists(path: &std::path::Path) -> Result<()> {
    crate::core::platform_security::secure_private_service_file_if_exists(path)?;
    Ok(())
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
