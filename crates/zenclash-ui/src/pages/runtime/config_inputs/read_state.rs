//! Input-read state; baseline profile identity stays with the retained controls.

use serde_json::Value;
use std::path::{Path, PathBuf};

use super::ConfigInputsTaskToken;

pub(super) struct ConfigInputsReadState {
    effective: Value,
    loaded_profile: Option<PathBuf>,
    generation: u64,
    loading: bool,
}

impl ConfigInputsReadState {
    pub(super) fn new(effective: Value) -> Self {
        Self {
            effective,
            loaded_profile: None,
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
        if self.loaded_profile.as_deref() == profile || self.loading {
            return None;
        }
        let Some(profile) = profile else {
            self.loaded_profile = None;
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
    }

    pub(super) fn accept(&mut self, effective: Value, profile: PathBuf) {
        self.effective = effective;
        self.loaded_profile = Some(profile);
    }

    pub(super) fn begin_bootstrap(&mut self) {
        self.loading = true;
    }

    pub(super) fn finish_bootstrap(&mut self) {
        self.loading = false;
    }
}
