//! Persistent, non-destructive configuration overrides for active profiles.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;
use serde_yaml::Value;
use thiserror::Error;

use crate::{
    listener_fallback::{
        apply_session_fallbacks, resolve_conflicts, validate_listener_change,
        SessionListenerFallback,
    },
    profile::{merge_payload_overrides, merge_profile_patch, merge_yaml},
    profiles::{atomic_write, read_profile_bytes, MAX_PROFILE_BYTES},
    CoreKind, MihomoClient, MihomoError, MihomoProcess,
};

const MEOW_DEFAULT_NAMESERVERS: [&str; 2] = ["223.5.5.5", "1.1.1.1"];

mod storage;

#[cfg(test)]
mod tests;

use storage::{default_data_dir, require_mapping, RuntimeCacheTransaction};

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
    root: PathBuf,
    transaction: Arc<Mutex<()>>,
    mutation_gate: Arc<tokio::sync::Mutex<()>>,
    session_listener_fallbacks: Arc<Mutex<BTreeMap<String, SessionListenerFallback>>>,
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
        Self {
            root: root.into(),
            transaction: Arc::new(Mutex::new(())),
            mutation_gate: Arc::new(tokio::sync::Mutex::new(())),
            session_listener_fallbacks: Arc::new(Mutex::new(BTreeMap::new())),
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
        Ok(merge_profile_patch(profile.as_ref(), patch)?)
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
        Ok(merge_payload_overrides(&payload, overrides)?)
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
        let session = self.session_listener_fallbacks.lock();
        let payload = apply_session_fallbacks(payload, &session)
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
    /// Both the accepted payload and any rollback payload contain the same
    /// explicit override layer, so settings changes cannot silently remove it.
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
        let _mutation_guard = self.mutation_gate.lock().await;
        let prepare_store = self.clone();
        let profile = profile.as_ref().to_path_buf();
        let patch = patch.clone();
        let update =
            tokio::task::spawn_blocking(move || prepare_store.prepare_json_update(profile, &patch))
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
        let previous_runtime = self.apply_session_listener_fallbacks(&previous_runtime)?;
        let next_runtime = self.apply_session_listener_fallbacks(&next_runtime)?;
        validate_listener_change(&previous_runtime, &next_runtime, true)
            .map_err(ControlledConfigError::ListenerFallback)?;
        let cache = self.accept_runtime_payload(client, next_runtime).await?;
        let commit_store = self.clone();
        let commit_update = update.clone();
        let commit = tokio::task::spawn_blocking(move || commit_store.commit(&commit_update))
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))?;
        if let Err(error) = commit {
            let cache_rollback = tokio::task::spawn_blocking(move || cache.rollback())
                .await
                .map_err(|task| ControlledConfigError::Task(task.to_string()))
                .and_then(|result| result);
            let runtime_rollback = client.reload_payload(previous_runtime, true).await;
            return match (cache_rollback, runtime_rollback) {
                (Ok(()), Ok(())) => Err(ControlledConfigError::Transaction(format!(
                    "保存失败，启动缓存与 Mihomo 均已恢复上一版本：{error}"
                ))),
                (cache, runtime) => Err(ControlledConfigError::Transaction(format!(
                    "保存失败：{error}；缓存恢复：{}；Mihomo 恢复：{}",
                    result_label(cache),
                    result_label(runtime)
                ))),
            };
        }
        cache.commit();
        Ok(())
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
        let mode = mode.trim().to_ascii_lowercase();
        if !matches!(mode.as_str(), "rule" | "global" | "direct") {
            return Err(ControlledConfigError::Profile(MihomoError::InvalidInput(
                format!("不支持的出站模式：{mode}"),
            )));
        }

        let _mutation_guard = self.mutation_gate.lock().await;
        let prepare_store = self.clone();
        let profile = profile.as_ref().to_path_buf();
        let patch = serde_json::json!({"mode": mode.clone()});
        let update =
            tokio::task::spawn_blocking(move || prepare_store.prepare_json_update(profile, &patch))
                .await
                .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let next_base = update.next_payload().to_owned();
        let next_runtime =
            tokio::task::spawn_blocking(move || merge_payload_overrides(&next_base, &overrides))
                .await
                .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let next_runtime = self.apply_session_listener_fallbacks(&next_runtime)?;
        let cache_store = self.clone();
        let cache =
            tokio::task::spawn_blocking(move || cache_store.stage_runtime_payload(&next_runtime))
                .await
                .map_err(|error| ControlledConfigError::Task(error.to_string()))??;

        let previous_mode = match client.runtime_config().await {
            Ok(config) => config.mode,
            Err(error) => {
                rollback_runtime_cache(cache).await?;
                return Err(ControlledConfigError::Profile(error));
            }
        };
        if let Err(error) = client.set_mode(&mode).await {
            let cache_rollback = rollback_runtime_cache(cache).await;
            let runtime_rollback = client.set_mode(&previous_mode).await;
            return match (cache_rollback, runtime_rollback) {
                (Ok(()), Ok(())) => Err(ControlledConfigError::Profile(error)),
                (cache, runtime) => Err(ControlledConfigError::Transaction(format!(
                    "切换模式失败：{error}；缓存恢复：{}；Mihomo 恢复：{}",
                    result_label(cache),
                    result_label(runtime)
                ))),
            };
        }

        let commit_store = self.clone();
        let commit_update = update.clone();
        let commit = tokio::task::spawn_blocking(move || commit_store.commit(&commit_update))
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))?;
        if let Err(error) = commit {
            let cache_rollback = rollback_runtime_cache(cache).await;
            let runtime_rollback = client.set_mode(&previous_mode).await;
            return match (cache_rollback, runtime_rollback) {
                (Ok(()), Ok(())) => Err(ControlledConfigError::Transaction(format!(
                    "保存模式失败，启动缓存与 Mihomo 均已恢复上一模式：{error}"
                ))),
                (cache, runtime) => Err(ControlledConfigError::Transaction(format!(
                    "保存模式失败：{error}；缓存恢复：{}；Mihomo 恢复：{}",
                    result_label(cache),
                    result_label(runtime)
                ))),
            };
        }
        cache.commit();
        Ok(())
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
        let _mutation_guard = self.mutation_gate.lock().await;
        let prepare_store = self.clone();
        let profile = profile.as_ref().to_path_buf();
        let patch = patch.clone();
        let update =
            tokio::task::spawn_blocking(move || prepare_store.prepare_json_update(profile, &patch))
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
        let next_runtime = self.apply_session_listener_fallbacks(&next_runtime)?;
        let previous_runtime = self.apply_session_listener_fallbacks(&previous_runtime)?;
        validate_listener_change(&previous_runtime, &next_runtime, process.is_running())
            .map_err(ControlledConfigError::ListenerFallback)?;
        let cache = self
            .accept_runtime_payload_with_restart(process.clone(), next_runtime)
            .await?;
        let commit_store = self.clone();
        let commit_update = update.clone();
        let commit = tokio::task::spawn_blocking(move || commit_store.commit(&commit_update))
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))?;
        if let Err(error) = commit {
            let rollback = rollback_cache_and_restart(cache, process).await;
            return Err(ControlledConfigError::Transaction(format!(
                "保存失败：{error}；运行内核恢复：{rollback}"
            )));
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
        let _mutation_guard = self.mutation_gate.lock().await;
        let store = self.clone();
        let profile = profile.as_ref().to_path_buf();
        let payload = tokio::task::spawn_blocking(move || store.effective_payload(profile))
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let payload = self.validate_candidate_listeners(payload, true)?;
        self.accept_runtime_payload(client, payload).await?.commit();
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
        let _mutation_guard = self.mutation_gate.lock().await;
        let store = self.clone();
        let profile = profile.as_ref().to_path_buf();
        let payload = tokio::task::spawn_blocking(move || {
            store.effective_with_overrides(profile, &overrides)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let payload = self.validate_candidate_listeners(payload, true)?;
        self.accept_runtime_payload(client, payload).await?.commit();
        Ok(())
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
        let _mutation_guard = self.mutation_gate.lock().await;
        let store = self.clone();
        let profile = profile.as_ref().to_path_buf();
        let payload = tokio::task::spawn_blocking(move || {
            store.effective_with_overrides(profile, &overrides)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let payload = normalize_runtime_payload(process.kind(), payload)?;
        let payload = self.validate_candidate_listeners(payload, process.is_running())?;
        self.accept_runtime_payload_with_restart(process, payload)
            .await?
            .commit();
        Ok(())
    }

    /// Persists a prepared update if the controlled layer has not changed.
    ///
    /// # Errors
    ///
    /// Returns an error for concurrent modification or an atomic write failure.
    pub fn commit(&self, update: &ControlledConfigUpdate) -> ControlledConfigResult<()> {
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
    ) -> ControlledConfigResult<RuntimeCacheTransaction> {
        let payload = self.apply_session_listener_fallbacks(&payload)?;
        let store = self.clone();
        let cache_payload = payload.clone();
        let cache =
            tokio::task::spawn_blocking(move || store.stage_runtime_payload(&cache_payload))
                .await
                .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        if let Err(error) = client.reload_payload(payload, true).await {
            return match tokio::task::spawn_blocking(move || cache.rollback()).await {
                Ok(Ok(())) => Err(ControlledConfigError::Profile(error)),
                Ok(Err(rollback)) => Err(ControlledConfigError::Transaction(format!(
                    "Mihomo 拒绝配置：{error}；恢复启动缓存失败：{rollback}"
                ))),
                Err(task) => Err(ControlledConfigError::Task(format!(
                    "Mihomo 拒绝配置：{error}；缓存恢复任务异常结束：{task}"
                ))),
            };
        }
        Ok(cache)
    }

    async fn accept_runtime_payload_with_restart(
        &self,
        process: Arc<MihomoProcess>,
        payload: String,
    ) -> ControlledConfigResult<RuntimeCacheTransaction> {
        let payload = normalize_runtime_payload(process.kind(), payload)?;
        let payload = self.apply_session_listener_fallbacks(&payload)?;
        let store = self.clone();
        let cache = tokio::task::spawn_blocking(move || store.stage_runtime_payload(&payload))
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        if let Err(error) = restart_and_wait(process.clone()).await {
            let rollback = rollback_cache_and_restart(cache, process).await;
            return Err(ControlledConfigError::Transaction(format!(
                "新配置启动失败：{error}；上一配置恢复：{rollback}"
            )));
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

    fn cached_runtime_payload(&self) -> ControlledConfigResult<Option<String>> {
        let bytes = match std::fs::read(self.runtime_path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(ControlledConfigError::Io(error)),
        };
        if bytes.len() > MAX_PROFILE_BYTES {
            return Err(ControlledConfigError::TooLarge);
        }
        String::from_utf8(bytes).map(Some).map_err(|error| {
            ControlledConfigError::Transaction(format!("运行缓存不是 UTF-8：{error}"))
        })
    }
}

fn normalize_runtime_payload(kind: CoreKind, payload: String) -> ControlledConfigResult<String> {
    if kind != CoreKind::Meow {
        return Ok(payload);
    }

    let mut document = serde_yaml::from_str::<Value>(&payload)?;
    let Some(root) = document.as_mapping_mut() else {
        return Err(ControlledConfigError::NotMapping);
    };
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

async fn restart_and_wait(process: Arc<MihomoProcess>) -> ControlledConfigResult<()> {
    let restart_process = process.clone();
    tokio::task::spawn_blocking(move || restart_process.restart())
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
        .map_err(ControlledConfigError::Profile)?;
    process
        .wait_until_ready(Duration::from_secs(20))
        .await
        .map_err(ControlledConfigError::Profile)
}

async fn rollback_runtime_cache(cache: RuntimeCacheTransaction) -> ControlledConfigResult<()> {
    tokio::task::spawn_blocking(move || cache.rollback())
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
}

async fn rollback_cache_and_restart(
    cache: RuntimeCacheTransaction,
    process: Arc<MihomoProcess>,
) -> String {
    let cache_rollback = tokio::task::spawn_blocking(move || cache.rollback())
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))
        .and_then(|result| result);
    if let Err(error) = cache_rollback {
        return format!("失败（缓存恢复失败：{error}）");
    }
    result_label(restart_and_wait(process).await)
}

fn result_label<T, E: std::fmt::Display>(result: Result<T, E>) -> String {
    result.map_or_else(|error| format!("失败（{error}）"), |_| "成功".into())
}
