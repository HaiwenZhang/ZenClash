//! Partial mode and service updates preserve accepted configuration and effective deltas.

use super::*;

impl ControlledConfigStore {
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

    pub(super) async fn apply_partial_update_for_session(
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
        // Persist the effective delta too so implicit TUN enable survives later
        // full applications. Generated defaults belong only to runtime YAML.
        update.next_patch = merge_held_delta(
            std::str::from_utf8(&update.next_patch)
                .map_err(|_| ControlledConfigError::NotMapping)?,
            &delta,
        )?
        .into_bytes();
        let next_runtime =
            normalize_partial_tun_payload(merge_held_delta(&previous_payload, &delta)?)?;
        let worker_lease = self.write_access.acquire();
        let worker = self.with_write_lease(&worker_lease);
        let cache = tokio::task::spawn_blocking(move || {
            let _lease = worker_lease;
            worker.stage_runtime_payload(&next_runtime)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let (runtime, previous_mode) = if let Some(prepared) = prepared {
            // The reference client forces reloads when changing TUN configuration.
            let force_reload = delta.get("tun").is_some();
            match prepared.apply(force_reload).await {
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
}

#[cfg(test)]
#[derive(Debug)]
pub(super) struct ModePatchGate {
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    released: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

#[cfg(test)]
pub(super) struct ModePatchRelease(Option<tokio::sync::oneshot::Sender<()>>);

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
    pub(super) fn new() -> (
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

pub(super) fn contains_delta(actual: &serde_json::Value, patch: &serde_json::Value) -> bool {
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
