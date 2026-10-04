//! Persistent, non-destructive configuration overrides for active profiles.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;
use serde_yaml::Value;
use thiserror::Error;

use crate::{
    CoreKind, MihomoClient, MihomoError, MihomoProcess,
    data_coordinator::{DataWriteAccess, DataWriteLease, shared_transaction},
    listener_fallback::{
        SessionListenerFallback, apply_session_fallbacks, resolve_conflicts,
        validate_listener_change,
    },
    profile::{merge_payload_overrides, merge_profile_patch, merge_yaml},
    profiles::{MAX_PROFILE_BYTES, atomic_write, read_profile_bytes},
};

const MEOW_DEFAULT_NAMESERVERS: [&str; 2] = ["223.5.5.5", "1.1.1.1"];
const MIHOMO_GEOX_URLS: [(&str, &str); 3] = [
    (
        "geoip",
        "https://testingcf.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@release/geoip.dat",
    ),
    (
        "geosite",
        "https://testingcf.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@release/geosite.dat",
    ),
    (
        "mmdb",
        "https://testingcf.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@release/geoip.metadb",
    ),
];

mod storage;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use storage::CommitGate;
use storage::{RuntimeCacheTransaction, default_data_dir, require_mapping};

#[must_use]
pub(crate) struct RuntimeApplicationTransaction {
    cache: RuntimeCacheTransaction,
    recovery: RuntimeApplicationRecovery,
    _mutation_guard: tokio::sync::OwnedMutexGuard<()>,
    _write_lease: DataWriteLease,
}

#[must_use]
pub(crate) struct RuntimeCandidateValidation {
    _mutation_guard: tokio::sync::OwnedMutexGuard<()>,
    _write_lease: DataWriteLease,
}

enum RuntimeApplicationRecovery {
    HotReload {
        runtime: Box<crate::client::AppliedConfig>,
        previous_payload: Option<String>,
    },
    Restart {
        process: Arc<MihomoProcess>,
        cancelled: Option<Arc<std::sync::atomic::AtomicBool>>,
    },
}

struct AcceptedRuntime {
    cache: RuntimeCacheTransaction,
    runtime: crate::client::AppliedConfig,
    previous_payload: Option<String>,
}

pub(crate) struct ServiceTunPersistence {
    cache: RuntimeCacheTransaction,
    store: ControlledConfigStore,
    update: ControlledConfigUpdate,
}

