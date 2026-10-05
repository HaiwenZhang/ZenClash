// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
use crate::core::auth::{AuthenticatedOwner, ServiceError, hash_session_token};
use crate::core::logger::set_or_update_writer;
use crate::core::manager::CORE_MANAGER;
use crate::core::paths::service_paths;
use crate::core::state::set_core_lifecycle_state;
use crate::{ClashConfig, OwnerIdentity, ServiceErrorCode, ServiceLifecycleState, WriterConfig};
use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Mutex;
use tracing::{info, warn};

static DESIRED_STATE_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
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
mod owner_tests {
    use super::{
        backup_legacy_state_file, clear_active_owner, commit_active_owner_session, load_active_owner,
        load_owner_desired_state, persist_active_owner, persist_owner_core_started, persist_owner_core_stopped,
        write_json_atomic,
    };
    use crate::core::auth::AuthenticatedOwner;
    use crate::{ClashConfig, CoreConfig, OwnerIdentity};
    use serial_test::serial;

    fn test_owner(uid: u32) -> AuthenticatedOwner {
        AuthenticatedOwner {
            key: uid.to_string(),
            identity: OwnerIdentity::Unix { uid, gid: 20 },
            app_data_root: std::env::temp_dir(),
        }
    }

    #[tokio::test]
    #[serial]
    async fn unusable_restored_core_does_not_reserve_execution() -> anyhow::Result<()> {
        let owner = test_owner(90_019);
        let directory = std::env::temp_dir().join(format!("missing-core-restore-{}", std::process::id()));
        std::fs::create_dir_all(&directory)?;
        let config = ClashConfig {
            core_config: CoreConfig {
                core_path: "/missing/mihomo".into(),
                core_ipc_path: directory.join("core.sock").to_string_lossy().into_owned(),
                ..Default::default()
            },
            log_config: Default::default(),
        };
        persist_owner_core_started(&owner, &config).await?;
        persist_active_owner(&owner).await?;
        assert!(crate::core::installation::inspect(&[], false).await?.core_busy);
        super::restore_desired_state().await?;
        let status = crate::core::installation::inspect(&[], false).await?;
        let occupied = status.core_busy;
        assert!(status.service_sha256.is_empty());
        clear_active_owner().await?;
        std::fs::remove_dir_all(directory)?;
        assert!(!occupied, "a failed restore with no core must permit Sidecar handoff");
        Ok(())
    }

