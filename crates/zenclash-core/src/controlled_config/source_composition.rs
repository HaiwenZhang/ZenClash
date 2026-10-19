//! Profile and controlled-layer composition, ordered overrides and candidate revisions.

use super::*;

impl ControlledConfigStore {
    /// Builds effective YAML for `profile` without modifying its source file.
    ///
    /// # Errors
    ///
    /// Returns an error when the controlled layer or source profile cannot be
    /// read, parsed, merged, validated, or serialized.
    pub fn effective_payload(&self, profile: impl AsRef<Path>) -> ControlledConfigResult<String> {
        let patch = self.load()?;
        self.tun_projection
            .project_payload(merge_profile_patch(profile.as_ref(), patch)?)
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
        self.tun_projection
            .project_payload(merge_payload_overrides(&payload, overrides)?)
    }

    pub(super) fn effective_source_with_overrides(
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
        self.tun_projection
            .project_payload(merge_payload_overrides(&payload, overrides)?)
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

    /// Prepares a recursive JSON patch without persisting it.
    /// `hosts` and `dns.nameserver-policy` are complete form values: when supplied,
    /// their mappings replace the saved and source mappings, including deleted keys.
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

    pub(super) fn prepare_partial_update(
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

    pub(super) fn prepare_partial_payload_update(
        expected_patch: Option<Vec<u8>>,
        mut current: Value,
        previous_payload: String,
        patch: &serde_json::Value,
        overrides: &[PathBuf],
    ) -> ControlledConfigResult<ControlledConfigUpdate> {
        let next_payload =
            normalize_partial_tun_payload(merge_held_delta(&previous_payload, patch)?)?;
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

    pub(super) fn prepare_update(
        &self,
        profile: &Path,
        patch: Value,
    ) -> ControlledConfigResult<ControlledConfigUpdate> {
        let _transaction = self.transaction.lock();
        let (expected_patch, current) = self.load_unlocked()?;
        let source = crate::profile::ProfileSourceDocument::read(profile)?;
        let previous_payload = source.patched_payload(current.clone())?;
        let mut next = current;
        merge_controlled_yaml(&mut next, patch);
        require_mapping(&next)?;
        let next_patch = serde_yaml::to_string(&next)?.into_bytes();
        if next_patch.len() > MAX_PROFILE_BYTES {
            return Err(ControlledConfigError::TooLarge);
        }
        let next_payload = source.patched_payload(next)?;
        Ok(ControlledConfigUpdate {
            expected_patch,
            next_patch,
            previous_payload,
            next_payload,
        })
    }
}