impl ServiceTunPersistence {
    pub(crate) async fn save(&self) -> ControlledConfigResult<()> {
        let store = self.store.clone();
        let update = self.update.clone();
        tokio::task::spawn_blocking(move || store.commit(&update))
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }
    pub(crate) async fn validate_patch(&self) -> ControlledConfigResult<()> {
        let store = self.store.clone();
        let expected = self.update.expected_patch.clone();
        tokio::task::spawn_blocking(move || {
            let _transaction = store.transaction.lock();
            if store.current_patch_bytes_unlocked()? != expected {
                return Err(ControlledConfigError::ConcurrentModification);
            }
            Ok(())
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }
    pub(crate) async fn rollback(self) -> ControlledConfigResult<()> {
        rollback_runtime_cache(self.cache).await
    }
    pub(crate) fn saved(self) {
        self.cache.commit();
    }
}

/// Receipt created only after durable business persistence, even if finalization fails.
pub(crate) struct SavedRuntimeReceipt {
    pub(crate) delta: Option<serde_json::Value>,
    pub(crate) confirmation: ControlledConfigResult<()>,
}

pub(crate) struct ServiceStartupPersistence(RuntimeCacheTransaction);

impl ServiceStartupPersistence {
    pub(crate) fn saved(self) {
        self.0.commit();
    }

    pub(crate) async fn rollback(self) -> ControlledConfigResult<()> {
        rollback_runtime_cache(self.0).await
    }
}

impl AcceptedRuntime {
    async fn commit(self) -> ControlledConfigResult<()> {
        self.cache.finalize(self.runtime.commit()).await
    }

    async fn rollback(self) -> ControlledConfigResult<()> {
        let cache = rollback_runtime_cache(self.cache).await;
        let runtime = self
            .runtime
            .rollback(self.previous_payload)
            .await
            .map_err(ControlledConfigError::Profile);
        match (cache, runtime) {
            (Ok(()), Ok(())) => Ok(()),
            (cache, runtime) => Err(ControlledConfigError::Transaction(format!(
                "缓存恢复：{}；运行内核恢复：{}",
                result_label(cache),
                result_label(runtime)
            ))),
        }
    }
}

impl RuntimeApplicationTransaction {
    pub(crate) async fn commit(self) -> ControlledConfigResult<()> {
        // Persistence has succeeded. A lost service ack must never invoke cache Drop rollback.
        if let RuntimeApplicationRecovery::HotReload { runtime, .. } = self.recovery {
            self.cache.finalize(runtime.commit()).await
        } else {
            self.cache.commit();
            Ok(())
        }
    }

    pub(crate) async fn rollback(self) -> ControlledConfigResult<()> {
        match self.recovery {
            RuntimeApplicationRecovery::HotReload {
                runtime,
                previous_payload,
            } => {
                let cache = rollback_runtime_cache(self.cache).await;
                let runtime = runtime
                    .rollback(previous_payload)
                    .await
                    .map_err(ControlledConfigError::Profile);
                match (cache, runtime) {
                    (Ok(()), Ok(())) => Ok(()),
                    (cache, runtime) => Err(ControlledConfigError::Transaction(format!(
                        "缓存恢复：{}；运行内核恢复：{}",
                        result_label(cache),
                        result_label(runtime)
                    ))),
                }
            }
            RuntimeApplicationRecovery::Restart { process, cancelled } => {
                let cache = rollback_runtime_cache(self.cache).await;
                let runtime = if cache.is_ok() {
                    process
                        .restart_and_wait_until_with_lease(
                            std::time::Duration::from_secs(20),
                            cancelled,
                            &self._write_lease,
                        )
                        .await
                        .map_err(ControlledConfigError::Profile)
                } else {
                    Err(ControlledConfigError::Transaction(
                        "启动缓存未恢复，拒绝使用候选缓存再次启动内核".into(),
                    ))
                };
                match (cache, runtime) {
                    (Ok(()), Ok(())) => Ok(()),
                    (cache, runtime) => Err(ControlledConfigError::Transaction(format!(
                        "缓存恢复：{}；运行内核恢复：{}",
                        result_label(cache),
                        result_label(runtime)
                    ))),
                }
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct RuntimeMutationError {
    pub(crate) cause: ControlledConfigError,
    pub(crate) attempted: bool,
}

impl RuntimeMutationError {
    fn attempted(cause: ControlledConfigError) -> Self {
        Self {
            cause,
            attempted: true,
        }
    }
}

impl From<ControlledConfigError> for RuntimeMutationError {
    fn from(cause: ControlledConfigError) -> Self {
        Self {
            cause,
            attempted: false,
        }
    }
}

impl From<MihomoError> for RuntimeMutationError {
    fn from(cause: MihomoError) -> Self {
        ControlledConfigError::Profile(cause).into()
    }
}

/// Errors produced while preparing or persisting controlled configuration.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ControlledConfigError {
    /// Filesystem access failed.
    #[error("受控配置 I/O 错误：{0}")]
    Io(#[from] std::io::Error),
    /// YAML parsing or serialization failed.
    #[error("受控配置 YAML 无效：{0}")]
    Yaml(#[from] serde_yaml::Error),
    /// JSON input could not be converted to YAML.
    #[error("受控配置 JSON 无效：{0}")]
    Json(#[from] serde_json::Error),
    /// A profile or merged payload was rejected before persistence.
    #[error("无法应用受控 Mihomo 配置：{0}")]
    Profile(#[from] MihomoError),
    /// The override document was not a YAML mapping.
    #[error("受控配置根节点必须是映射")]
    NotMapping,
    /// The override changed after an update was prepared.
    #[error("受控配置已被其他操作修改，请刷新后重试")]
    ConcurrentModification,
    /// An enabled YAML override would persist a different outbound mode.
    #[error("{}", zenclash_i18n::text_with("mode.errors.override_conflict", &[("requested", requested.clone()), ("effective", effective.clone())]))]
    ModeOverrideConflict {
        /// Outbound mode requested by the caller.
        requested: String,
        /// Outbound mode supplied by the enabled YAML override chain.
        effective: String,
    },
    /// An enabled override conflicts with a requested partial configuration field.
    #[error("{}", zenclash_i18n::text("overrides.errors.partial_conflict"))]
    PartialOverrideConflict,
    /// The override exceeded the defensive file-size limit.
    #[error("受控配置超过 16 MiB 限制")]
    TooLarge,
    /// A platform data directory could not be determined.
    #[error("无法确定受控配置目录")]
    MissingDataDirectory,
    /// An asynchronous preparation or commit worker ended unexpectedly.
    #[error("受控配置后台任务异常结束：{0}")]
    Task(String),
    /// Persistence failed after Mihomo accepted the update.
    #[error("受控配置事务失败：{0}")]
    Transaction(String),
    /// The selected runtime core cannot perform an operation in its current mode.
    #[error("当前内核不支持该配置操作：{0}")]
    UnsupportedCoreOperation(String),
    /// Listener probing or session-only fallback failed.
    #[error("代理监听端口处理失败：{0}")]
    ListenerFallback(String),
}

/// Result type for controlled-configuration operations.
pub type ControlledConfigResult<T> = Result<T, ControlledConfigError>;

/// One listener port moved for the current managed-core session only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListenerPortFallback {
    /// Clash/Mihomo configuration key, such as `mixed-port`.
    pub listener: String,
    /// Port requested by the effective persistent configuration.
    pub original: u16,
    /// Available port selected for this process session.
    pub current: u16,
}

/// Prepared controlled-config mutation that has not been persisted yet.
///
/// The new effective payload should first be accepted by Mihomo. Call
/// [`ControlledConfigStore::commit`] only after that succeeds. If persistence
/// then fails, reload [`Self::previous_payload`] to restore runtime state.
#[derive(Clone, Debug)]
pub struct ControlledConfigUpdate {
    expected_patch: Option<Vec<u8>>,
    next_patch: Vec<u8>,
    previous_payload: String,
    next_payload: String,
}

impl ControlledConfigUpdate {
    /// Effective YAML before this mutation.
    #[must_use]
    pub fn previous_payload(&self) -> &str {
        &self.previous_payload
    }

    /// Effective YAML including the proposed mutation.
    #[must_use]
    pub fn next_payload(&self) -> &str {
        &self.next_payload
    }
}

/// Atomic store for a global controlled-config YAML layer.
///
/// Subscription and local source files remain untouched. The stored mapping is
/// recursively merged over whichever profile is active.
#[derive(Clone, Debug)]
pub struct ControlledConfigStore {
    startup_tun_disabled: bool,
    runtime_tun_policy: Option<std::sync::Weak<crate::client::ControllerBinding>>,
    root: PathBuf,
    write_access: DataWriteAccess,
    transaction: Arc<Mutex<()>>,
    mutation_gate: Arc<tokio::sync::Mutex<()>>,
    session_listener_fallbacks: Arc<Mutex<BTreeMap<String, SessionListenerFallback>>>,
    #[cfg(test)]
    commit_gate: Option<Arc<CommitGate>>,
    #[cfg(test)]
    mode_patch_gate: Option<Arc<ModePatchGate>>,
}

impl ControlledConfigStore {
    /// Opens the platform-default `ZenClash` controlled-config directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the application data directory cannot be found.
    pub fn discover() -> ControlledConfigResult<Self> {
        Ok(Self::new(default_data_dir()?.join("controlled-config")))
    }

    /// Creates a store rooted at an explicit directory.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            startup_tun_disabled: false,
            runtime_tun_policy: None,
            write_access: DataWriteAccess::new(&root),
            transaction: shared_transaction(&root),
            root,
            mutation_gate: Arc::new(tokio::sync::Mutex::new(())),
            session_listener_fallbacks: Arc::new(Mutex::new(BTreeMap::new())),
            #[cfg(test)]
            commit_gate: None,
            #[cfg(test)]
            mode_patch_gate: None,
        }
    }

    /// Creates a startup-only projection for an unprivileged ordinary child.
    /// Saved TUN intent is retained for a later privileged service handover.
    #[must_use]
    pub fn without_startup_tun(&self) -> Self {
        Self {
            startup_tun_disabled: true,
            ..self.clone()
        }
    }

    /// Projects ordinary Mihomo updates with TUN off until a privileged owner is published.
    /// A weak binding observation prevents configuration stores retaining old owners.
    #[must_use]
    pub fn with_runtime_tun_policy(&self, client: &MihomoClient) -> Self {
        Self {
            runtime_tun_policy: Some(Arc::downgrade(&client.binding)),
            ..self.clone()
        }
    }

    pub(crate) fn for_service_runtime(&self) -> Self {
        Self {
            startup_tun_disabled: false,
            runtime_tun_policy: None,
            ..self.clone()
        }
    }

    fn startup_payload(&self, payload: String) -> ControlledConfigResult<String> {
        let ordinary = self
            .runtime_tun_policy
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .is_some_and(|binding| {
                let runtime = binding.descriptor();
                runtime.kind() == CoreKind::Mihomo
                    && runtime.backend() == crate::CoreRuntimeBackend::Local
                    && !zenclash_service::current_process_elevated()
            });
        if !self.startup_tun_disabled && !ordinary {
            return Ok(payload);
        }
        Ok(crate::profile::merge_payload_patch(
            &payload,
            serde_yaml::from_str("tun: {enable: false}")?,
        )?)
    }

    pub(crate) async fn acquire_write_lease(&self) -> ControlledConfigResult<DataWriteLease> {
        self.write_access
            .acquire_async()
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))
    }

    pub(crate) async fn acquire_write_lease_for_paths(
        &self,
        paths: Vec<PathBuf>,
    ) -> ControlledConfigResult<DataWriteLease> {
        if paths.is_empty() {
            return self.acquire_write_lease().await;
        }
        self.write_access
            .acquire_paths_async(paths)
            .await
            .map_err(ControlledConfigError::Task)
    }

    pub(crate) fn with_write_lease(&self, lease: &DataWriteLease) -> Self {
        Self {
            write_access: self.write_access.authorized(lease),
            ..self.clone()
        }
    }

    /// Loads the current YAML override mapping.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable, oversized, malformed, or non-mapping
    /// content.
    pub fn load(&self) -> ControlledConfigResult<Value> {
        let _transaction = self.transaction.lock();
        self.load_unlocked().map(|(_, value)| value)
    }

    /// Loads the current override as JSON for native UI state.
    ///
    /// # Errors
    ///
    /// Returns an error when the YAML layer is invalid or cannot be represented
    /// as a JSON object.
    pub fn load_json(&self) -> ControlledConfigResult<serde_json::Value> {
        Ok(serde_json::to_value(self.load()?)?)
    }

    /// Builds effective YAML for `profile` without modifying its source file.
    ///
    /// # Errors
    ///
    /// Returns an error when the controlled layer or source profile cannot be
    /// read, parsed, merged, validated, or serialized.
    pub fn effective_payload(&self, profile: impl AsRef<Path>) -> ControlledConfigResult<String> {
        let patch = self.load()?;
        self.startup_payload(merge_profile_patch(profile.as_ref(), patch)?)
    }

    /// Reads the untouched UTF-8 source profile for diagnostics and previews.
    ///
    /// # Errors
    ///
    /// Returns an error when the profile is unreadable, oversized, or not
    /// UTF-8.
    pub fn source_payload(&self, profile: impl AsRef<Path>) -> ControlledConfigResult<String> {
        let path = profile.as_ref();
        let bytes = read_profile_bytes(path).map_err(|error| match error {
            crate::ProfileStoreError::Io(error) => ControlledConfigError::Io(error),
            other => ControlledConfigError::Transaction(other.to_string()),
        })?;
        String::from_utf8(bytes).map_err(|error| {
            ControlledConfigError::Transaction(format!(
                "基础配置 {} 不是 UTF-8：{error}",
                path.display()
            ))
        })
    }

    /// Builds the effective profile as JSON for native configuration forms.
    ///
    /// # Errors
    ///
    /// Returns an error when the profile cannot be merged or represented as
    /// JSON.
    pub fn effective_json(
        &self,
        profile: impl AsRef<Path>,
    ) -> ControlledConfigResult<serde_json::Value> {
        let payload = self.effective_payload(profile)?;
        let yaml = serde_yaml::from_str::<Value>(&payload)?;
        Ok(serde_json::to_value(yaml)?)
    }

    /// Builds the final YAML after applying the controlled layer and ordered
    /// explicit override files.
    ///
    /// # Errors
    ///
    /// Returns an error when any source cannot be read, merged, validated, or
    /// serialized.
    pub fn effective_with_overrides(
        &self,
        profile: impl AsRef<Path>,
        overrides: &[PathBuf],
    ) -> ControlledConfigResult<String> {
        let payload = self.effective_payload(profile)?;
        self.startup_payload(merge_payload_overrides(&payload, overrides)?)
    }

    fn effective_source_with_overrides(
        &self,
        source: crate::profile::ProfileRuntimeSource,
        overrides: &[PathBuf],
    ) -> ControlledConfigResult<String> {
        let payload = match source {
            crate::profile::ProfileRuntimeSource::File(path) => self.effective_payload(path)?,
            crate::profile::ProfileRuntimeSource::Frozen(payload) => {
                crate::profile::merge_payload_patch(&payload, self.load()?)?
            }
            crate::profile::ProfileRuntimeSource::Prepared(candidate) => {
                let _transaction = self.transaction.lock();
                let (update, expected_cache) = *candidate;
                if self.current_patch_bytes_unlocked()? != update.expected_patch
                    || self.cached_runtime_payload()? != expected_cache
                {
                    return Err(ControlledConfigError::ConcurrentModification);
                }
                // Ordered overrides are already part of this checked final payload.
                return Ok(update.next_payload);
            }
        };
        self.startup_payload(merge_payload_overrides(&payload, overrides)?)
    }

    /// Builds the final effective profile as JSON after ordered YAML overrides.
    ///
    /// # Errors
    ///
    /// Returns an error when merging, validation, or JSON conversion fails.
    pub fn effective_json_with_overrides(
        &self,
        profile: impl AsRef<Path>,
        overrides: &[PathBuf],
    ) -> ControlledConfigResult<serde_json::Value> {
        let payload = self.effective_with_overrides(profile, overrides)?;
        let yaml = serde_yaml::from_str::<Value>(&payload)?;
        Ok(serde_json::to_value(yaml)?)
    }

    /// Materializes effective YAML for managed-core startup.
    ///
    /// The returned file is a generated cache. The source profile and
    /// controlled override remain authoritative and are never overwritten.
    ///
    /// # Errors
    ///
    /// Returns an error when merging or the atomic cache write fails.
    pub fn materialize(&self, profile: impl AsRef<Path>) -> ControlledConfigResult<PathBuf> {
        let _write_lease = self.write_access.acquire();
        let payload = self.effective_payload(profile)?;
        self.write_runtime_payload(&payload)
    }

    /// Materializes startup YAML with ordered managed overrides applied.
    ///
    /// # Errors
    ///
    /// Returns an error when merging, validation, or the atomic cache write fails.
    pub fn materialize_with_overrides(
        &self,
        profile: impl AsRef<Path>,
        overrides: &[PathBuf],
    ) -> ControlledConfigResult<PathBuf> {
        let _write_lease = self.write_access.acquire();
        let payload = self.effective_with_overrides(profile, overrides)?;
        self.write_runtime_payload(&payload)
    }

    /// Materializes startup YAML with compatibility defaults for one core.
    ///
    /// meow-rs treats `dns.enable: true` with no configured upstream as an
    /// enabled but empty resolver. Mihomo supplies an internal fallback for
    /// the same profile. To keep subscriptions portable, generated meow-rs
    /// runtime YAML receives conservative IP-literal DNS defaults only when
    /// `nameserver` is absent or empty. An explicit `default-nameserver` is
    /// retained; source profiles and the persistent controlled override remain
    /// untouched.
    ///
    /// # Errors
    ///
    /// Returns an error when merging, normalization, or the atomic cache write fails.
    pub fn materialize_with_overrides_for_core(
        &self,
        profile: impl AsRef<Path>,
        overrides: &[PathBuf],
        kind: CoreKind,
    ) -> ControlledConfigResult<PathBuf> {
        let _write_lease = self.write_access.acquire();
        let payload = self.effective_with_overrides(profile, overrides)?;
        let payload = normalize_runtime_payload(kind, payload)?;
        self.write_runtime_payload(&payload)
    }

    fn write_runtime_payload(&self, payload: &str) -> ControlledConfigResult<PathBuf> {
        let path = self.runtime_path();
        let payload = self.apply_session_listener_fallbacks(payload)?;
        atomic_write(&path, payload.as_bytes())?;
        Ok(path)
    }

    /// Detects externally occupied proxy listeners and moves only the generated
    /// runtime configuration to available ports for this process session.
    /// Source profiles and persistent controlled overrides are never changed.
    /// Later runtime-cache generations retain the fallback unless the user
    /// explicitly chooses another port.
    ///
    /// # Errors
    ///
    /// Returns an error when the generated runtime file cannot be read, parsed,
    /// probed, serialized, or atomically replaced.
    pub fn resolve_startup_listener_conflicts(
        &self,
    ) -> ControlledConfigResult<Vec<ListenerPortFallback>> {
        let _write_lease = self.write_access.acquire();
        let path = self.runtime_path();
        let bytes = read_profile_bytes(&path).map_err(|error| match error {
            crate::ProfileStoreError::Io(error) => ControlledConfigError::Io(error),
            other => ControlledConfigError::ListenerFallback(other.to_string()),
        })?;
        let payload = String::from_utf8(bytes).map_err(|error| {
            ControlledConfigError::ListenerFallback(format!("生成的运行配置不是 UTF-8：{error}"))
        })?;
        let mut document = serde_yaml::from_str::<Value>(&payload)?;
        let mut next_session = self.session_listener_fallbacks.lock().clone();
        let resolved = resolve_conflicts(&mut document, &mut next_session)
            .map_err(ControlledConfigError::ListenerFallback)?;
        if resolved.is_empty() {
            return Ok(Vec::new());
        }
        let payload = serde_yaml::to_string(&document)?;
        if payload.len() > MAX_PROFILE_BYTES {
            return Err(ControlledConfigError::TooLarge);
        }
        atomic_write(&path, payload.as_bytes())?;
        *self.session_listener_fallbacks.lock() = next_session;
        Ok(resolved
            .into_iter()
            .map(|(listener, fallback)| ListenerPortFallback {
                listener,
                original: fallback.original,
                current: fallback.current,
            })
            .collect())
    }

    fn apply_session_listener_fallbacks(&self, payload: &str) -> ControlledConfigResult<String> {
        let payload = self.startup_payload(payload.to_owned())?;
        let session = self.session_listener_fallbacks.lock();
        let payload = apply_session_fallbacks(&payload, &session)
            .map_err(ControlledConfigError::ListenerFallback)?;
        if payload.len() > MAX_PROFILE_BYTES {
            return Err(ControlledConfigError::TooLarge);
        }
        Ok(payload)
    }

    /// Prepares a recursive JSON patch without persisting it.
    ///
    /// # Errors
    ///
    /// Returns an error for non-object input or when either the current or next
    /// effective profile cannot be generated and validated.
    pub fn prepare_json_update(
        &self,
        profile: impl AsRef<Path>,
        patch: &serde_json::Value,
    ) -> ControlledConfigResult<ControlledConfigUpdate> {
        if !patch.is_object() {
            return Err(ControlledConfigError::NotMapping);
        }
        let patch = serde_json::from_value::<Value>(patch.clone())?;
        self.prepare_update(profile.as_ref(), patch)
    }

    /// Applies, verifies, and persists a JSON override through real Mihomo.
    ///
    /// The startup cache is staged before the complete effective YAML is sent
    /// to Mihomo. The override and cache are committed together only after
    /// Mihomo accepts the payload; failures restore both runtime and cache.
    ///
    /// # Errors
    ///
    /// Returns preparation, Mihomo reload, commit, rollback, or task errors.
    pub async fn apply_json_update(
        &self,
        client: &MihomoClient,
        profile: impl AsRef<Path>,
        patch: &serde_json::Value,
    ) -> ControlledConfigResult<()> {
        self.apply_json_update_with_overrides(client, profile, patch, Vec::new())
            .await
    }

    /// Applies and persists a JSON patch while preserving ordered YAML overrides.
    ///
    /// The candidate includes the current explicit override chain. On failure,
    /// rollback replays the cached payload from the previous application.
    ///
    /// # Errors
    ///
    /// Returns preparation, merge, Mihomo reload, commit, rollback, or task errors.
    pub async fn apply_json_update_with_overrides(
        &self,
        client: &MihomoClient,
        profile: impl AsRef<Path>,
        patch: &serde_json::Value,
        overrides: Vec<PathBuf>,
    ) -> ControlledConfigResult<()> {
        self.apply_json_update_for_session(client, profile, patch, overrides)
            .await
            .map_err(|error| error.cause)?
            .confirmation
    }

    pub(crate) async fn apply_json_update_for_session(
        &self,
        client: &MihomoClient,
        profile: impl AsRef<Path>,
        patch: &serde_json::Value,
        overrides: Vec<PathBuf>,
    ) -> Result<SavedRuntimeReceipt, RuntimeMutationError> {
        if client.service_client().is_some() && service_partial_patch(patch) {
            return self
                .apply_partial_update_for_session(client, profile, patch, overrides)
                .await;
        }
        let client = client.pin_binding()?;
        let write_lease = self
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        client.ensure_binding_current()?;
        let leased_client = client.with_write_lease(&write_lease)?;
        let client = &leased_client;
        let leased_store = self.with_write_lease(&write_lease);
        let mutation_guard = leased_store.mutation_gate.clone().lock_owned().await;
        client.ensure_binding_current()?;
        let prepare_lease = leased_store.write_access.acquire();
        let prepare_store = leased_store.with_write_lease(&prepare_lease);
        let profile = profile.as_ref().to_path_buf();
        let patch = patch.clone();
        let (update, applied_payload) = tokio::task::spawn_blocking(move || {
            let _write_lease = prepare_lease;
            let applied_payload = prepare_store.cached_runtime_payload()?;
            Ok::<_, ControlledConfigError>((
                prepare_store.prepare_json_update(profile, &patch)?,
                applied_payload,
            ))
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let next_base = update.next_payload().to_owned();
        let previous_base = update.previous_payload().to_owned();
        let (next_runtime, previous_runtime) = tokio::task::spawn_blocking(move || {
            Ok::<_, MihomoError>((
                merge_payload_overrides(&next_base, &overrides)?,
                merge_payload_overrides(&previous_base, &overrides)?,
            ))
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let previous_runtime = leased_store.apply_session_listener_fallbacks(&previous_runtime)?;
        let next_runtime = leased_store.apply_session_listener_fallbacks(&next_runtime)?;
        let previous_runtime = applied_payload.as_deref().unwrap_or(&previous_runtime);
        validate_listener_change(previous_runtime, &next_runtime, true)
            .map_err(ControlledConfigError::ListenerFallback)?;
        let cache = leased_store
            .accept_runtime_payload_for_session(client, next_runtime)
            .await?;
        // One admitted task retains the write lease, store gate and runtime token to completion.
        tokio::spawn(async move {
            let _write_lease = write_lease;
            let _mutation_guard = mutation_guard;
            let commit_lease = leased_store.write_access.acquire();
            let commit_store = leased_store.with_write_lease(&commit_lease);
            let commit_update = update.clone();
            let commit = tokio::task::spawn_blocking(move || {
                let _write_lease = commit_lease;
                commit_store.commit(&commit_update)
            })
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))
            .and_then(|result| result);
            if let Err(error) = commit {
                return match cache.rollback().await {
                    Ok(()) => Err(ControlledConfigError::Transaction(format!(
                        "保存失败，启动缓存与 Mihomo 均已恢复上一版本：{error}"
                    ))),
                    Err(rollback) => Err(ControlledConfigError::Transaction(format!(
                        "保存失败：{error}；上一版本恢复：{rollback}"
                    ))),
                }
                .map_err(RuntimeMutationError::attempted);
            }
            Ok(SavedRuntimeReceipt {
                delta: None,
                confirmation: cache.commit().await,
            })
        })
        .await
        .map_err(|error| {
            RuntimeMutationError::attempted(ControlledConfigError::Task(error.to_string()))
        })?
    }

    /// Changes the live outbound mode without reloading unrelated listeners,
    /// then persists the selection for the next managed-core startup.
    ///
    /// A mode switch is a partial Mihomo runtime mutation. Reloading the whole
    /// profile here can fail because of an unrelated listener, DNS, or TUN
    /// setting and makes a simple routing change unnecessarily disruptive.
    /// The generated startup cache is still updated with explicit overrides
    /// applied so the next launch uses the same effective configuration.
    ///
    /// # Errors
    ///
    /// Returns preparation, merge, runtime patch/readback, persistence, cache,
    /// or rollback errors.
    pub async fn apply_mode_update_with_overrides(
        &self,
        client: &MihomoClient,
        profile: impl AsRef<Path>,
        mode: &str,
        overrides: Vec<PathBuf>,
    ) -> ControlledConfigResult<()> {
        self.apply_mode_update_for_session(client, profile, mode, overrides)
            .await
            .map_err(|error| error.cause)?
            .confirmation
    }

    pub(crate) async fn apply_mode_update_for_session(
        &self,
        client: &MihomoClient,
        profile: impl AsRef<Path>,
        mode: &str,
        overrides: Vec<PathBuf>,
    ) -> Result<SavedRuntimeReceipt, RuntimeMutationError> {
        let mode = mode.trim().to_ascii_lowercase();
        if !matches!(mode.as_str(), "rule" | "global" | "direct") {
            return Err(MihomoError::InvalidInput(format!("不支持的出站模式：{mode}")).into());
        }
        self.apply_partial_update_for_session(
            client,
            profile,
            &serde_json::json!({"mode":mode}),
            overrides,
        )
        .await
    }

    async fn apply_partial_update_for_session(
        &self,
        client: &MihomoClient,
        profile: impl AsRef<Path>,
        patch: &serde_json::Value,
        overrides: Vec<PathBuf>,
    ) -> Result<SavedRuntimeReceipt, RuntimeMutationError> {
        let client = client.pin_binding()?;
        let lease = self
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        client.ensure_binding_current()?;
        let client = client.with_write_lease(&lease)?;
        let store = self.with_write_lease(&lease);
        let guard = store.mutation_gate.clone().lock_owned().await;
        client.ensure_binding_current()?;
        let profile = profile.as_ref().to_path_buf();
        let patch = patch.clone();
        // An admitted owner retains data authority through persistence and finalization.
        tokio::spawn(async move {
            let _lease = lease;
            let _guard = guard;
            store
                .apply_partial_admitted(client, profile, patch, overrides)
                .await
        })
        .await
        .map_err(|error| {
            RuntimeMutationError::attempted(ControlledConfigError::Task(error.to_string()))
        })?
    }

    async fn apply_partial_admitted(
        &self,
        client: MihomoClient,
        profile: PathBuf,
        patch: serde_json::Value,
        overrides: Vec<PathBuf>,
    ) -> Result<SavedRuntimeReceipt, RuntimeMutationError> {
        let worker_lease = self.write_access.acquire();
        let worker = self.with_write_lease(&worker_lease);
        let requested = patch.clone();
        let fallback_bundle = client.service_runtime_snapshot()?;
        let mut update = tokio::task::spawn_blocking(move || {
            let _lease = worker_lease;
            worker.prepare_partial_update(
                &profile,
                &requested,
                &overrides,
                fallback_bundle.as_deref(),
            )
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let previous_payload = update.previous_payload.clone();
        let prepared = if client.service_client().is_some() {
            Some(client.prepare_runtime_patch(&patch).await?)
        } else {
            None
        };
        let delta = prepared
            .as_ref()
            .and_then(|prepared| prepared.effective_delta())
            .unwrap_or(patch);
        // Persist the effective delta too: implicit TUN enable and DNS normalization
        // must survive later full applications of the controlled layer.
        update.next_patch = merge_held_delta(
            std::str::from_utf8(&update.next_patch)
                .map_err(|_| ControlledConfigError::NotMapping)?,
            &delta,
        )?
        .into_bytes();
        let next_runtime = merge_held_delta(&previous_payload, &delta)?;
        let worker_lease = self.write_access.acquire();
        let worker = self.with_write_lease(&worker_lease);
        let cache = tokio::task::spawn_blocking(move || {
            let _lease = worker_lease;
            worker.stage_runtime_payload(&next_runtime)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let (runtime, previous_mode) = if let Some(prepared) = prepared {
            match prepared.apply(false).await {
                Ok(runtime) => (Some(runtime), None),
                Err(error) => {
                    let attempted = error.mutation_result_unknown();
                    let cause = match rollback_runtime_cache(cache).await {
                        Ok(()) => ControlledConfigError::Profile(error),
                        Err(rollback) => ControlledConfigError::Transaction(format!(
                            "部分更新失败：{error}；缓存恢复：{rollback}"
                        )),
                    };
                    return Err(RuntimeMutationError { cause, attempted });
                }
            }
        } else {
            let previous_mode = match client.runtime_config().await {
                Ok(config) => config.mode,
                Err(error) => {
                    rollback_runtime_cache(cache).await?;
                    return Err(error.into());
                }
            };
            #[cfg(test)]
            if let Some(gate) = &self.mode_patch_gate {
                gate.wait().await;
            }
            if let Err(error) = client.patch_configs_verified(&delta).await {
                if matches!(error, MihomoError::StaleBinding) {
                    rollback_runtime_cache(cache).await?;
                    return Err(error.into());
                }
                let cache_result = rollback_runtime_cache(cache).await;
                let runtime_result = client.set_mode(&previous_mode).await;
                return Err(RuntimeMutationError::attempted(
                    ControlledConfigError::Transaction(format!(
                        "部分更新失败：{error}；缓存恢复：{}；内核恢复：{}",
                        result_label(cache_result),
                        result_label(runtime_result)
                    )),
                ));
            }
            (None, Some(previous_mode))
        };
        let worker_lease = self.write_access.acquire();
        let worker = self.with_write_lease(&worker_lease);
        let commit = tokio::task::spawn_blocking(move || {
            let _lease = worker_lease;
            worker.commit(&update)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))
        .and_then(|result| result);
        if let Err(error) = commit {
            let cache_result = rollback_runtime_cache(cache).await;
            let runtime_result = if let Some(runtime) = runtime {
                runtime.rollback(Some(previous_payload)).await
            } else {
                client
                    .set_mode(previous_mode.as_deref().ok_or_else(|| {
                        ControlledConfigError::Transaction("缺少上一模式快照".into())
                    })?)
                    .await
            };
            return Err(RuntimeMutationError::attempted(
                ControlledConfigError::Transaction(format!(
                    "保存部分配置失败：{error}；缓存恢复：{}；内核恢复：{}",
                    result_label(cache_result),
                    result_label(runtime_result)
                )),
            ));
        }
        let confirmation = if let Some(runtime) = runtime {
            cache.finalize(runtime.commit()).await
        } else {
            cache.commit();
            Ok(())
        };
        Ok(SavedRuntimeReceipt {
            delta: Some(delta),
            confirmation,
        })
    }

    pub(crate) async fn lock_service_tun_mutation(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.mutation_gate.clone().lock_owned().await
    }

    pub(crate) fn owns_service_tun_mutation(
        &self,
        guard: &tokio::sync::OwnedMutexGuard<()>,
    ) -> bool {
        Arc::ptr_eq(
            tokio::sync::OwnedMutexGuard::mutex(guard),
            &self.mutation_gate,
        )
    }

    pub(crate) async fn prepare_service_tun_update(
        &self,
        profile: PathBuf,
        overrides: Vec<PathBuf>,
        bundle: Option<Arc<crate::ServiceRuntimeBundle>>,
    ) -> ControlledConfigResult<ControlledConfigUpdate> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            if store.cached_runtime_payload()?.is_none() && bundle.is_none() {
                return Err(ControlledConfigError::Transaction(zenclash_i18n::text(
                    "core_page.service.no_snapshot",
                )));
            }
            store.prepare_partial_update(
                &profile,
                &serde_json::json!({"tun":{"enable":true},"dns":{"enable":true}}),
                &overrides,
                bundle.as_deref(),
            )
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    pub(crate) async fn validate_prepared_service_tun(
        &self,
        update: ControlledConfigUpdate,
        expected_cache: Option<String>,
    ) -> ControlledConfigResult<()> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let _transaction = store.transaction.lock();
            if store.current_patch_bytes_unlocked()? != update.expected_patch
                || store.cached_runtime_payload()? != expected_cache
            {
                return Err(ControlledConfigError::ConcurrentModification);
            }
            Ok(())
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    // The session owns completion and its transition gate; this method keeps
    // persistence, restart fallback and rollback on the same checked payload.
    pub(crate) async fn apply_frozen_local_update(
        &self,
        client: &MihomoClient,
        update: ControlledConfigUpdate,
        expected_cache: Option<String>,
        persist_patch: bool,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(crate::CoreApplyKind, SavedRuntimeReceipt), RuntimeMutationError> {
        let lease = self
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        let store = self.with_write_lease(&lease);
        let client = client.with_write_lease(&lease)?;
        let mutation = store.mutation_gate.clone().lock_owned().await;
        client.ensure_binding_current()?;
        store
            .validate_prepared_service_tun(update.clone(), expected_cache)
            .await?;
        let payload = update.next_payload().to_owned();
        let (cache, recovery, kind) = match store
            .accept_runtime_payload_for_session(&client, payload.clone())
            .await
        {
            Ok(accepted) => (
                accepted.cache,
                RuntimeApplicationRecovery::HotReload {
                    runtime: Box::new(accepted.runtime),
                    previous_payload: accepted.previous_payload,
                },
                crate::CoreApplyKind::HotReloaded,
            ),
            Err(error) if crate::core_session::should_restart_after_hot_reload(&error.cause) => {
                let Some(crate::owned_core::OwnedCore::Local(process)) = client.owned_core() else {
                    return Err(error);
                };
                // Restart reads the owner's launch path. Never fall back to a
                // mutable source file after admitting this frozen configuration.
                if process.launch_config().config_file != store.runtime_path() {
                    return Err(error);
                }
                let cache = store
                    .accept_runtime_payload_with_restart_for_session(
                        process.clone(),
                        payload,
                        Some(cancelled.clone()),
                    )
                    .await
                    .map_err(|mut restart| {
                        restart.attempted |= error.attempted;
                        restart
                    })?;
                (
                    cache,
                    RuntimeApplicationRecovery::Restart {
                        process,
                        cancelled: Some(cancelled),
                    },
                    crate::CoreApplyKind::Restarted,
                )
            }
            Err(error) => return Err(error),
        };
        let transaction = RuntimeApplicationTransaction {
            cache,
            recovery,
            _mutation_guard: mutation,
            _write_lease: lease,
        };
        if persist_patch {
            let commit_lease = store.write_access.acquire();
            let commit_store = store.with_write_lease(&commit_lease);
            let commit = tokio::task::spawn_blocking(move || {
                let _lease = commit_lease;
                commit_store.commit(&update)
            })
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))
            .and_then(|result| result);
            if let Err(error) = commit {
                let rollback = transaction.rollback().await;
                return Err(RuntimeMutationError::attempted(
                    ControlledConfigError::Transaction(format!(
                        "保存失败：{error}；上一版本恢复：{}",
                        result_label(rollback)
                    )),
                ));
            }
        }
        Ok((
            kind,
            SavedRuntimeReceipt {
                delta: None,
                confirmation: transaction.commit().await,
            },
        ))
    }

    pub(crate) async fn prepare_service_config_update(
        &self,
        profile: impl Into<crate::profile::ProfileRuntimeSource> + Send,
        patch: Option<serde_json::Value>,
        overrides: Vec<PathBuf>,
        previous_payload: Option<String>,
    ) -> ControlledConfigResult<ControlledConfigUpdate> {
        let profile = profile.into();
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut update =
                if let (crate::profile::ProfileRuntimeSource::File(path), Some(patch)) =
                    (&profile, &patch)
                {
                    store.prepare_update(path, serde_yaml::to_value(patch)?)?
                } else {
                    if patch.is_some() {
                        return Err(ControlledConfigError::PartialOverrideConflict);
                    }
                    let _transaction = store.transaction.lock();
                    let (expected_patch, current) = store.load_unlocked()?;
                    let payload = match profile {
                        crate::profile::ProfileRuntimeSource::File(path) => {
                            merge_profile_patch(&path, current.clone())?
                        }
                        crate::profile::ProfileRuntimeSource::Frozen(payload) => {
                            crate::profile::merge_payload_patch(&payload, current.clone())?
                        }
                        crate::profile::ProfileRuntimeSource::Prepared(_) => {
                            return Err(ControlledConfigError::PartialOverrideConflict);
                        }
                    };
                    ControlledConfigUpdate {
                        expected_patch,
                        next_patch: serde_yaml::to_string(&current)?.into_bytes(),
                        previous_payload: payload.clone(),
                        next_payload: payload,
                    }
                };
            update.next_payload =
                store.apply_session_listener_fallbacks(&normalize_runtime_payload(
                    CoreKind::Mihomo,
                    merge_payload_overrides(update.next_payload(), &overrides)?,
                )?)?;
            if let Some(payload) = previous_payload {
                update.previous_payload = payload;
            }
            validate_listener_change(update.previous_payload(), update.next_payload(), true)
                .map_err(ControlledConfigError::ListenerFallback)?;
            Ok(update)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    pub(crate) fn prepare_backup_config_update(
        &self,
        candidate: &crate::backup::BackupRuntimeCandidate,
        previous_payload: String,
    ) -> ControlledConfigResult<ControlledConfigUpdate> {
        let next_payload = self.apply_session_listener_fallbacks(&normalize_runtime_payload(
            CoreKind::Mihomo,
            candidate.payload.clone(),
        )?)?;
        validate_listener_change(&previous_payload, &next_payload, true)
            .map_err(ControlledConfigError::ListenerFallback)?;
        Ok(ControlledConfigUpdate {
            expected_patch: Some(candidate.patch.clone()),
            next_patch: candidate.patch.clone(),
            previous_payload,
            next_payload,
        })
    }

    pub(crate) async fn prepare_service_tun_recovery_update(
        &self,
        bundle: Arc<crate::ServiceRuntimeBundle>,
    ) -> ControlledConfigResult<ControlledConfigUpdate> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let _transaction = store.transaction.lock();
            let (expected_patch, current) = store.load_unlocked()?;
            Self::prepare_partial_payload_update(
                expected_patch,
                current,
                bundle.yaml().to_owned(),
                &serde_json::json!({"tun":{"enable":true},"dns":{"enable":true}}),
                &[],
            )
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    // Caller holds the store mutation gate; borrowed write authority must remain live.
    pub(crate) async fn persist_local_recovery_payload_admitted(
        &self,
        payload: String,
    ) -> ControlledConfigResult<()> {
        let lease = self
            .write_access
            .borrowed_authority(std::slice::from_ref(&self.root))
            .map_err(ControlledConfigError::Task)?
            .ok_or_else(|| {
                ControlledConfigError::Task("Local recovery write authority has expired".into())
            })?;
        let store = self.with_write_lease(&lease);
        tokio::spawn(async move {
            let _lease = lease;
            let prepare = store.clone();
            let update = tokio::task::spawn_blocking(move || {
                if payload.len() > MAX_PROFILE_BYTES {
                    return Err(ControlledConfigError::TooLarge);
                }
                let document: Value = serde_yaml::from_str(&payload)?;
                if document
                    .get("tun")
                    .and_then(|tun| tun.get("enable"))
                    .and_then(Value::as_bool)
                    != Some(false)
                {
                    return Err(ControlledConfigError::PartialOverrideConflict);
                }
                let _transaction = prepare.transaction.lock();
                let (expected, current) = prepare.load_unlocked()?;
                Self::prepare_partial_payload_update(
                    expected,
                    current,
                    payload,
                    &serde_json::json!({"tun":{"enable":false}}),
                    &[],
                )
            })
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
            let persistence = store.stage_service_tun_update(update).await?;
            if let Err(error) = persistence.save().await {
                return match persistence.rollback().await {
                    Ok(()) => Err(error),
                    Err(rollback) => Err(ControlledConfigError::Transaction(format!(
                        "{error}; {rollback}"
                    ))),
                };
            }
            persistence.saved();
            Ok(())
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    pub(crate) async fn prepare_service_startup_payload(
        &self,
        profile: PathBuf,
        overrides: Vec<PathBuf>,
    ) -> ControlledConfigResult<(String, Vec<ListenerPortFallback>)> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let payload = store.effective_with_overrides(profile, &overrides)?;
            let payload = normalize_runtime_payload(CoreKind::Mihomo, payload)?;
            let payload = store.apply_session_listener_fallbacks(&payload)?;
            let mut document = serde_yaml::from_str::<Value>(&payload)?;
            let mut next_session = store.session_listener_fallbacks.lock().clone();
            let resolved = resolve_conflicts(&mut document, &mut next_session)
                .map_err(ControlledConfigError::ListenerFallback)?;
            let payload = serde_yaml::to_string(&document)?;
            if payload.len() > MAX_PROFILE_BYTES {
                return Err(ControlledConfigError::TooLarge);
            }
            *store.session_listener_fallbacks.lock() = next_session;
            Ok((
                payload,
                resolved
                    .into_iter()
                    .map(|(listener, fallback)| ListenerPortFallback {
                        listener,
                        original: fallback.original,
                        current: fallback.current,
                    })
                    .collect(),
            ))
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    pub(crate) async fn stage_service_startup_payload(
        &self,
        payload: String,
    ) -> ControlledConfigResult<ServiceStartupPersistence> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            store
                .stage_runtime_payload(&payload)
                .map(ServiceStartupPersistence)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    pub(crate) async fn stage_service_tun_update(
        &self,
        update: ControlledConfigUpdate,
    ) -> ControlledConfigResult<ServiceTunPersistence> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let cache = store.stage_runtime_payload(&update.next_payload)?;
            Ok(ServiceTunPersistence {
                cache,
                store,
                update,
            })
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    fn prepare_partial_update(
        &self,
        profile: &Path,
        patch: &serde_json::Value,
        overrides: &[PathBuf],
        fallback_bundle: Option<&crate::ServiceRuntimeBundle>,
    ) -> ControlledConfigResult<ControlledConfigUpdate> {
        let _transaction = self.transaction.lock();
        let (expected_patch, current) = self.load_unlocked()?;
        // Held cache is authoritative; changed profile sources never enter an accepted delta.
        let previous_payload = match self.cached_runtime_payload()? {
            Some(payload) => payload,
            None if fallback_bundle.is_some() => fallback_bundle
                .ok_or(ControlledConfigError::NotMapping)?
                .yaml()
                .to_owned(),
            None => {
                merge_payload_overrides(&merge_profile_patch(profile, current.clone())?, overrides)?
            }
        };
        Self::prepare_partial_payload_update(
            expected_patch,
            current,
            previous_payload,
            patch,
            overrides,
        )
    }

    fn prepare_partial_payload_update(
        expected_patch: Option<Vec<u8>>,
        mut current: Value,
        previous_payload: String,
        patch: &serde_json::Value,
        overrides: &[PathBuf],
    ) -> ControlledConfigResult<ControlledConfigUpdate> {
        let next_payload = merge_held_delta(&previous_payload, patch)?;
        let overridden = merge_payload_overrides(&next_payload, overrides)?;
        let actual: serde_json::Value = serde_yaml::from_str(&overridden)?;
        if !contains_delta(&actual, patch) {
            if let Some(mode) = patch.get("mode").and_then(serde_json::Value::as_str) {
                return Err(ControlledConfigError::ModeOverrideConflict {
                    requested: mode.into(),
                    effective: actual.get("mode").map_or_else(
                        || "null".into(),
                        |value| {
                            value
                                .as_str()
                                .map_or_else(|| value.to_string(), str::to_owned)
                        },
                    ),
                });
            }
            return Err(ControlledConfigError::PartialOverrideConflict);
        }
        merge_yaml(&mut current, serde_yaml::to_value(patch)?);
        require_mapping(&current)?;
        let next_patch = serde_yaml::to_string(&current)?.into_bytes();
        if next_patch.len() > MAX_PROFILE_BYTES {
            return Err(ControlledConfigError::TooLarge);
        }
        Ok(ControlledConfigUpdate {
            expected_patch,
            next_patch,
            previous_payload,
            next_payload,
        })
    }

    /// Applies and persists a JSON patch by restarting a managed core with the
    /// newly materialized effective profile.
    ///
    /// This is the safe configuration path for meow-rs because its `/configs`
    /// endpoint currently accepts only a subset of a complete Clash profile.
    /// The previous cache and process are restored when validation, restart,
    /// readiness, or persistence fails.
    ///
    /// # Errors
    ///
    /// Returns preparation, merge, restart, readiness, commit, or rollback errors.
    pub async fn apply_json_update_with_restart(
        &self,
        process: Arc<MihomoProcess>,
        profile: impl AsRef<Path>,
        patch: &serde_json::Value,
        overrides: Vec<PathBuf>,
    ) -> ControlledConfigResult<()> {
        self.apply_json_update_with_restart_for_session(process, profile, patch, overrides, None)
            .await
            .map_err(|error| error.cause)
    }

    pub(crate) async fn apply_json_update_with_restart_for_session(
        &self,
        process: Arc<MihomoProcess>,
        profile: impl AsRef<Path>,
        patch: &serde_json::Value,
        overrides: Vec<PathBuf>,
        cancelled: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<(), RuntimeMutationError> {
        let write_lease = self
            .acquire_write_lease_for_paths(process.write_scopes())
            .await?;
        let leased_store = self.with_write_lease(&write_lease);
        let _mutation_guard = leased_store.mutation_gate.lock().await;
        let prepare_lease = leased_store.write_access.acquire();
        let prepare_store = leased_store.with_write_lease(&prepare_lease);
        let profile = profile.as_ref().to_path_buf();
        let patch = patch.clone();
        let (update, applied_payload) = tokio::task::spawn_blocking(move || {
            let _write_lease = prepare_lease;
            let applied_payload = prepare_store.cached_runtime_payload()?;
            Ok::<_, ControlledConfigError>((
                prepare_store.prepare_json_update(profile, &patch)?,
                applied_payload,
            ))
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let next_base = update.next_payload().to_owned();
        let previous_base = update.previous_payload().to_owned();
        let kind = process.kind();
        let (next_runtime, previous_runtime) = tokio::task::spawn_blocking(move || {
            Ok::<_, ControlledConfigError>((
                normalize_runtime_payload(kind, merge_payload_overrides(&next_base, &overrides)?)?,
                normalize_runtime_payload(
                    kind,
                    merge_payload_overrides(&previous_base, &overrides)?,
                )?,
            ))
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let next_runtime = leased_store.apply_session_listener_fallbacks(&next_runtime)?;
        let previous_runtime = leased_store.apply_session_listener_fallbacks(&previous_runtime)?;
        let previous_runtime = applied_payload.as_deref().unwrap_or(&previous_runtime);
        validate_listener_change(previous_runtime, &next_runtime, process.is_running())
            .map_err(ControlledConfigError::ListenerFallback)?;
        let cache = leased_store
            .accept_runtime_payload_with_restart_for_session(
                process.clone(),
                next_runtime,
                cancelled.clone(),
            )
            .await?;
        let commit_lease = leased_store.write_access.acquire();
        let commit_store = leased_store.with_write_lease(&commit_lease);
        let commit_update = update.clone();
        let commit = tokio::task::spawn_blocking(move || {
            let _write_lease = commit_lease;
            commit_store.commit(&commit_update)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))
        .and_then(|result| result);
        if let Err(error) = commit {
            let rollback =
                rollback_cache_and_restart(cache, process, &write_lease, cancelled).await;
            return Err(RuntimeMutationError::attempted(
                ControlledConfigError::Transaction(format!(
                    "保存失败：{error}；运行内核恢复：{rollback}"
                )),
            ));
        }
        cache.commit();
        Ok(())
    }

    /// Reloads one source profile with the persisted controlled layer applied.
    ///
    /// This operation shares the same asynchronous mutation gate as
    /// [`Self::apply_json_update`], preventing a Profile switch from racing a
    /// settings update.
    ///
    /// # Errors
    ///
    /// Returns merge, task, or Mihomo reload errors.
    pub async fn reload_profile(
        &self,
        client: &MihomoClient,
        profile: impl AsRef<Path>,
    ) -> ControlledConfigResult<()> {
        let client = client.pin_binding()?;
        let write_lease = self
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        client.ensure_binding_current()?;
        let leased_client = client.with_write_lease(&write_lease)?;
        let client = &leased_client;
        let leased_store = self.with_write_lease(&write_lease);
        let _mutation_guard = leased_store.mutation_gate.lock().await;
        client.ensure_binding_current()?;
        let store = leased_store.clone();
        let profile = profile.as_ref().to_path_buf();
        let payload = tokio::task::spawn_blocking(move || store.effective_payload(profile))
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let payload = leased_store.validate_candidate_listeners(payload, true)?;
        leased_store
            .accept_runtime_payload(client, payload)
            .await?
            .commit()
            .await?;
        Ok(())
    }

    /// Reloads a profile with both the controlled layer and ordered YAML
    /// override files applied.
    ///
    /// The controlled layer is merged first, followed by the explicit files
    /// in their supplied order. This shares the profile mutation gate so a
    /// settings save cannot race the reload.
    ///
    /// # Errors
    ///
    /// Returns merge, task, file-read, or Mihomo reload errors.
    pub async fn reload_with_overrides(
        &self,
        client: &MihomoClient,
        profile: impl AsRef<Path>,
        overrides: Vec<PathBuf>,
    ) -> ControlledConfigResult<()> {
        self.reload_with_overrides_for_session(client, profile, overrides)
            .await
            .map_err(|error| error.cause)
    }

    pub(crate) async fn reload_with_overrides_for_session(
        &self,
        client: &MihomoClient,
        profile: impl AsRef<Path>,
        overrides: Vec<PathBuf>,
    ) -> Result<(), RuntimeMutationError> {
        let client = client.pin_binding()?;
        let write_lease = self
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        client.ensure_binding_current()?;
        let leased_client = client.with_write_lease(&write_lease)?;
        let client = &leased_client;
        let leased_store = self.with_write_lease(&write_lease);
        let _mutation_guard = leased_store.mutation_gate.lock().await;
        client.ensure_binding_current()?;
        let worker_lease = leased_store.write_access.acquire();
        let store = leased_store.with_write_lease(&worker_lease);
        let profile = profile.as_ref().to_path_buf();
        let payload = tokio::task::spawn_blocking(move || {
            let _write_lease = worker_lease;
            store.effective_with_overrides(profile, &overrides)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let payload = leased_store.validate_candidate_listeners(payload, true)?;
        leased_store
            .accept_runtime_payload_for_session(client, payload)
            .await?
            .commit()
            .await
            .map_err(RuntimeMutationError::attempted)?;
        Ok(())
    }

    pub(crate) async fn stage_profile_reload(
        &self,
        client: &MihomoClient,
        candidate: impl Into<crate::profile::ProfileRuntimeSource> + Send,
        _previous: Option<PathBuf>,
        overrides: Vec<PathBuf>,
    ) -> Result<RuntimeApplicationTransaction, RuntimeMutationError> {
        self.stage_profile_runtime(client, candidate.into(), overrides, false, None)
            .await
            .map(|(transaction, _)| transaction)
    }

    pub(crate) async fn stage_profile_reload_with_restart(
        &self,
        client: &MihomoClient,
        candidate: crate::profile::ProfileRuntimeSource,
        overrides: Vec<PathBuf>,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(RuntimeApplicationTransaction, crate::CoreApplyKind), RuntimeMutationError> {
        self.stage_profile_runtime(client, candidate, overrides, true, Some(cancelled))
            .await
    }

    async fn stage_profile_runtime(
        &self,
        client: &MihomoClient,
        candidate: crate::profile::ProfileRuntimeSource,
        overrides: Vec<PathBuf>,
        allow_restart: bool,
        cancelled: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<(RuntimeApplicationTransaction, crate::CoreApplyKind), RuntimeMutationError> {
        let client = client.pin_binding()?;
        let write_lease = self
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        client.ensure_binding_current()?;
        let client = client.with_write_lease(&write_lease)?;
        let store = self.with_write_lease(&write_lease);
        let mutation_guard = store.mutation_gate.clone().lock_owned().await;
        client.ensure_binding_current()?;
        let worker_lease = store.write_access.acquire();
        let worker = store.with_write_lease(&worker_lease);
        let (payload, previous_payload) = tokio::task::spawn_blocking(move || {
            let _lease = worker_lease;
            let previous_payload = worker.cached_runtime_payload()?;
            Ok::<_, ControlledConfigError>((
                worker.effective_source_with_overrides(candidate, &overrides)?,
                previous_payload,
            ))
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let payload =
            store.validate_candidate_listeners(client.normalize_config_payload(payload)?, true)?;
        // Keep the candidate, mutation gate and write authority through fallback.
        // Re-merging a changed source or override can turn an admitted TUN-off
        // hot reload into an unauthorized TUN-on process restart.
        let (cache, recovery, kind) = match store
            .accept_runtime_payload_with_snapshot(&client, payload.clone(), previous_payload)
            .await
        {
            Ok(accepted) => (
                accepted.cache,
                RuntimeApplicationRecovery::HotReload {
                    runtime: Box::new(accepted.runtime),
                    previous_payload: accepted.previous_payload,
                },
                crate::CoreApplyKind::HotReloaded,
            ),
            Err(error)
                if allow_restart
                    && crate::core_session::should_restart_after_hot_reload(&error.cause) =>
            {
                let Some(crate::owned_core::OwnedCore::Local(process)) = client.owned_core() else {
                    return Err(error);
                };
                if process.launch_config().config_file != store.runtime_path() {
                    return Err(error);
                }
                let cache = store
                    .accept_runtime_payload_with_restart_for_session(
                        process.clone(),
                        payload,
                        cancelled.clone(),
                    )
                    .await
                    .map_err(|mut restart| {
                        restart.attempted |= error.attempted;
                        restart
                    })?;
                (
                    cache,
                    RuntimeApplicationRecovery::Restart { process, cancelled },
                    crate::CoreApplyKind::Restarted,
                )
            }
            Err(error) => return Err(error),
        };
        Ok((
            RuntimeApplicationTransaction {
                cache,
                recovery,
                _mutation_guard: mutation_guard,
                _write_lease: write_lease,
            },
            kind,
        ))
    }

    pub(crate) async fn stage_profile_validation(
        &self,
        kind: CoreKind,
        client: &MihomoClient,
        candidate: impl Into<crate::profile::ProfileRuntimeSource> + Send,
        overrides: Vec<PathBuf>,
    ) -> ControlledConfigResult<RuntimeCandidateValidation> {
        let candidate = candidate.into();
        let client = client.pin_binding()?;
        let write_lease = self
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        client.ensure_binding_current()?;
        let leased_client = client.with_write_lease(&write_lease)?;
        let client = &leased_client;
        let leased_store = self.with_write_lease(&write_lease);
        let mutation_guard = leased_store.mutation_gate.clone().lock_owned().await;
        client.ensure_binding_current()?;
        let worker_lease = leased_store.write_access.acquire();
        let store = leased_store.with_write_lease(&worker_lease);
        let payload = tokio::task::spawn_blocking(move || {
            let _write_lease = worker_lease;
            store.effective_source_with_overrides(candidate, &overrides)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let payload = normalize_runtime_payload(kind, payload)?;
        client.validate_config_payload(&payload).await?;
        Ok(RuntimeCandidateValidation {
            _mutation_guard: mutation_guard,
            _write_lease: write_lease,
        })
    }

    /// Materializes a complete effective profile and restarts a managed core.
    ///
    /// This provides real profile switching for cores whose hot-reload API is
    /// incomplete. A failed restart restores the prior runtime cache and
    /// attempts to bring the previous configuration back online.
    ///
    /// # Errors
    ///
    /// Returns merge, cache, process restart, readiness, or rollback errors.
    pub async fn restart_with_overrides(
        &self,
        process: Arc<MihomoProcess>,
        profile: impl AsRef<Path>,
        overrides: Vec<PathBuf>,
    ) -> ControlledConfigResult<()> {
        self.restart_with_overrides_for_session(process, profile, overrides, None)
            .await
            .map_err(|error| error.cause)
    }

    pub(crate) async fn restart_with_overrides_for_session(
        &self,
        process: Arc<MihomoProcess>,
        profile: impl AsRef<Path>,
        overrides: Vec<PathBuf>,
        cancelled: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<(), RuntimeMutationError> {
        let write_lease = self
            .acquire_write_lease_for_paths(process.write_scopes())
            .await?;
        let leased_store = self.with_write_lease(&write_lease);
        let _mutation_guard = leased_store.mutation_gate.lock().await;
        let worker_lease = leased_store.write_access.acquire();
        let store = leased_store.with_write_lease(&worker_lease);
        let profile = profile.as_ref().to_path_buf();
        let payload = tokio::task::spawn_blocking(move || {
            let _write_lease = worker_lease;
            store.effective_with_overrides(profile, &overrides)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let payload = normalize_runtime_payload(process.kind(), payload)?;
        let payload = leased_store.validate_candidate_listeners(payload, process.is_running())?;
        leased_store
            .accept_runtime_payload_with_restart_for_session(process, payload, cancelled)
            .await?
            .commit();
        Ok(())
    }

    pub(crate) async fn stage_profile_restart(
        &self,
        process: Arc<MihomoProcess>,
        candidate: impl Into<crate::profile::ProfileRuntimeSource> + Send,
        overrides: Vec<PathBuf>,
        cancelled: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<RuntimeApplicationTransaction, RuntimeMutationError> {
        let candidate = candidate.into();
        let write_lease = self
            .acquire_write_lease_for_paths(process.write_scopes())
            .await?;
        let leased_store = self.with_write_lease(&write_lease);
        let mutation_guard = leased_store.mutation_gate.clone().lock_owned().await;
        let worker_lease = leased_store.write_access.acquire();
        let store = leased_store.with_write_lease(&worker_lease);
        let payload = tokio::task::spawn_blocking(move || {
            let _write_lease = worker_lease;
            store.effective_source_with_overrides(candidate, &overrides)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let payload = normalize_runtime_payload(process.kind(), payload)?;
        let payload = leased_store.validate_candidate_listeners(payload, process.is_running())?;
        let cache = leased_store
            .accept_runtime_payload_with_restart_for_session(
                process.clone(),
                payload,
                cancelled.clone(),
            )
            .await?;
        Ok(RuntimeApplicationTransaction {
            cache,
            recovery: RuntimeApplicationRecovery::Restart { process, cancelled },
            _mutation_guard: mutation_guard,
            _write_lease: write_lease,
        })
    }

    /// Persists a prepared update if the controlled layer has not changed.
    ///
    /// # Errors
    ///
    /// Returns an error for concurrent modification or an atomic write failure.
    pub fn commit(&self, update: &ControlledConfigUpdate) -> ControlledConfigResult<()> {
        #[cfg(test)]
        if let Some(gate) = &self.commit_gate {
            gate.wait();
        }
        let _write_lease = self.write_access.acquire();
        let _transaction = self.transaction.lock();
        if self.current_patch_bytes_unlocked()? != update.expected_patch {
            return Err(ControlledConfigError::ConcurrentModification);
        }
        atomic_write(&self.patch_path(), &update.next_patch)?;
        Ok(())
    }

    /// Returns the generated runtime-cache path.
    #[must_use]
    pub fn runtime_path(&self) -> PathBuf {
        self.root.join("effective.yaml")
    }

    /// Fingerprints an effective source change not already represented by the runtime cache.
    ///
    /// Ignores formatting and comments, so application writes and atomic cache
    /// replacement cannot cause a restart loop. Run off the UI thread.
    ///
    /// # Errors
    /// Returns source, merge, size-limit or YAML errors without changing any files.
    pub fn pending_source_revision(
        &self,
        kind: CoreKind,
        profile: &Path,
        overrides: &[PathBuf],
    ) -> ControlledConfigResult<Option<[u8; 32]>> {
        use sha2::{Digest, Sha256};
        let candidate =
            normalize_runtime_payload(kind, self.effective_with_overrides(profile, overrides)?)?;
        let candidate = self.apply_session_listener_fallbacks(&candidate)?;
        let candidate: Value = serde_yaml::from_str(&candidate)?;
        if let Some(current) = self.cached_runtime_payload()? {
            let current: Value = serde_yaml::from_str(&current)?;
            if current == candidate {
                return Ok(None);
            }
        }
        Ok(Some(
            Sha256::digest(serde_yaml::to_string(&candidate)?.as_bytes()).into(),
        ))
    }

    /// Returns the controlled-config storage root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Quarantines a malformed persisted override and restores an empty layer.
    ///
    /// Only parse, shape, and defensive-size failures are recoverable. Native
    /// I/O errors are returned unchanged so permission and disk failures are
    /// never mistaken for successful repair.
    ///
    /// # Errors
    ///
    /// Returns the original non-recoverable error or a filesystem error while
    /// moving the malformed document to its timestamped backup path.
    pub fn quarantine_invalid_patch(&self) -> ControlledConfigResult<Option<PathBuf>> {
        let _write_lease = self.write_access.acquire();
        let _transaction = self.transaction.lock();
        let error = match self.load_unlocked() {
            Ok(_) => return Ok(None),
            Err(error) => error,
        };
        if !matches!(
            error,
            ControlledConfigError::Yaml(_)
                | ControlledConfigError::NotMapping
                | ControlledConfigError::TooLarge
        ) {
            return Err(error);
        }
        let source = self.patch_path();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let quarantine = self.root.join(format!(
            "override.invalid-{}-{timestamp}.yaml",
            std::process::id()
        ));
        fs::rename(&source, &quarantine)?;
        Ok(Some(quarantine))
    }

    async fn accept_runtime_payload(
        &self,
        client: &MihomoClient,
        payload: String,
    ) -> ControlledConfigResult<AcceptedRuntime> {
        self.accept_runtime_payload_for_session(client, payload)
            .await
            .map_err(|error| error.cause)
    }

    async fn accept_runtime_payload_for_session(
        &self,
        client: &MihomoClient,
        payload: String,
    ) -> Result<AcceptedRuntime, RuntimeMutationError> {
        let previous_payload = self.cached_runtime_payload()?;
        self.accept_runtime_payload_with_snapshot(client, payload, previous_payload)
            .await
    }

    async fn accept_runtime_payload_with_snapshot(
        &self,
        client: &MihomoClient,
        payload: String,
        previous_payload: Option<String>,
    ) -> Result<AcceptedRuntime, RuntimeMutationError> {
        let payload = client.normalize_config_payload(payload)?;
        let payload = self.apply_session_listener_fallbacks(&payload)?;
        let prepared = client.prepare_runtime_payload(payload.clone()).await?;
        let worker_lease = self.write_access.acquire();
        let store = self.with_write_lease(&worker_lease);
        let cache_payload = payload.clone();
        let cache = tokio::task::spawn_blocking(move || {
            let _write_lease = worker_lease;
            store.stage_runtime_payload(&cache_payload)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let runtime = match prepared.apply(true).await {
            Ok(runtime) => runtime,
            Err(error) => {
                let attempted = error.mutation_result_unknown();
                return match tokio::task::spawn_blocking(move || cache.rollback()).await {
                    Ok(Ok(())) => Err(ControlledConfigError::Profile(error)),
                    Ok(Err(rollback)) => Err(ControlledConfigError::Transaction(format!(
                        "Mihomo 拒绝配置：{error}；恢复启动缓存失败：{rollback}"
                    ))),
                    Err(task) => Err(ControlledConfigError::Task(format!(
                        "Mihomo 拒绝配置：{error}；缓存恢复任务异常结束：{task}"
                    ))),
                }
                .map_err(|cause| RuntimeMutationError { cause, attempted });
            }
        };
        Ok(AcceptedRuntime {
            cache,
            runtime,
            previous_payload,
        })
    }

    async fn accept_runtime_payload_with_restart_for_session(
        &self,
        process: Arc<MihomoProcess>,
        payload: String,
        cancelled: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<RuntimeCacheTransaction, RuntimeMutationError> {
        let write_lease = self
            .acquire_write_lease_for_paths(process.write_scopes())
            .await?;
        let leased_store = self.with_write_lease(&write_lease);
        let payload = normalize_runtime_payload(process.kind(), payload)?;
        let payload = self.apply_session_listener_fallbacks(&payload)?;
        let worker_lease = leased_store.write_access.acquire();
        let store = leased_store.with_write_lease(&worker_lease);
        let validator = process.config_validator().with_write_lease(&worker_lease);
        let cache = tokio::task::spawn_blocking(move || {
            let _write_lease = worker_lease;
            validator.validate_payload(&payload).map_err(|error| {
                ControlledConfigError::Profile(MihomoError::Process(error.to_string()))
            })?;
            store.stage_runtime_payload(&payload)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let attempted = cancelled
            .as_ref()
            .is_none_or(|flag| !flag.load(std::sync::atomic::Ordering::Acquire));
        if let Err(error) = process
            .restart_and_wait_until_with_lease(
                std::time::Duration::from_secs(20),
                cancelled.clone(),
                &write_lease,
            )
            .await
        {
            let rollback =
                rollback_cache_and_restart(cache, process, &write_lease, cancelled).await;
            return Err(RuntimeMutationError {
                attempted,
                cause: ControlledConfigError::Transaction(format!(
                    "新配置启动失败：{error}；上一配置恢复：{rollback}"
                )),
            });
        }
        Ok(cache)
    }

    fn prepare_update(
        &self,
        profile: &Path,
        patch: Value,
    ) -> ControlledConfigResult<ControlledConfigUpdate> {
        let _transaction = self.transaction.lock();
        let (expected_patch, current) = self.load_unlocked()?;
        let previous_payload = merge_profile_patch(profile, current.clone())?;
        let mut next = current;
        merge_yaml(&mut next, patch);
        require_mapping(&next)?;
        let next_patch = serde_yaml::to_string(&next)?.into_bytes();
        if next_patch.len() > MAX_PROFILE_BYTES {
            return Err(ControlledConfigError::TooLarge);
        }
        let next_payload = merge_profile_patch(profile, next)?;
        Ok(ControlledConfigUpdate {
            expected_patch,
            next_patch,
            previous_payload,
            next_payload,
        })
    }

    fn validate_candidate_listeners(
        &self,
        candidate: String,
        current_core_is_running: bool,
    ) -> ControlledConfigResult<String> {
        let candidate = self.apply_session_listener_fallbacks(&candidate)?;
        let current = self
            .cached_runtime_payload()?
            .unwrap_or_else(|| candidate.clone());
        validate_listener_change(&current, &candidate, current_core_is_running)
            .map_err(ControlledConfigError::ListenerFallback)?;
        Ok(candidate)
    }

    pub(crate) fn cached_runtime_payload(&self) -> ControlledConfigResult<Option<String>> {
        let bytes = match std::fs::read(self.runtime_path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(ControlledConfigError::Io(error)),
        };
        if bytes.len() > MAX_PROFILE_BYTES {
            return Err(ControlledConfigError::TooLarge);
        }
        String::from_utf8(bytes).map(Some).map_err(|error| {
            ControlledConfigError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })
    }

    pub(crate) async fn restore_exact_runtime_payload(
        &self,
        client: &MihomoClient,
        process: Option<Arc<MihomoProcess>>,
        payload: String,
        service_bundle: Option<Arc<crate::ServiceRuntimeBundle>>,
        authority: crate::core_session::RuntimeRestoreAuthority,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), RuntimeMutationError> {
        let client = client.pin_binding()?;
        let mut scopes = client.write_scopes();
        if let Some(process) = &process {
            scopes.extend(process.write_scopes());
        }
        let write_lease = self.acquire_write_lease_for_paths(scopes).await?;
        client.ensure_binding_current()?;
        let client = client.with_write_lease(&write_lease)?;
        let store = self.with_write_lease(&write_lease);
        let _mutation_guard = store.mutation_gate.lock().await;
        client.ensure_binding_current()?;
        // Validate before touching the cache, without remerging source files or fallbacks.
        let prepared = if process.is_none() {
            Some(
                client
                    .prepare_saved_runtime(payload.clone(), service_bundle, authority)
                    .await?,
            )
        } else {
            None
        };
        let worker_lease = store.write_access.acquire();
        let cache_store = store.with_write_lease(&worker_lease);
        let validator = process
            .as_ref()
            .map(|process| process.config_validator().with_write_lease(&worker_lease));
        let cache_payload = payload.clone();
        let cache = tokio::task::spawn_blocking(move || {
            let _write_lease = worker_lease;
            if let Some(validator) = validator {
                validator
                    .validate_payload(&cache_payload)
                    .map_err(|error| {
                        ControlledConfigError::Profile(MihomoError::Process(error.to_string()))
                    })?;
            }
            cache_store.stage_runtime_payload(&cache_payload)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let mut applied_runtime = None;
        client.ensure_binding_current()?;
        let (applied, attempted) = if let Some(process) = &process {
            let attempted = !cancelled.load(std::sync::atomic::Ordering::Acquire);
            (
                process
                    .restart_and_wait_until_with_lease(
                        std::time::Duration::from_secs(20),
                        Some(cancelled.clone()),
                        &write_lease,
                    )
                    .await,
                attempted,
            )
        } else {
            let applied = match prepared {
                Some(prepared) => prepared.apply(true).await.map(|runtime| {
                    applied_runtime = Some(runtime);
                }),
                None => Err(MihomoError::Process("Missing prepared runtime".into())),
            };
            let attempted = applied
                .as_ref()
                .err()
                .is_some_and(MihomoError::mutation_result_unknown);
            (applied, attempted)
        };
        if let Err(error) = applied {
            let cache_rollback = if let Some(process) = process {
                rollback_cache_and_restart(cache, process, &write_lease, Some(cancelled)).await
            } else {
                result_label(rollback_runtime_cache(cache).await)
            };
            return Err(RuntimeMutationError {
                attempted,
                cause: ControlledConfigError::Transaction(zenclash_i18n::text_with(
                    "backup.errors.exact_runtime_restore_failed",
                    &[("error", error.to_string()), ("cache", cache_rollback)],
                )),
            });
        }
        cache.commit();
        if let Some(runtime) = applied_runtime {
            runtime.commit().await.map_err(|cause| {
                RuntimeMutationError::attempted(ControlledConfigError::Profile(cause))
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[derive(Debug)]
struct ModePatchGate {
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    released: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

#[cfg(test)]
struct ModePatchRelease(Option<tokio::sync::oneshot::Sender<()>>);

#[cfg(test)]
impl Drop for ModePatchRelease {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            let _ = release.send(());
        }
    }
}

#[cfg(test)]
impl ModePatchGate {
    fn new() -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<()>,
        ModePatchRelease,
    ) {
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        (
            Arc::new(Self {
                entered: Mutex::new(Some(entered)),
                released: Mutex::new(Some(released)),
            }),
            waiting,
            ModePatchRelease(Some(release)),
        )
    }

    async fn wait(&self) {
        let released = self.released.lock().take().expect("mode gate awaited once");
        if let Some(entered) = self.entered.lock().take() {
            let _ = entered.send(());
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), released)
            .await
            .expect("mode test did not release its gate")
            .expect("mode test release disappeared");
    }
}

pub(crate) fn normalize_runtime_payload(
    kind: CoreKind,
    payload: String,
) -> ControlledConfigResult<String> {
    let mut document = serde_yaml::from_str::<Value>(&payload)?;
    let Some(root) = document.as_mapping_mut() else {
        return Err(ControlledConfigError::NotMapping);
    };
    if kind == CoreKind::Mihomo {
        if !insert_missing_mihomo_geox_urls(root) {
            return Ok(payload);
        }
        return Ok(serde_yaml::to_string(&document)?);
    }

    let dns_key = Value::String("dns".into());
    let Some(dns) = root.get_mut(&dns_key).and_then(Value::as_mapping_mut) else {
        return Ok(payload);
    };
    if !yaml_bool(dns, "enable") || has_nonempty_dns_value(dns, "nameserver") {
        return Ok(payload);
    }

    let defaults = Value::Sequence(
        MEOW_DEFAULT_NAMESERVERS
            .into_iter()
            .map(|server| Value::String(server.into()))
            .collect(),
    );
    dns.insert(Value::String("nameserver".into()), defaults.clone());
    if !has_nonempty_dns_value(dns, "default-nameserver") {
        dns.insert(Value::String("default-nameserver".into()), defaults);
    }
    let normalized = serde_yaml::to_string(&document)?;
    if normalized.len() > MAX_PROFILE_BYTES {
        return Err(ControlledConfigError::TooLarge);
    }
    Ok(normalized)
}

fn insert_missing_mihomo_geox_urls(root: &mut serde_yaml::Mapping) -> bool {
    let geox_key = Value::String("geox-url".into());
    let geox_urls = match root.get_mut(&geox_key) {
        Some(Value::Mapping(geox_urls)) => geox_urls,
        Some(value @ Value::Null) => {
            *value = Value::Mapping(serde_yaml::Mapping::new());
            value.as_mapping_mut().expect("mapping inserted")
        }
        Some(_) => return false,
        None => {
            root.insert(geox_key.clone(), Value::Mapping(serde_yaml::Mapping::new()));
            root.get_mut(&geox_key)
                .and_then(Value::as_mapping_mut)
                .expect("mapping inserted")
        }
    };
    let mut changed = false;
    for (name, url) in MIHOMO_GEOX_URLS {
        let key = Value::String(name.into());
        if !geox_urls.contains_key(&key) {
            geox_urls.insert(key, Value::String(url.into()));
            changed = true;
        }
    }
    changed
}

fn yaml_bool(mapping: &serde_yaml::Mapping, key: &str) -> bool {
    mapping
        .get(Value::String(key.into()))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn has_nonempty_dns_value(dns: &serde_yaml::Mapping, key: &str) -> bool {
    dns.get(Value::String(key.into()))
        .is_some_and(yaml_value_is_nonempty)
}

fn yaml_value_is_nonempty(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(value) => !value.trim().is_empty(),
        Value::Sequence(values) => !values.is_empty(),
        _ => true,
    }
}

async fn rollback_runtime_cache(cache: RuntimeCacheTransaction) -> ControlledConfigResult<()> {
    tokio::task::spawn_blocking(move || cache.rollback())
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
}

async fn rollback_cache_and_restart(
    cache: RuntimeCacheTransaction,
    process: Arc<MihomoProcess>,
    write_lease: &DataWriteLease,
    cancelled: Option<Arc<std::sync::atomic::AtomicBool>>,
) -> String {
    let cache_rollback = tokio::task::spawn_blocking(move || cache.rollback())
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))
        .and_then(|result| result);
    if let Err(error) = cache_rollback {
        return format!("失败（缓存恢复失败：{error}）");
    }
    result_label(
        process
            .restart_and_wait_until_with_lease(
                std::time::Duration::from_secs(20),
                cancelled,
                write_lease,
            )
            .await,
    )
}

fn result_label<T, E: std::fmt::Display>(result: Result<T, E>) -> String {
    result.map_or_else(|error| format!("失败（{error}）"), |_| "成功".into())
}

pub(crate) fn service_partial_patch(patch: &serde_json::Value) -> bool {
    patch.as_object().is_some_and(|object| {
        !object.is_empty()
            && object.iter().all(|(key, value)| match key.as_str() {
                "mode" | "log-level" | "port" | "socks-port" | "mixed-port" | "redir-port"
                | "tproxy-port" | "ipv6" | "allow-lan" | "tcp-concurrent" | "bind-address"
                | "interface-name" | "find-process-mode" => true,
                "tun" => value.as_object().is_some_and(|tun| {
                    !tun.is_empty()
                        && tun.iter().all(|(key, value)| match key.as_str() {
                            "mtu" => value
                                .as_u64()
                                .is_some_and(|mtu| (576..=65535).contains(&mtu)),
                            "enable"
                            | "stack"
                            | "device"
                            | "auto-route"
                            | "auto-detect-interface"
                            | "auto-redirect"
                            | "strict-route"
                            | "gso"
                            | "dns-hijack" => true,
                            _ => false,
                        })
                }),
                _ => false,
            })
    })
}

pub(crate) fn merge_held_delta(
    payload: &str,
    patch: &serde_json::Value,
) -> ControlledConfigResult<String> {
    let mut value: Value = serde_yaml::from_str(payload)?;
    merge_yaml(&mut value, serde_yaml::to_value(patch)?);
    let payload = serde_yaml::to_string(&value)?;
    if payload.len() > MAX_PROFILE_BYTES {
        return Err(ControlledConfigError::TooLarge);
    }
    Ok(payload)
}

fn contains_delta(actual: &serde_json::Value, patch: &serde_json::Value) -> bool {
    match patch {
        serde_json::Value::Object(object) => actual.as_object().is_some_and(|actual| {
            object.iter().all(|(key, value)| {
                actual
                    .get(key)
                    .is_some_and(|actual| contains_delta(actual, value))
            })
        }),
        _ => actual == patch,
    }
}