    #[tokio::test]
    async fn desired_state_is_scoped_by_owner_key() -> anyhow::Result<()> {
        let owner_a = test_owner(90_001);
        let owner_b = test_owner(90_002);
        let config = ClashConfig {
            core_config: CoreConfig {
                core_path: "/tmp/mock-core-a".to_string(),
                ..Default::default()
            },
            log_config: Default::default(),
        };

        persist_owner_core_started(&owner_a, &config).await?;
        persist_owner_core_stopped(&owner_b).await?;

        assert!(load_owner_desired_state(&owner_a.key).await?.core_should_be_running);
        assert!(!load_owner_desired_state(&owner_b.key).await?.core_should_be_running);
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn active_owner_can_be_atomically_replaced_and_cleared() -> anyhow::Result<()> {
        let owner_a = test_owner(90_003);
        let owner_b = test_owner(90_004);

        persist_active_owner(&owner_a).await?;
        assert_eq!(
            load_active_owner()
                .await?
                .as_ref()
                .map(|state| state.owner_key.as_str()),
            Some("90003")
        );

        persist_active_owner(&owner_b).await?;
        assert_eq!(
            load_active_owner()
                .await?
                .as_ref()
                .map(|state| state.owner_key.as_str()),
            Some("90004")
        );

        clear_active_owner().await?;
        assert!(load_active_owner().await?.is_none());
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn owner_generation_survives_active_owner_clear() -> anyhow::Result<()> {
        clear_active_owner().await?;
        let owner = test_owner(90_005);
        let first_token = "33".repeat(32);
        let first = commit_active_owner_session(&owner, &first_token).await?;

        assert_ne!(first.session_token_hash, first_token);
        assert!(
            !tokio::fs::read_to_string(crate::service_paths()?.active_owner_path())
                .await?
                .contains(&first_token)
        );

        clear_active_owner().await?;
        let second = commit_active_owner_session(&owner, &"44".repeat(32)).await?;

        assert!(second.generation > first.generation);
        clear_active_owner().await?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn legacy_active_owner_defaults_session_fields() -> anyhow::Result<()> {
        clear_active_owner().await?;
        let owner = test_owner(90_006);
        let legacy_state = serde_json::json!({
            "owner_key": owner.key,
            "identity": owner.identity,
            "app_data_root": owner.app_data_root.to_string_lossy(),
        });
        write_json_atomic(&crate::service_paths()?.active_owner_path(), &legacy_state).await?;

        let active = load_active_owner().await?.expect("legacy owner should load");

        assert_eq!(active.generation, 0);
        assert!(active.session_token_hash.is_empty());
        clear_active_owner().await?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn session_commit_rejects_non_lowercase_hex_tokens() -> anyhow::Result<()> {
        clear_active_owner().await?;
        let owner = test_owner(90_007);

        for invalid in ["55".repeat(31), "AA".repeat(32), "gg".repeat(32)] {
            assert!(commit_active_owner_session(&owner, &invalid).await.is_err());
        }
        assert!(load_active_owner().await?.is_none());
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn corrupt_owner_desired_state_is_quarantined_and_rebuilt() -> anyhow::Result<()> {
        let owner = test_owner(90_008);
        let owner_root = crate::service_paths()?.for_owner_key(&owner.key).root().to_path_buf();
        let _ = std::fs::remove_dir_all(&owner_root);
        std::fs::create_dir_all(&owner_root)?;
        let path = owner_root.join("desired-state.json");
        let damaged = b"\0damaged-json";
        std::fs::write(&path, damaged)?;

        let state = persist_owner_core_started(&owner, &ClashConfig::default()).await?;

        assert!(state.core_should_be_running);
        assert!(load_owner_desired_state(&owner.key).await?.core_should_be_running);
        let backups = std::fs::read_dir(&owner_root)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("desired-state.json.corrupt-"))
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read(&backups[0])?, damaged);
        std::fs::remove_dir_all(owner_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn corrupt_owner_desired_state_load_returns_safe_default() -> anyhow::Result<()> {
        let owner = test_owner(90_010);
        let owner_root = crate::service_paths()?.for_owner_key(&owner.key).root().to_path_buf();
        let _ = std::fs::remove_dir_all(&owner_root);
        std::fs::create_dir_all(&owner_root)?;
        let path = owner_root.join("desired-state.json");
        let damaged = b"\0damaged-before-restore";
        std::fs::write(&path, damaged)?;

        let state = load_owner_desired_state(&owner.key).await?;

        assert!(!state.core_should_be_running);
        assert!(state.last_clash_config.is_none());
        assert!(!path.exists());
        let backup = std::fs::read_dir(&owner_root)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("desired-state.json.corrupt-"))
            })
            .ok_or_else(|| anyhow::anyhow!("corrupt desired state was not quarantined"))?;
        assert_eq!(std::fs::read(backup)?, damaged);
        std::fs::remove_dir_all(owner_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn unavailable_owner_desired_state_does_not_reject_live_state_change() -> anyhow::Result<()> {
        let owner = test_owner(90_009);
        let owner_root = crate::service_paths()?.for_owner_key(&owner.key).root().to_path_buf();
        let _ = std::fs::remove_dir_all(&owner_root);
        std::fs::create_dir_all(owner_root.join("desired-state.json"))?;

        let state = persist_owner_core_started(&owner, &ClashConfig::default()).await?;

        assert!(state.core_should_be_running);
        assert!(owner_root.join("desired-state.json").is_dir());
        std::fs::remove_dir_all(owner_root)?;
        Ok(())
    }

    #[tokio::test]
    async fn legacy_global_state_is_backed_up_without_becoming_owner_state() -> anyhow::Result<()> {
        let root = std::env::temp_dir().join(format!("legacy-desired-state-backup-{}", std::process::id()));
        let legacy = root.join("desired-state.json");
        let backup = root.join("desired-state.json.legacy.bak");
        std::fs::create_dir_all(&root)?;
        std::fs::write(&legacy, br#"{"core_should_be_running":true}"#)?;

        backup_legacy_state_file(&legacy).await?;

        assert!(!legacy.exists());
        assert_eq!(std::fs::read(&backup)?, br#"{"core_should_be_running":true}"#);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}

#[cfg(test)]
mod core_path_recovery_tests {
    use super::core_path_is_unusable;
    use crate::ServiceErrorCode;
    use crate::core::auth::ServiceError;

    #[test]
    fn a_rejected_core_location_clears_the_desired_state_instead_of_wedging_startup() {
        let error = anyhow::Error::new(ServiceError::new(
            ServiceErrorCode::InvalidInstallLocation,
            "core path is outside /Applications",
        ))
        .context("failed to start core");

        assert!(core_path_is_unusable(&error));
    }

    #[test]
    fn a_vanished_core_binary_is_still_treated_as_unusable() {
        let error = anyhow::Error::new(ServiceError::new(
            ServiceErrorCode::InvalidRuntimeAsset,
            "core is unavailable: No such file or directory (os error 2)",
        ));

        assert!(core_path_is_unusable(&error));
    }

    #[test]
    fn a_missing_volatile_ipc_directory_no_longer_wipes_a_good_desired_state() {
        let error = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::NotFound))
            .context("failed to open core IPC directory");

        assert!(!core_path_is_unusable(&error));
    }

    #[test]
    fn an_unrelated_start_failure_retains_the_desired_state() {
        let error = anyhow::anyhow!("core IPC socket did not appear");

        assert!(!core_path_is_unusable(&error));
    }
}
