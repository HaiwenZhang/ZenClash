//! Profile and generation identity for configuration-input reads.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::pages::runtime) struct ConfigInputsTaskToken {
    pub(in crate::pages::runtime) profile: PathBuf,
    pub(in crate::pages::runtime) generation: u64,
}

impl ConfigInputsTaskToken {
    pub(in crate::pages::runtime) fn is_current(
        &self,
        profile: Option<&Path>,
        generation: u64,
    ) -> bool {
        profile == Some(self.profile.as_path()) && self.generation == generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_inputs_task_rejects_result_for_replaced_profile() {
        let token = ConfigInputsTaskToken {
            profile: PathBuf::from("profiles/old.yaml"),
            generation: 4,
        };

        assert!(!token.is_current(Some(Path::new("profiles/new.yaml")), 4));
    }
    #[test]
    fn config_inputs_task_rejects_result_after_same_profile_is_invalidated() {
        let token = ConfigInputsTaskToken {
            profile: PathBuf::from("profiles/active.yaml"),
            generation: 4,
        };

        assert!(!token.is_current(Some(Path::new("profiles/active.yaml")), 5));
        assert!(token.is_current(Some(Path::new("profiles/active.yaml")), 4));
    }
}
