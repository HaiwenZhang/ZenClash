//! Input-read state; baseline profile identity stays with the retained controls.

use serde_json::Value;
use std::path::{Path, PathBuf};

use super::ConfigInputsTaskToken;

pub(super) struct ConfigInputsReadState {
    effective: Value,
    loaded_profile: Option<PathBuf>,
    failed_profile: Option<PathBuf>,
    generation: u64,
    loading: bool,
}

impl ConfigInputsReadState {
    pub(super) fn new(effective: Value) -> Self {
        Self {
            effective,
            loaded_profile: None,
            failed_profile: None,
            generation: 0,
            loading: false,
        }
    }

    pub(super) fn effective(&self) -> &Value {
        &self.effective
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn is_loading(&self) -> bool {
        self.loading
    }

    pub(super) fn begin(&mut self, profile: Option<&Path>) -> Option<ConfigInputsTaskToken> {
        if self.loaded_profile.as_deref() == profile
            || (profile.is_some() && self.failed_profile.as_deref() == profile)
            || self.loading
        {
            return None;
        }
        let Some(profile) = profile else {
            self.loaded_profile = None;
            self.failed_profile = None;
            return None;
        };
        self.generation = self.generation.wrapping_add(1);
        self.loading = true;
        Some(ConfigInputsTaskToken {
            profile: profile.to_path_buf(),
            generation: self.generation,
        })
    }

    pub(super) fn finish(&mut self, token: &ConfigInputsTaskToken, profile: Option<&Path>) -> bool {
        let current = token.is_current(profile, self.generation);
        self.loading = false;
        current
    }

    pub(super) fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.loaded_profile = None;
        self.failed_profile = None;
    }

    pub(super) fn accept(&mut self, effective: Value, profile: PathBuf) {
        self.effective = effective;
        self.loaded_profile = Some(profile);
        self.failed_profile = None;
    }

    pub(super) fn record_failure(&mut self, profile: PathBuf) {
        self.failed_profile = Some(profile);
    }

    pub(super) fn begin_bootstrap(&mut self) {
        self.loading = true;
    }

    pub(super) fn finish_bootstrap(&mut self) {
        self.loading = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_profile_read_is_cached_until_invalidated_or_switched() {
        let mut state = ConfigInputsReadState::new(Value::Null);
        let broken = Path::new("/tmp/broken.yaml");
        let other = Path::new("/tmp/other.yaml");

        let token = state.begin(Some(broken)).expect("first read starts");
        assert!(state.finish(&token, Some(broken)));
        state.record_failure(token.profile);

        // Repeated 1Hz refreshes for the same broken profile must not re-parse it.
        assert!(state.begin(Some(broken)).is_none());

        // Explicit invalidation or switching profiles allows a new read.
        state.invalidate();
        let retry = state
            .begin(Some(broken))
            .expect("invalidation allows retry");
        assert!(state.finish(&retry, Some(broken)));
        state.record_failure(retry.profile);
        assert!(state.begin(Some(broken)).is_none());

        assert!(state.begin(Some(other)).is_some());
    }
}
