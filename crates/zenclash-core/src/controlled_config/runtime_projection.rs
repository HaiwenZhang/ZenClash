//! Runtime-only TUN and listener projections; persistent user intent stays unchanged.

use super::*;

#[derive(Clone, Debug)]
pub(super) enum TunProjection {
    PreserveIntent,
    Disabled,
    FollowRuntimeBinding(std::sync::Weak<crate::client::ControllerBinding>),
}

impl TunProjection {
    fn with_runtime_binding(&self, binding: &Arc<crate::client::ControllerBinding>) -> Self {
        match self {
            Self::Disabled => Self::Disabled,
            Self::PreserveIntent | Self::FollowRuntimeBinding(_) => {
                Self::FollowRuntimeBinding(Arc::downgrade(binding))
            }
        }
    }

    pub(super) fn project_payload(&self, payload: String) -> ControlledConfigResult<String> {
        let disable_tun = match self {
            Self::PreserveIntent => false,
            Self::Disabled => true,
            Self::FollowRuntimeBinding(binding) => binding.upgrade().is_some_and(|binding| {
                let runtime = binding.descriptor();
                runtime.kind() == CoreKind::Mihomo
                    && runtime.backend() == crate::CoreRuntimeBackend::Local
                    && !crate::current_process_elevated()
            }),
        };
        if !disable_tun {
            return Ok(payload);
        }
        Ok(crate::profile::merge_payload_patch(
            &payload,
            serde_yaml::from_str("tun: {enable: false}")?,
        )?)
    }
}

impl ControlledConfigStore {
    /// Creates a startup-only projection for an unprivileged ordinary child.
    /// Saved TUN intent is retained for a later privileged service handover.
    #[must_use]
    pub fn without_startup_tun(&self) -> Self {
        Self {
            tun_projection: TunProjection::Disabled,
            ..self.clone()
        }
    }

    /// Projects ordinary Mihomo updates with TUN off until a privileged owner is published.
    /// A weak binding observation prevents configuration stores retaining old owners.
    #[must_use]
    pub fn with_runtime_tun_policy(&self, client: &MihomoClient) -> Self {
        Self {
            tun_projection: self.tun_projection.with_runtime_binding(&client.binding),
            ..self.clone()
        }
    }

    pub(crate) fn for_service_runtime(&self) -> Self {
        Self {
            tun_projection: TunProjection::PreserveIntent,
            ..self.clone()
        }
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
        let _write_lease = self.write_access.acquire()?;
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

    pub(super) fn apply_session_listener_fallbacks(
        &self,
        payload: &str,
    ) -> ControlledConfigResult<String> {
        let payload = self.tun_projection.project_payload(payload.to_owned())?;
        let session = self.session_listener_fallbacks.lock();
        let payload = apply_session_fallbacks(&payload, &session)
            .map_err(ControlledConfigError::ListenerFallback)?;
        if payload.len() > MAX_PROFILE_BYTES {
            return Err(ControlledConfigError::TooLarge);
        }
        Ok(payload)
    }

    pub(super) fn validate_candidate_listeners(
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
    /// Materializes effective YAML for managed-core startup.
    ///
    /// The returned file is a generated cache. The source profile and
    /// controlled override remain authoritative and are never overwritten.
    ///
    /// # Errors
    ///
    /// Returns an error when merging or the atomic cache write fails.
    pub fn materialize(&self, profile: impl AsRef<Path>) -> ControlledConfigResult<PathBuf> {
        let _write_lease = self.write_access.acquire()?;
        let payload = self.effective_payload(profile)?;
        self.write_projected_runtime_payload(&payload)
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
        let _write_lease = self.write_access.acquire()?;
        let payload = self.effective_with_overrides(profile, overrides)?;
        self.write_projected_runtime_payload(&payload)
    }

    /// Materializes startup YAML with compatibility defaults for one core.
    ///
    /// Adds runtime defaults without rewriting source profiles or persistent overrides.
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
        let _write_lease = self.write_access.acquire()?;
        let payload = self.effective_with_overrides(profile, overrides)?;
        let payload = normalize_runtime_payload(kind, payload)?;
        self.write_projected_runtime_payload(&payload)
    }

    fn write_projected_runtime_payload(&self, payload: &str) -> ControlledConfigResult<PathBuf> {
        let path = self.runtime_path();
        let payload = self.apply_session_listener_fallbacks(payload)?;
        atomic_write(&path, payload.as_bytes())?;
        Ok(path)
    }
}
